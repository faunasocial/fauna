"""End-to-end tests for `fauna-mail-bridge` running in its MTA role.

The bridge spawns alongside a real `fauna-nest` via the `mail_bridge_mta`
fixture (conftest.py): admin-enrolled MTA service-user keypair,
admin-claimed primary local domain, bridge process bound to ephemeral
loopback ports. These tests drive SMTP wire bytes through the bridge's
real port-25 (and, in E.3, 465/587) listeners and assert the round-trip
all the way to nest's stored mail.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real wire end-to-end.
- `independent`: the bridge is a server-side concern; no per-app
  driver participates, so the suite runs once per machine regardless
  of `--client`. Mirrors `tests/api/`'s independent shape.
"""

import base64
import datetime
import hashlib
import json
import secrets
import socket
import sqlite3
import time
import urllib.request

import pytest

from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _find_tagged,
    _poke_outbound_bridge,
    _smtp_auth_plain,
    _wait_for_tagged,
    dkim_signature_tag,
)
from helpers.waiting import wait_until
from helpers.mail_aliases import add_exact_alias

pytestmark = pytest.mark.tier_3


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _register_harness_seal_key(nest, actor_id: bytes) -> None:
    """Register a recipient seal key the harness holds, for an actor that is
    this module's OWN (never a shared one — `helpers/shared_identity.py` refuses
    that). Nothing here opens what gets sealed to it; the tests read the SMTP
    verdicts and the nest's acceptance."""
    from helpers.recipient_seal_key import provision_recipient_seal_key

    with _admin_ws(nest) as ws:
        provision_recipient_seal_key(ws, actor_id)


@pytest.fixture(scope="module")
def role_address_venue(request, nest_mode, tmp_path_factory):
    """A claimed mail nest of this module's own, for the journeys whose mail is
    delivered to the deployment ADMIN — role addresses (`postmaster@`, …) route
    there (smtp-server.md § abuse@/postmaster@ routing).

    Not the session `mail_bridge_mta`: that nest's admin is shared with every
    `admin_app` test, and a seal key the harness writes for it is one the
    admin's app never holds, so role mail sealed to it could never open there
    (e2e-conventions.md convention 10, *A sixth form*). This venue's admin is
    signed in by no app: the harness is its only client and holds the key it
    registers, as it does for any actor a fixture mints.
    """
    from conftest import _start_mail_venue

    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "role-address-venue"
    )
    try:
        _register_harness_seal_key(handle.nest, handle.nest["admin"]["actor_id_bytes"])
        _open_mail_gate_and_wait_for_the_mta(handle)
        yield handle
    finally:
        cleanup()


def _mx_accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0):
            return True
    except OSError:
        return False


def _open_mail_gate_and_wait_for_the_mta(handle, budget_s: float = 120.0) -> None:
    """The admin opens the deployment's mail gate; return once the MTA serves.

    A venue starts with the gate shut, and the admin's act has TWO possible
    outcomes depending on a race this setup cannot see. The MTA is spawned
    without waiting to serve, and its first config fetch may land after the
    gate opens: it then serves at once and never exits. Or it has already
    idled: then it exits for rebind, and the venue's `rebind_after_enable`
    plays the supervisor. Either outcome is observable through the venue's own
    methods (the listener accepting, or `assert_mta_running` failing), so the
    wait ends on whichever state the MTA reaches. Waiting only for the exit
    timed out against an MTA that was already serving.
    """
    handle.admin_opens_mail_gate()

    def _state():
        if _mx_accepts(handle.mx_port):
            return "serving"
        try:
            handle.assert_mta_running()
        except AssertionError:
            return "exited"
        return None

    state = wait_until(
        _state,
        budget_s,
        interval=0.5,
        diagnose=lambda: f"the MTA neither served nor exited for rebind; {handle.bridge_log_hint('mta')}",
    )
    if state == "exited":
        handle.rebind_after_enable(mda=False)


@pytest.mark.feature("mail-server")
def test_inbound_mx_round_trip(mail_bridge_mta):
    """SMTP MAIL/RCPT/DATA against port 25 lands as inbound mail in nest.

    E.2: end-to-end inbound MX exercise of fauna.bridges.validate_recipient
    + fauna.bridges.ingest_inbound_mail through the real bridge binary
    against a real fauna-nest. Port 25 is plaintext (no STARTTLS); no
    submission auth involved.

    A single SMTP transaction completes: the fixture's `put_spam_policy`
    override clears DNSBL, disables greylist, and sets
    `fcrdns_mode=off`, so the first RCPT is accepted (no first-seen
    greylist tempfail) and `Backend.NewSession` does no resolver lookup
    on the loopback peer. Before finding #4 this test paid a 60 s
    greylist hold-down across two connections and bound to IPv6 to dodge
    the DNSBL response for 127.0.0.x; on the default `fcrdns_mode`
    `NewSession`'s connection-time PTR lookup on the loopback peer could
    also stall. Disabling the DNS-dependent gates via the production
    admin override removes all of that without dropping the test below
    tier_3 (real nest, real bridge, real wire).

    Reply reading goes through `_SmtpConn`, which keeps a persistent
    receive buffer: go-smtp flushes the whole multi-line EHLO capability
    list as one TCP segment, so a per-line reader that dropped everything
    past the first CRLF would hang waiting for lines the server had
    already sent (a TCP-segmentation-luck flake the previous helpers had).

    Verification: the bridge ACKs DATA `.` with 250 only when the full
    WS-RPC ingest path succeeded all the way to nest's mail_records
    insert. A 250 OK on `.` is the round-trip signal.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_local = handle.recipient_local_part
    recipient_addr = f"{recipient_local}@{domain}"

    body_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {recipient_addr}",
        "Subject: E.2 inbound MX round-trip",
        f"Message-ID: <e2-{int(time.time() * 1000)}@external.test>",
        "Date: Thu, 16 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Hello from the e2e inbound MX round-trip test.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    # Port 25 is InboundTLSMode=required (smtp-server.md § TLS posture), so
    # STARTTLS first — a cleartext MAIL FROM 530s. (Pre-T1.1 this test went
    # cleartext; updated alongside the content-scan e2e to track the code.)
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        # End-of-data marker: a line containing only ".".
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-server")
def test_inbound_role_address_postmaster_routes_to_admin(role_address_venue):
    """`postmaster@<local-domain>` with NO postmaster alias is accepted at
    RCPT TO (never 550) and round-trips to the admin's mailbox.

    T2.5 T1 — smtp-server.md § abuse@/postmaster@ role-address routing:
    RFC 2142 / RFC 5321 §4.5.1 reserve `postmaster@` (and abuse@/noc@/
    security@); these can't be claimed as a user alias (the reservation
    set blocks alias creation), so the exact-alias lookup always misses.
    The nest `validate_recipient` handler must NOT 550 that miss for a
    reserved role local-part — it routes to the deployment admin actor
    (the box's claimer) and never rejects (smtp-server.md :201).

    Two assertions in one transaction isolate the routing as the cause:
      - `postmaster@domain` (no alias) → 250 at RCPT TO. Without T1 this
        is a 550 5.1.1 "No such user here" (the unknown-recipient reject).
      - a fresh non-reserved local (`ghost-<ts>@domain`, also no alias) →
        550 at RCPT TO. Proves the never-reject is specific to role
        addresses, not "everything 250s now".

    The full DATA round-trip then proves postmaster mail resolved to a
    *real* actor nest accepts for ingest: the admin (`role_address_venue`
    registers a key for its own admin, mirroring a real onboarded claimer,
    so the inbound HPKE seal-to-recipient succeeds). A 250 on `.` is the
    round-trip signal — same contract as `test_inbound_mx_round_trip`.
    """
    handle = role_address_venue
    domain = handle.domain
    postmaster = f"postmaster@{domain}"
    ghost = f"ghost-{int(time.time() * 1000)}@{domain}"

    body_lines = [
        "From: Operator <ops@external.test>",
        f"To: {postmaster}",
        "Subject: T2.5 postmaster role-address routing",
        f"Message-ID: <pm-{int(time.time() * 1000)}@external.test>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Operator-to-operator mail to a role address must never bounce.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<ops@external.test>", "250", deadline)
        # Reserved role local-part with no alias → never-reject → 250.
        conn.cmd(f"RCPT TO:<{postmaster}>", "250", deadline)
        # Non-reserved unknown local → still the unknown-recipient 550.
        conn.cmd(f"RCPT TO:<{ghost}>", "550", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        # Delivered to the admin actor (the only accepted recipient).
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-server", "admin-mail-policy")
def test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses(
    role_address_venue,
):
    """An admin-lowered IMAP storage quota actually *binds* on the inbound MX
    delivery path, and the role-address exemption survives the lowered cap.

    An earlier nest mail/IMAP follow-up made `enforce_imap_storage_quota` (and the reporting
    handler + `delete_nonempty` + CalDAV retention reads) consume the
    **effective** policy — catalog default ⊕ the admin `put_imap_policy`
    override — instead of the hardcoded `ImapPolicy::default()` 1 GiB ceiling.
    This proves the full loop end-to-end over the real wire: admin WS-RPC
    `put_imap_policy {storage_bytes_default: 10}` → nest `mail_imap_policy` row
    → Go MTA inbound DATA → `fauna.bridges.ingest_inbound_mail` →
    `enforce_imap_storage_quota` reads the effective 10-byte ceiling →
    `over_quota` typed error → `server.go` maps it to `552 5.2.2 Mailbox full`
    on end-of-DATA. A second transaction to `postmaster@` (a role address)
    over the same ceiling still delivers `250`: nest skips the pre-check for
    role addresses (the `is_role_address` bit from `validate_recipient`;
    smtp-server.md :204), so the cap never reaches that branch.

    The **IMAP APPEND → `NO [OVERQUOTA]`** half of the effective-override
    binding is covered by the nest handler unit test
    `bridge_imap_handlers::tests::append_over_lowered_override_quota_is_rejected`
    — IMAP MUA-AUTH (LOGIN) over the wire is deferred to a follow-up nest
    mail/MDA track (see `test_mail_bridge_mda.py`), so the APPEND path can't be driven
    end-to-end here yet. The Go `NO [OVERQUOTA]` mapping itself is pinned by
    `internal/mda/imap/append_test.go::TestAppendOverQuotaMapsToNoOverQuota`.

    Runs on `role_address_venue` (the postmaster leg delivers to the admin;
    see that fixture), with a regular recipient of this test's own. The
    override is **restored to the catalog default in `finally`** — the venue is
    module-scoped, and a stray 10-byte ceiling would `552` every later
    delivery on it.
    """
    from conftest import _make_user

    handle = role_address_venue
    domain = handle.domain
    nest = handle.nest
    recipient = _make_user(nest)
    recipient_local = f"quota{secrets.token_hex(4)}"
    recipient_addr = f"{recipient_local}@{domain}"
    postmaster = f"postmaster@{domain}"
    add_exact_alias(nest["url"], recipient["signing_key"], domain, recipient_local)
    _register_harness_seal_key(nest, recipient["actor_id_bytes"])

    def _set_imap_storage_cap(storage_bytes: int) -> None:
        # `put_imap_policy` full-replaces the single `mail_imap_policy` row;
        # the other knobs left unset resolve to their catalog defaults.
        with _admin_ws(nest) as ws:
            ws.call(
                "fauna.bridges.put_imap_policy",
                {"storage_bytes_default": storage_bytes},
            )

    def _body(to_addr: str, subject: str) -> bytes:
        lines = [
            "From: External Sender <sender@external.test>",
            f"To: {to_addr}",
            f"Subject: {subject}",
            f"Message-ID: <q-{int(time.time() * 1000)}@external.test>",
            "Date: Sun, 24 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "This body is far larger than a 10-byte mailbox-quota ceiling.",
        ]
        return ("\r\n".join(lines) + "\r\n").encode()

    # 10-byte ceiling: any real message exceeds it, so the regular recipient is
    # always over-quota regardless of prior usage on this session-scoped nest.
    _set_imap_storage_cap(10)
    try:
        # Regular recipient over the lowered cap → 552 5.2.2 on end-of-DATA.
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_body(recipient_addr, "over the lowered quota"))
            reply = conn.cmd(".", "552", deadline)
            assert "5.2.2" in reply, f"expected 552 5.2.2 Mailbox full, got {reply!r}"
            conn.cmd("QUIT", "221", deadline)

        # Role address bypasses the quota → still delivers (250) under the cap.
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd("MAIL FROM:<ops@external.test>", "250", deadline)
            conn.cmd(f"RCPT TO:<{postmaster}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_body(postmaster, "role address bypasses quota"))
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)
    finally:
        _set_imap_storage_cap(1 << 30)


@pytest.mark.feature("mail-server")
def test_inbound_greylist_defers_first_then_passes_on_retry(mail_bridge_mta, nest_instance):
    """Greylist state lives **nest-side** (per the nest mail-bridge greylist follow-up): a
    first-seen `(sender_domain, recipient, /24)` tuple is deferred `451 4.7.1`
    at RCPT TO; the retry from the same tuple — on a brand-new connection,
    proving the state survives connection teardown (and structurally bridge
    restart, since the bridge now holds no greylist map at all) — is accepted
    `250` and round-trips through ingest.

    Greylisting is the one inbound gate the new design enforces nest-side, so
    nest reads `greylist_enabled` fresh on every `check_greylist` call: this
    test flips it on via the production `put_spam_policy` admin override
    (`greylist_enabled=true`, `greylist_delay_secs=0` so the retry passes with
    no wall-clock wait — the 60 s / 4 h / 30 d timing is unit-tested in
    `fauna_mail::greylist` + the nest handler). The override is **restored to
    the fixture's `greylist_enabled=false` in `finally`** so it can't leak into
    a later test on this session-scoped nest (a stray-on greylist would 451 the
    first RCPT of every sibling inbound test). `dnsbl_servers=[]` / `fcrdns_
    mode=off` are re-sent on both writes because `put_spam_policy` full-replaces
    the override row (dropping them would revert to the DNSBL/PTR catalog
    defaults the fixture disabled).

    A **unique sender domain** keeps the tuple fresh regardless of earlier
    inbound tests — greylisting keys on the sender domain, so the sibling
    tests' `external.test` senders never collide with this one.

    Placed before `test_inbound_graceful_shutdown_*` for the same reason as the
    forward test below: that test SIGTERMs the session bridge and never
    respawns it.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    # Unique sender domain → fresh greylist tuple, isolated from sibling tests.
    sender = f"op@greylist-{int(time.time() * 1000)}.external.test"
    admin = nest_instance["admin"]

    def _set_greylist(enabled: bool) -> None:
        ws = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with ws:
            ws.call(
                "fauna.bridges.put_spam_policy",
                {
                    "baseline_standing_publish": False,
                    # Preserve the fixture's other overrides — put_spam_policy
                    # full-replaces, and the MTA now HOT-APPLIES spam policy on
                    # the config_changed push (nest mail-bridge hot-reload work, Slice 2).
                    # Omitting max_conn_per_min here would reset the live per-IP
                    # rate limiter to the catalog default (~10), and since every
                    # test connects from 127.0.0.1 sharing one budget, later
                    # tests in this session-scoped fixture would then spuriously
                    # 421. Re-send the fixture's 1000, like dnsbl/fcrdns below.
                    "dnsbl_servers": [],
                    "fcrdns_mode": "off",
                    "max_conn_per_min": 1000,
                    "greylist_enabled": enabled,
                    "greylist_delay_secs": 0,
                },
            )

    body_lines = [
        f"From: Operator <{sender}>",
        f"To: {recipient_addr}",
        "Subject: greylist nest-side defer-then-pass",
        f"Message-ID: <gl-{int(time.time() * 1000)}@external.test>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Greylisted on first contact; accepted on retry.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    _set_greylist(True)
    try:
        # First contact from a fresh tuple → 451 4.7.1 at RCPT TO.
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "451", deadline)
            conn.cmd("QUIT", "221", deadline)

        # Retry on a brand-new connection — state persisted nest-side → 250.
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)
    finally:
        _set_greylist(False)


@pytest.mark.feature("mail-filter-rules")
def test_inbound_forward_all_enqueues_forwarded_outbound_row(mail_bridge_mta, nest_instance):
    """Forward-all set → an inbound MX message enqueues an is_forwarded
    outbound_mail_queue row attributed to the recipient (mail-forwarding N2-go).

    Seeds the recipient's `forward_all_to` into `mail_account_settings` (the
    same row the User WS-RPC `set_forward_all_to` writes — N1's config-SET path
    has its own nest unit tests; this test exercises the N2-go delivery
    *trigger* in the Go MTA), sends a real inbound message through port 25, and
    asserts a forward row lands carrying `is_forwarded=1`,
    `forward_actor_id=<recipient>`, `forward_rule_id="forward-all"`, and
    `recipient=<forward target>` — plus the original envelope sender.

    Deliberately does NOT assert downstream delivery: the SRS envelope rewrite
    (N3) is unbuilt, so a real downstream MX would SPF-reject the row. The
    enqueue is the whole N2-go contract; dispatch is N3+.

    Test placement is no longer load-bearing: `test_inbound_graceful_
    shutdown_*` now SIGTERMs its own `disposable_mta_bridge`, not the
    session bridge, so a bridge-using test ordered after it no longer gets
    ConnectionRefused.

    `nest_instance` is taken alongside `mail_bridge_mta` to reach the same
    (session-scoped) nest's SQLite file. `outbound_mail_queue` accumulates
    across the session, so the assertions filter on this test's unique forward
    destination, and the seeded config row is cleared in `finally` so it never
    trips a later test that mails the same shared recipient.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_local = handle.recipient_local_part
    recipient_addr = f"{recipient_local}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    # Unique, external (non-local-domain) destination so the row is isolable on
    # the shared queue and passes the forward-target validator.
    forward_target = f"forwarded-{int(time.time() * 1000)}@external.test"
    sender = "sender@external.test"
    db_path = nest_instance["db_path"]

    def _seed_forward_config(value):
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)"
                " VALUES (?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to, updated_at = excluded.updated_at",
                (recipient_actor, value, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    _seed_forward_config(forward_target)
    try:
        body_lines = [
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            "Subject: forward-all delivery trigger",
            f"Message-ID: <fwd-{int(time.time() * 1000)}@external.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Forward me downstream.",
        ]
        body = "\r\n".join(body_lines) + "\r\n"

        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            # 250 on `.` signals the full ingest path committed; the forward
            # enqueue runs in the same DATA handler before this ACK.
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # Poll the queue for our forwarded row (the WS-RPC write to nest may
        # lag the SMTP ACK by a hair under load).
        row = None
        poll_deadline = time.monotonic() + 10.0
        while time.monotonic() < poll_deadline:
            db = sqlite3.connect(db_path, timeout=10.0)
            try:
                cur = db.execute(
                    "SELECT recipient, original_sender, forward_rule_id, forward_actor_id,"
                    " is_forwarded FROM outbound_mail_queue WHERE recipient = ?",
                    (forward_target,),
                )
                found = cur.fetchall()
            finally:
                db.close()
            if found:
                assert len(found) == 1, f"expected one forward row for {forward_target}, got {found}"
                row = found[0]
                break
            time.sleep(0.25)

        assert row is not None, (
            f"no outbound_mail_queue row for forward target {forward_target} within 10s"
        )
        recipient_col, original_sender, forward_rule_id, forward_actor_id, is_forwarded = row
        assert is_forwarded == 1, f"is_forwarded: {is_forwarded!r}"
        assert recipient_col == forward_target, f"forward destination: {recipient_col!r}"
        assert original_sender == sender, f"original_sender: {original_sender!r}"
        assert forward_rule_id == "forward-all", f"forward_rule_id: {forward_rule_id!r}"
        assert bytes(forward_actor_id) == recipient_actor, (
            "forward_actor_id must attribute the forward to the recipient account"
        )
    finally:
        # Clear the seeded config so later tests mailing this shared recipient
        # don't unexpectedly forward.
        _seed_forward_config(None)


@pytest.mark.feature("admin-forwarders")
def test_inbound_admin_forwarder_redirects_to_external(mail_bridge_mta, nest_instance):
    """Admin external forwarder (mail-aliases.md § Kind 7) → an inbound MX
    message to a mailbox-less forwarder address is redirected to its external
    target through the shared forward dispatch, attributed to the managing admin,
    with NO local mailbox write. The Slice-3 AF-dispatch deliverable
    (mail-forwarding.md § Admin external forwarders).

    Drives the production flow end to end: `fauna.bridges.create_forwarder`
    (Admin WS-RPC) registers `info-<ts>@<domain>` → an external address; a real
    inbound through port 25 resolves via `resolve_recipient` to the `Forward`
    outcome (the RCPT-TO cutover this slice landed), and the Go MTA dispatches it
    via `forward_message(copy_mode=redirect)` reusing the shared loop/SRS/NDR
    pipeline. Asserts one `outbound_mail_queue` forward row (`is_forwarded=1`,
    `forward_actor_id=<admin>`, `forward_rule_id="forwarder"`,
    `recipient=<external target>`) AND that no `bridge_imap_messages` row was
    written for it (redirect-shaped, no local copy — mail-forwarding.md:59).

    Like the forward-all sibling, downstream delivery is not asserted (the SRS
    envelope rewrite is nest-side at queue-out; the enqueue is the dispatch
    contract). `outbound_mail_queue` accumulates across the session-scoped nest,
    so the queue assertion filters on this test's unique external destination.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    domain = handle.domain
    admin = nest_instance["admin"]
    admin_actor = admin["actor_id_bytes"]
    db_path = nest_instance["db_path"]

    ts = int(time.time() * 1000)
    forwarder_local = f"info-{ts}"
    forwarder_addr = f"{forwarder_local}@{domain}"
    # Unique external (non-local-domain) destination: isolable on the shared
    # queue and passes the forward-target validator (not a hosted domain).
    forward_target = f"forwarded-{ts}@external.test"
    sender = "sender@external.test"

    # Register the forwarder via the production Admin RPC (the alias-half this
    # slice consumes; create_forwarder attributes the row to the calling admin).
    ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with ws:
        ws.call(
            "fauna.bridges.create_forwarder",
            {"local_domain": domain, "pattern": forwarder_local, "forward_target": forward_target},
        )

    def _imap_row_count():
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            return db.execute("SELECT COUNT(*) FROM bridge_imap_messages").fetchone()[0]
        finally:
            db.close()

    # Baseline AFTER create_forwarder, BEFORE sending — the forwarder must add no
    # INBOX row (tests in this worker run serially against the one session nest,
    # and every prior message committed synchronously on its 250, so the count is
    # stable across this single send).
    imap_before = _imap_row_count()

    body_lines = [
        f"From: External Sender <{sender}>",
        f"To: {forwarder_addr}",
        "Subject: admin forwarder redirect",
        f"Message-ID: <af-{ts}@external.test>",
        "Date: Mon, 01 Jun 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Redirect me to the external destination.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        # Forwarder address (no local mailbox) accepted at RCPT via the Forward
        # resolver outcome — without the cutover this would 550 (exact-only).
        conn.cmd(f"RCPT TO:<{forwarder_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    row = None
    poll_deadline = time.monotonic() + 10.0
    while time.monotonic() < poll_deadline:
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            found = db.execute(
                "SELECT recipient, original_sender, forward_rule_id, forward_actor_id,"
                " is_forwarded FROM outbound_mail_queue WHERE recipient = ?",
                (forward_target,),
            ).fetchall()
        finally:
            db.close()
        if found:
            assert len(found) == 1, f"expected one forward row for {forward_target}, got {found}"
            row = found[0]
            break
        time.sleep(0.25)

    assert row is not None, f"no outbound_mail_queue forward row for {forward_target} within 10s"
    recipient_col, original_sender, forward_rule_id, forward_actor_id, is_forwarded = row
    assert is_forwarded == 1, f"is_forwarded: {is_forwarded!r}"
    assert recipient_col == forward_target, f"forward destination: {recipient_col!r}"
    assert original_sender == sender, f"original_sender: {original_sender!r}"
    assert forward_rule_id == "forwarder", f"forward_rule_id: {forward_rule_id!r}"
    assert bytes(forward_actor_id) == admin_actor, (
        "forward_actor_id must attribute the forward to the managing admin"
    )

    # Redirect-shaped: NO local mailbox row was written for the forwarder.
    assert _imap_row_count() == imap_before, (
        "an admin forwarder keeps no local copy (redirect, mail-forwarding.md:59)"
    )


@pytest.mark.feature("mail-filter-rules")
def test_inbound_forward_all_dispatches_under_srs(mail_bridge_mta, nest_instance):
    """Forward-all set → the forwarded message leaves the box with an
    SRS-rewritten envelope MAIL FROM (mail-forwarding N3 — § SRS on outbound).

    Builds on the N2-go enqueue test: with `forward_all_to` pointing at an
    external address that resolves (via `mta_mx_override`) to the in-process
    stub MX, a real inbound message is locally delivered, the Go MTA enqueues
    an `is_forwarded` row carrying the *original* envelope, and nest rewrites
    the envelope MAIL FROM under SRS at `fetch_outbound_due` (queue-out,
    nest-side, so the per-deployment SRS secret never leaves nest). The
    outbound worker then dispatches to the stub, which records the rewritten
    sender.

    Asserts the stub received the forwarded message under a
    `SRS0=…@<primary-domain>` MAIL FROM (so the downstream MX's SPF check
    aligns to *our* domain), and that the rewrite embeds the original sender's
    domain (percent-escaped) so the bounce decodes back. Pre-N3 the row
    dispatched with the original sender and SPF-failed downstream.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    stamp = int(time.time() * 1000)
    # External forward target → routed to the stub MX by the fixture's
    # `mta_mx_override` for `external.test`. The original sender's domain rides
    # the SRS payload, so a unique sender lets us isolate THIS forward.
    forward_target = f"srs-fwd-{stamp}@external.test"
    sender_local = f"srs-orig-{stamp}"
    sender = f"{sender_local}@orig.test"
    token = f"srs-token-{stamp}"

    def _seed_forward_config(value):
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)"
                " VALUES (?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to, updated_at = excluded.updated_at",
                (recipient_actor, value, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    _seed_forward_config(forward_target)
    try:
        body_lines = [
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            f"Subject: srs forward dispatch {token}",
            f"Message-ID: <{token}@orig.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Forward me downstream under SRS.",
        ]
        body = "\r\n".join(body_lines) + "\r\n"

        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # The outbound worker drains the forward row and delivers to the stub;
        # the stub records the envelope MAIL FROM nest rewrote. The N4 forward-
        # enqueue nudge (maybeForwardForRecipient → outboundTrigger) pokes the
        # worker, so the row drains on the next poll rather than after a full
        # PollInterval — delivery is prompt; the deadline is generous for CI.
        mail_from = None
        wait_deadline = time.monotonic() + 20.0
        while time.monotonic() < wait_deadline:
            for env_from, raw in handle.stub_mx.records():
                if token.encode() in raw:
                    mail_from = env_from
                    break
            if mail_from is not None:
                break
            time.sleep(0.25)

        assert mail_from is not None, (
            f"stub MX received no forwarded message tagged {token} within 45s — "
            f"SRS dispatch did not complete (bridge log: {handle.log_file})"
        )
        assert mail_from.startswith("SRS0="), (
            f"forwarded envelope must be SRS-rewritten, got MAIL FROM {mail_from!r}"
        )
        assert mail_from.endswith(f"@{domain}"), (
            f"SRS rewrite must align the envelope to our primary domain {domain!r}, "
            f"got {mail_from!r}"
        )
        # The original sender's domain rides the SRS payload so the bounce can
        # decode back to it (the localpart slot is percent-escaped).
        assert "orig.test" in mail_from, (
            f"SRS payload must carry the original sender domain: {mail_from!r}"
        )
    finally:
        _seed_forward_config(None)


@pytest.mark.feature("mail-filter-rules")
def test_inbound_srs_bounce_routes_to_forwarder(mail_bridge_mta, nest_instance):
    """A permanent-failure bounce of a forward — addressed to our `SRS0=…@<domain>`
    envelope — is recognized at RCPT-TO, decoded+verified nest-side, and delivered
    to the *forwarder's* mailbox, not the original sender (mail-forwarding N4 —
    § Bounce decode / § NDR routing). A forged `SRS0=` (tampered MAC) is rejected.

    End-to-end (builds on the N3 SRS-dispatch test): seed forward-all → send a
    real inbound → the forward dispatches under SRS, and the stub MX records the
    `SRS0=…@<domain>` MAIL FROM — which is exactly the address a downstream
    bounce returns to. We then play the downstream MX: connect to port 25 and
    send a null-sender DSN to that SRS0 address. The bridge calls
    `decode_srs_bounce`, gets `ok`, and delivers the DSN to the forwarder's
    INBOX (one new `bridge_imap_messages` row). A copy of the SRS0 address with
    one HHH (MAC) char flipped decodes `mac_fail` → 550 5.1.1.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    stamp = int(time.time() * 1000)
    forward_target = f"srs-bnc-{stamp}@external.test"
    sender = f"srs-bnc-orig-{stamp}@orig.test"
    token = f"srs-bnc-token-{stamp}"

    def _seed_forward_config(value):
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)"
                " VALUES (?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to, updated_at = excluded.updated_at",
                (recipient_actor, value, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    def _inbox_count():
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute(
                "SELECT COUNT(*) FROM bridge_imap_messages"
                " WHERE actor_id = ? AND mailbox = 'INBOX'",
                (recipient_actor,),
            ).fetchone()
            return n
        finally:
            conn.close()

    _seed_forward_config(forward_target)
    try:
        # 1. Real inbound → local delivery + a forward dispatched under SRS.
        body_lines = [
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            f"Subject: srs bounce setup {token}",
            f"Message-ID: <{token}@orig.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Forward me so a bounce comes back.",
        ]
        body = "\r\n".join(body_lines) + "\r\n"
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # 2. Capture the SRS0 envelope the stub recorded — the address the
        #    downstream MX would bounce back to. Prompt via the N4 worker nudge.
        srs_addr = None
        wait_deadline = time.monotonic() + 20.0
        while time.monotonic() < wait_deadline:
            for env_from, raw in handle.stub_mx.records():
                if token.encode() in raw and env_from.startswith("SRS0="):
                    srs_addr = env_from
                    break
            if srs_addr is not None:
                break
            time.sleep(0.25)
        assert srs_addr is not None, (
            f"stub MX recorded no SRS-rewritten forward for {token} within 20s "
            f"(bridge log: {handle.log_file})"
        )
        assert srs_addr.endswith(f"@{domain}"), f"unexpected SRS envelope: {srs_addr!r}"

        # 3. Play the downstream MX: a null-sender DSN bounced to the SRS0 address.
        before = _inbox_count()
        dsn_lines = [
            "From: Mail Delivery System <MAILER-DAEMON@external.test>",
            f"To: <{srs_addr}>",
            "Subject: Delivery Status Notification (Failure)",
            f"Message-ID: <dsn-{stamp}@external.test>",
            "Date: Thu, 16 May 2026 12:05:00 +0000",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"Your message to {forward_target} could not be delivered.",
            "550 5.1.1 mailbox unavailable",
        ]
        dsn = "\r\n".join(dsn_lines) + "\r\n"
        deadline2 = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline2) as conn:
            conn.cmd("MAIL FROM:<>", "250", deadline2)
            # The SRS0 recipient is recognized + decoded `ok` → accepted (a plain
            # validate_recipient would 550 it — it's not a real local user).
            conn.cmd(f"RCPT TO:<{srs_addr}>", "250", deadline2)
            conn.cmd("DATA", "354", deadline2)
            conn.send_raw(dsn.encode())
            conn.cmd(".", "250", deadline2)  # 250 ⇒ ingest to the forwarder succeeded
            conn.cmd("QUIT", "221", deadline2)

        # 4. The bounce landed in the forwarder's INBOX (placement is synchronous
        #    with the DATA "." 250; poll briefly for any journal lag).
        after = before
        place_deadline = time.monotonic() + 10.0
        while time.monotonic() < place_deadline:
            after = _inbox_count()
            if after > before:
                break
            time.sleep(0.25)
        assert after == before + 1, (
            f"the SRS bounce must be delivered to the forwarder's mailbox: "
            f"INBOX count {before} → {after}"
        )

        # 5. A forged SRS0 — one HHH (MAC) char flipped — fails verification 550.
        local = srs_addr.rsplit("@", 1)[0]
        i = 5  # first HHH char, right after the "SRS0=" prefix
        flipped = "B" if local[i] != "B" else "C"
        forged_addr = f"{local[:i]}{flipped}{local[i + 1:]}@{domain}"
        assert forged_addr != srs_addr, "forge must change the address"
        deadline3 = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline3) as conn:
            conn.cmd("MAIL FROM:<>", "250", deadline3)
            # decode_srs_bounce → mac_fail → hard-reject (mail-forwarding.md:100).
            conn.cmd(f"RCPT TO:<{forged_addr}>", "550", deadline3)
            conn.cmd("QUIT", "221", deadline3)
    finally:
        _seed_forward_config(None)


@pytest.mark.feature("mail-server", "mail-filter-rules")
def test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay(mail_bridge_mta, nest_instance):
    """When OUR outbound worker permanently fails to deliver a forward, the DSN
    is sealed DIRECTLY into the forwarder's INBOX — never the original sender,
    and never relayed back out over the MX (mail-forwarding.md § NDR routing/N4b;
    smtp-server.md § Outbound submission flow).

    This is the *synchronous-permfail* half (distinct from N4a's async inbound
    bounce). The OLD design enqueued the NDR to the forward row's own
    `SRS0=…@<primary-domain>` envelope and relied on the inbound SRS-decode
    loopback; on a containerized deploy that hairpinned through the docker bridge
    and 554-bounced at the inbound HELO-identity check — the same self-loop class
    fixed for in-domain mailbox / DSN / auto-reply delivery. The fix
    (`generate_forwarder_ndr`) seals the DSN straight to the forwarder, a known
    in-domain actor whose id rides on the queue row, via `seal_and_ingest_local`
    — uniform with security mail, no MX loopback.

    Setup: per-user `forward_all_to` to a target the stub MX 550-rejects at RCPT
    TO (`nonexistent-fwd-*@external.test`). A real inbound is delivered locally
    AND triggers the forward; the outbound worker attempts delivery, the stub
    550s the RCPT → the bridge classifies permanent → `mark_outbound_bounced` →
    nest's `generate_permfail_bounce` → `generate_forwarder_ndr` seals the DSN
    into the forwarder's INBOX.

    We assert (1) the DSN is sealed into the forwarder's INBOX — a new message
    whose plaintext-floor `from_norm` is OUR domain (the DSN `From:
    postmaster@<domain>`, distinct from the locally-delivered original whose
    `from_norm` is the external sender domain), and the INBOX grows by two (the
    kept local copy + the bounce); and (2) NO `multipart/report` is relayed out
    to the downstream MX — the regression guard for the containerized SRS0
    hairpin. The DSN *body* shape (multipart/report, Final-Recipient, the
    forward-rule blurb) is covered by the Rust unit
    `forwarder_dsn_body_carries_rule_and_recipients`; here we assert the
    cross-binary seal-and-deliver integration the unit cannot reach.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    stamp = int(time.time() * 1000)
    # `nonexistent-fwd-*` → stub MX 550s the RCPT (deterministic permanent 5xx).
    forward_target = f"nonexistent-fwd-{stamp}@external.test"
    # A distinct sender domain so its from_norm can never collide with the DSN's.
    sender = f"fwd-bnc-orig-{stamp}@bounce-orig.test"
    token = f"fwd-permfail-{stamp}"

    def _seed_forward_config(value):
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)"
                " VALUES (?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to, updated_at = excluded.updated_at",
                (recipient_actor, value, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    def _inbox_count():
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute(
                "SELECT COUNT(*) FROM bridge_imap_messages"
                " WHERE actor_id = ? AND mailbox = 'INBOX'",
                (recipient_actor,),
            ).fetchone()
            return n
        finally:
            conn.close()

    def _dsn_count():
        """INBOX rows whose plaintext-floor `from_norm` is OUR domain — the
        sealed DSN's `From: postmaster@<domain>` (`seal_and_ingest_local` stamps
        `public_metadata.sender_domain = <primary domain>`). The locally
        delivered original carries `from_norm = bounce-orig.test`, so this counts
        only forwarder NDRs."""
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute(
                "SELECT COUNT(*) FROM bridge_imap_messages"
                " WHERE actor_id = ? AND mailbox = 'INBOX'"
                "   AND LOWER(from_norm) = LOWER(?)",
                (recipient_actor, domain),
            ).fetchone()
            return n
        finally:
            conn.close()

    before_total = _inbox_count()
    before_dsn = _dsn_count()
    # Read the queue high-water mark BEFORE the trigger: step 4's guard scopes
    # to rows this test could have caused, and the fixture is session-scoped.
    before_queue_id = _max_queue_id(handle.nest_url)
    _seed_forward_config(forward_target)
    try:
        # 1. Real inbound → local delivery to the forwarder's INBOX AND a forward
        #    dispatched to the (dead) downstream target.
        body_lines = [
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            f"Subject: forward permfail setup {token}",
            f"Message-ID: <{token}@bounce-orig.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Forward me to a dead downstream so our worker gives up.",
        ]
        body = "\r\n".join(body_lines) + "\r\n"
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # 2. The forward is 550-rejected at the stub → nest seals the DSN
        #    directly into the forwarder's INBOX (no MX relay). Poll for the
        #    sealed DSN by its plaintext-floor from_norm (= our domain).
        dsn_after = before_dsn
        poll_deadline = time.monotonic() + 30.0
        while time.monotonic() < poll_deadline:
            dsn_after = _dsn_count()
            if dsn_after >= before_dsn + 1:
                break
            time.sleep(0.25)
        assert dsn_after == before_dsn + 1, (
            "the forwarder NDR was not sealed into the forwarder's INBOX within "
            f"30s — nest did not deliver the forwarded permfail locally "
            f"(from_norm-matched DSN rows {before_dsn} → {dsn_after}; "
            f"bridge log: {handle.log_file})"
        )

        # 3. The forwarder's INBOX holds BOTH the kept local copy AND the sealed
        #    DSN: it grew by exactly two. (The old hairpin behaviour would relay
        #    the DSN out — it would 554-bounce, never land — leaving only +1.)
        total_after = _inbox_count()
        place_deadline = time.monotonic() + 10.0
        while time.monotonic() < place_deadline:
            total_after = _inbox_count()
            if total_after >= before_total + 2:
                break
            time.sleep(0.25)
        assert total_after == before_total + 2, (
            "the forwarder INBOX must hold the locally-delivered original AND the "
            f"sealed DSN: count {before_total} → {total_after} "
            f"(expected {before_total + 2})"
        )

        # 4. Regression guard for the containerized SRS0 hairpin: the DSN must
        #    NOT be relayed back out over the MX (the old design hairpinned one
        #    here, which 554-bounced on a real deploy).
        #
        #    Asserted on the ENQUEUE side, anchored to the forwarded row going
        #    terminal — `generate_forwarder_ndr` seals the DSN and only then
        #    marks the row bounced, so a terminal row proves the NDR path ran to
        #    completion and enqueued whatever it was going to enqueue. See
        #    `_await_bounce_decided` for the full ordering. This replaced a
        #    `sleep(2.0)`, which could not have caught the regression anyway:
        #    a relayed DSN reaches the stub only after an outbound drain cycle,
        #    and the worker's `PollInterval` is 30 s.
        bounced = _await_bounce_decided(
            handle.nest_url,
            msgid=f"<{token}@bounce-orig.test>",
            recipient=forward_target,
        )
        assert bounced["status"] == "bounced", (
            "the forwarded row should end `bounced` (the 550 is a permanent "
            f"failure), not {bounced['status']!r} — reason: {bounced['last_error']!r}"
        )
        relayed = _hairpinned_dsn_rows(handle.nest_url, before_queue_id, domain)
        assert relayed == [], (
            "regression: the forwarder DSN was ENQUEUED for MX relay instead of "
            "being sealed into the forwarder's INBOX (the SRS0 hairpin is back) "
            f"— null-sender rows enqueued by this test: "
            f"{[(r['original_msgid'], r['recipient']) for r in relayed]}"
        )

        # The delivery-side twin of the same guard, now free of any wait: with
        # the enqueue side proven empty, nothing can reach the downstream MX.
        for msg in handle.stub_mx.messages():
            assert b"multipart/report" not in msg.lower(), (
                "regression: a forwarder DSN was relayed out to the downstream MX "
                "(the SRS0 hairpin is back) — it must be sealed locally instead:\n"
                f"{msg[:400]!r}"
            )
    finally:
        _seed_forward_config(None)


@pytest.mark.feature("mail-filter-rules")
def test_forward_rate_cap_suppresses_over_cap_forward_and_notifies(mail_bridge_mta, nest_instance):
    """A forward over the per-account hourly cap is parked (not dispatched), and
    at the queue ceiling the forwarder gets an in-app eviction notification
    (mail-forwarding N5 — § Per-account forward rate-limit / § Queue ceiling).

    Drives the real MTA→nest `forward_message` rate-cap path with the recipient's
    `forward_per_hour` seeded to **0**, which is the deterministic over-cap edge:
    every forward exceeds `min(0, ceiling) = 0`, so it is parked, and the queue
    ceiling (`0 * 24 = 0`) immediately FIFO-evicts it, firing the
    `mail.forward_queue_evicted` in-app notification (NOT an email — § Don't do
    these). The end-to-end contract proven: the cap is consulted at
    `forward_message`, an over-cap forward produces **no** outbound dispatch row,
    and the forwarder is notified in-app.

    `forward_per_hour=0` is order-independent: unlike a small positive cap, it
    doesn't depend on how many `is_forwarded` rows sibling forward tests already
    put on this session-shared recipient's hourly sliding window.

    Placed before `test_inbound_graceful_shutdown_*` (it SIGTERMs the session
    bridge); the seeded config is reset in `finally`.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_local = handle.recipient_local_part
    recipient_addr = f"{recipient_local}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    forward_target = f"ratecap-{int(time.time() * 1000)}@external.test"
    sender = "sender@external.test"
    db_path = nest_instance["db_path"]

    def _seed(forward_all_to, per_hour):
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings"
                " (actor_id, forward_all_to, forward_per_hour, updated_at)"
                " VALUES (?, ?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to,"
                " forward_per_hour = excluded.forward_per_hour,"
                " updated_at = excluded.updated_at",
                (recipient_actor, forward_all_to, per_hour, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    def _evicted_notif_count():
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            cur = db.execute(
                "SELECT COUNT(*) FROM notifications"
                " WHERE actor_id = ? AND notif_type = 'mail.forward_queue_evicted'",
                (recipient_actor,),
            )
            return cur.fetchone()[0]
        finally:
            db.close()

    baseline_notifs = _evicted_notif_count()
    # forward-all on, cap 0 → every forward is over-cap.
    _seed(forward_target, 0)
    try:
        body_lines = [
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            "Subject: rate-cap over-cap suppression",
            f"Message-ID: <ratecap-{int(time.time() * 1000)}@external.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "This forward should be capped.",
        ]
        body = "\r\n".join(body_lines) + "\r\n"

        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            # Local delivery still commits + ACKs; only the forward is capped.
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

        # The eviction notification is the positive end-to-end signal.
        poll_deadline = time.monotonic() + 10.0
        got_notif = False
        while time.monotonic() < poll_deadline:
            if _evicted_notif_count() > baseline_notifs:
                got_notif = True
                break
            time.sleep(0.25)
        assert got_notif, (
            "an over-cap forward must record a mail.forward_queue_evicted "
            "in-app notification for the forwarder"
        )

        # No outbound dispatch row for the capped forward, and nothing retained
        # in the holding queue (ceiling 0 evicted it).
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            dispatched = db.execute(
                "SELECT COUNT(*) FROM outbound_mail_queue WHERE recipient = ?",
                (forward_target,),
            ).fetchone()[0]
            parked = db.execute(
                "SELECT COUNT(*) FROM forward_queue WHERE actor_id = ?",
                (recipient_actor,),
            ).fetchone()[0]
        finally:
            db.close()
        assert dispatched == 0, "an over-cap forward must NOT be dispatched outbound"
        assert parked == 0, "at ceiling 0 the parked forward is FIFO-evicted, not retained"
    finally:
        # Reset to disabled + the default cap so later shared-recipient tests
        # forward normally.
        _seed(None, 100)


@pytest.mark.feature("mail-aliases")
def test_inbound_over_per_alias_rate_cap_tempfails_451(mail_bridge_mta):
    """A user-set per-alias rate cap actually turns mail away at RCPT with
    `451 4.7.0 Rate limit exceeded` (mail-aliases.md § Per-alias rate-cap).

    The knob was end-to-end *except* the decision: the app add-sheet wrote
    `rate_limit_per_hour`, nest stored it, the Go MTA received it as
    `control_overrides` — and nothing read it, so every message delivered no
    matter what the user set. This test is the gate's witness, and it asserts
    the user-visible effect (delivery vs. tempfail on the wire), never a
    counter.

    Both halves on one alias, which makes `1/hour` the deterministic edge:
      - message 1 is under the cap → RCPT 250 and DATA `.` 250 (the bridge
        ACKs `.` only once the WS-RPC ingest committed nest-side, so a 250
        there is proof of delivery, not just of acceptance);
      - message 2 is over it → RCPT **451**, the ratified *tempfail* — a
        legitimate sender retries when the window rolls off, where a 5xx
        would bounce the mail permanently.

    The alias is minted through the production **user** door
    (`fauna.bridges.create_account_alias`, the RPC the apps' add-sheet calls)
    rather than seeded into SQLite, so the row the gate reads is the row the
    product writes. It carries a unique timestamped local-part, so its hit
    window starts empty regardless of what sibling tests did to the
    session-shared recipient.

    Placed before `test_inbound_graceful_shutdown_*`, which SIGTERMs its
    bridge.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    domain = handle.domain
    recipient = handle.recipient_actor
    ts = int(time.time() * 1000)
    capped_local = f"capped-{ts}"
    capped_addr = f"{capped_local}@{domain}"
    sender = "sender@external.test"

    # The signed WS-RPC client is class-agnostic — the caller's actor decides
    # its class nest-side — so the recipient signs their own User-class
    # `create_account_alias`.
    ws = WsRpcAdminClient(
        handle.nest_url,
        actor_id=recipient["actor_id_bytes"],
        signing_key=bytes(recipient["signing_key"]),
    )
    with ws:
        ws.call(
            "fauna.bridges.create_account_alias",
            {
                "kind": "exact",
                "local_domain": domain,
                "pattern": capped_local,
                "controls": {
                    "label": "rate-capped",
                    "spam_threshold_override": None,
                    "rate_limit_per_hour": 1,
                    "rate_limit_per_day": None,
                },
            },
        )

    def _message(tag):
        return "\r\n".join(
            [
                f"From: External Sender <{sender}>",
                f"To: {capped_addr}",
                f"Subject: per-alias rate cap {tag}",
                f"Message-ID: <cap-{ts}-{tag}@external.test>",
                "Date: Thu, 16 May 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"Message {tag} to a 1/hour-capped alias.",
            ]
        ) + "\r\n"

    deadline = time.monotonic() + 60.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        # Under the cap → delivered.
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{capped_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_message("one").encode())
        conn.cmd(".", "250", deadline)

        # Over the cap → tempfail at RCPT, before DATA is ever offered.
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        reply = conn.cmd(f"RCPT TO:<{capped_addr}>", "451", deadline)
        conn.cmd("QUIT", "221", deadline)

    assert "4.7.0" in reply, (
        "the over-quota reply must carry the ratified enhanced code 4.7.0, "
        f"got {reply!r}"
    )
    assert "Rate limit exceeded" in reply, (
        f"expected the ratified reason text, got {reply!r}"
    )


@pytest.mark.feature("mail-server")
def test_inbound_graceful_shutdown_drains_and_421s(disposable_mta_bridge):
    """SIGTERM mid-session drains gracefully (mail-bridge-lifecycle.md
    § Shutting down; T2.6).

    With one connection already established (STARTTLS-upgraded, past the
    port-25 TLS gate), SIGTERM the bridge. The MTA must:
      1. stop accepting new connections (a fresh connect is refused);
      2. answer `421 4.3.2 Service shutting down` on a *new* MAIL FROM on
         the still-open connection — ahead of every other gate;
      3. drain that connection once it closes and exit 0 (clean drain
         within `mail.bridge.shutdown_grace_seconds`, default 30 s).

    This is the cross-binary proof that the drain budget projected via
    `fetch_config` (BridgePolicy) and the Go drain machinery cooperate on a
    real SIGTERM. The drain mechanics themselves are unit/integration-tested
    in `internal/mta` (drain_test.go); here we exercise the real signal path
    against a nest-provisioned bridge.

    Uses `disposable_mta_bridge` — a throwaway bridge with its own keypair /
    enrollment / ports (conftest.py) — so SIGTERM'ing it never strands the
    session-scoped `mail_bridge_mta` every other test in this file shares.
    The test owns the process it kills (process-safety: only ever terminate a
    bridge you spawned).
    """
    handle = disposable_mta_bridge
    deadline = time.monotonic() + 90.0

    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        # In flight when the shutdown signal arrives.
        handle.proc.terminate()

        # Wait until the listener stops accepting — gracefulDrain closes it
        # right after flipping the draining flag, so a refused fresh connect
        # means draining is in effect (no race with the MAIL FROM below).
        drain_deadline = time.monotonic() + 20.0
        while True:
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probe.settimeout(1.0)
            try:
                probe.connect(("127.0.0.1", handle.mx_port))
                probe.close()
            except OSError:
                break
            if time.monotonic() > drain_deadline:
                raise AssertionError(
                    "port 25 still accepting after SIGTERM; drain did not begin "
                    f"(see {handle.log_file})"
                )
            time.sleep(0.1)

        # New MAIL FROM on the still-open connection → 421 4.3.2, ahead of
        # the STARTTLS / policy gates.
        reply = conn.cmd("MAIL FROM:<sender@external.test>", "421", deadline)
        assert "4.3.2" in reply, f"expected 421 4.3.2 Service shutting down, got {reply!r}"

    # The `with` exit closed the socket → the in-flight session leaves → the
    # drain completes cleanly → the process exits 0.
    rc = handle.proc.wait(timeout=40)
    assert rc == 0, f"clean drain should exit 0, got exit code {rc}; see {handle.log_file}"


def _scan_body(recipient_addr: str, *, infected: bool) -> bytes:
    """An inbound message body for the content-scan tests.

    When `infected`, the body carries the fake clamd's infection marker
    (`fakes.fake_clamd.INFECTED_MARKER`) so the gate's ClamAV verdict is
    Infected → reject. Otherwise it's a plain benign message.
    """
    from fakes.fake_clamd import INFECTED_MARKER

    payload = "Hello from the content-scan e2e test."
    if infected:
        payload = INFECTED_MARKER.decode() + " " + payload
    body_lines = [
        "From: External Sender <sender@external.test>",
        f"To: {recipient_addr}",
        "Subject: T1.4 content-scan e2e",
        f"Message-ID: <scan-{int(time.time() * 1000)}@external.test>",
        "Date: Thu, 16 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        payload,
    ]
    return ("\r\n".join(body_lines) + "\r\n").encode()


@pytest.mark.feature("mail-abuse-refused")
def test_inbound_scan_clean_delivers(mail_bridge_mta):
    """A benign inbound message scans clean and ingests with scan verdict.

    T1.4 (nest mail-bridge scan-gate work, T4): the fixture wires fake clamd + rspamd
    into the bridge's operator-hatch, so the real MTA runs `applyScanGate` on
    every inbound DATA. A clean verdict delivers — and the 250 on `.` proves
    the `ingest_inbound_mail` request *carrying the new clamav_verdict +
    rspamd_score fields* round-trips through nest's strict DAG-CBOR decode
    (the cross-language wire contract for the scan fields). The stored
    `message_scan_results` row + the X-Fauna-Scan-* headers are asserted by
    the nest + Go unit tests (this tier_3 path has no scan-result read RPC
    yet — that is client-track).
    """
    handle = mail_bridge_mta
    recipient_addr = f"{handle.recipient_local_part}@{handle.domain}"
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_scan_body(recipient_addr, infected=False))
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("mail-abuse-refused")
def test_inbound_scan_infected_rejects(mail_bridge_mta):
    """An infected inbound message is rejected at the SMTP perimeter (554).

    T1.4: the fake clamd returns a FOUND verdict for a body carrying the
    infection marker; the scan gate's default action is `reject`, so the
    bridge answers `554 5.7.1 Message contains malware: <signature>` on DATA
    `.` and never stores the message. (The bridge also fires the metadata-only
    `fauna.bridges.report_rejected_scan` forensic RPC; that the row lands is
    covered by the nest unit test — there is no scan-result read RPC to assert
    it from here.)
    """
    handle = mail_bridge_mta
    recipient_addr = f"{handle.recipient_local_part}@{handle.domain}"
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(_scan_body(recipient_addr, infected=True))
        reply = conn.cmd(".", "554", deadline)
        assert "malware" in reply.lower(), f"554 reply should name malware: {reply!r}"
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("standard-mail-apps")
def test_submission_round_trip(mail_bridge_mta):
    """AUTH PLAIN on 465 → submit to an external recipient → the message
    leaves DKIM-signed and is delivered to the stub external MX.

    E.3: end-to-end exercise of the full submission stack against a real
    fauna-nest — implicit TLS on 465 (admin-uploaded TLS blob), SASL PLAIN
    AUTH (the user wrapped-submission-token, AEAD-unsealed + inner-Ed25519-
    verified bridge-side), envelope-sender identity binding, outbound
    DKIM signing (the nest signs at the outbound hand-out, under the key it
    holds for the primary domain), and
    MX delivery via the operator-hatch `mta_mx_override` to the in-process
    stub MX. The DKIM-Signature header on the delivered message is the
    round-trip signal that signing fired; `test_submission_dkim_signature_
    verifies` asserts that signature additionally *cryptographically verifies*
    against the selector's published public key (the `dkim=pass` proxy).

    Both wrapped blobs (TLS, submission token) are sealed to the bridge's
    ephemeral X25519 pubkey by the seal-helper and provisioned over WS-RPC in
    the `mail_bridge_mta` fixture; see conftest.py § 4b. The DKIM key is the
    nest's own, minted when the fixture adds the mail domain.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    # Unique per-run marker so the assertion reads THIS message off the
    # session-scoped stub MX (which accumulates every test's delivery — and
    # `wait_for_message` returns the first one forever), never an earlier
    # test's. Same per-token pattern as the outbound tests' `_wait_for_tagged`.
    token = f"sub-{int(time.time() * 1000)}"

    # SASL PLAIN initial response: authzid (empty) NUL authcid NUL passwd.
    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()

    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {external_rcpt}",
        f"Subject: E.3 submission round-trip {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Hello from the e2e submission round-trip test.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        # MAIL FROM must match the authenticated identity (Mail() binds the
        # envelope sender to sender@primary_domain).
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # The bridge enqueues the message to nest's outbound queue and the worker
    # drains it immediately (submission Data triggers a poll); the nest signs
    # it at that hand-out.
    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"outbound delivery did not complete (bridge log: {handle.log_file})"
    )
    assert b"dkim-signature:" in received.lower(), (
        "delivered message has no DKIM-Signature header — the nest handed out "
        f"unsigned mail. First 600 bytes:\n{received[:600]!r}"
    )


@pytest.mark.feature("standard-mail-apps")
def test_submission_mail_from_owned_alias_accepted(mail_bridge_mta):
    """An authenticated user may submit with MAIL FROM set to an owned
    *alias* (local-part ≠ their login handle), not just the login handle.

    This is the tier_3 end-to-end proof of the cross-domain submission
    ownership check (mail-multidomain.md § Cross-domain submission policy).
    `test_submission_round_trip` submits from the login handle, which takes
    the no-RPC fast-path (`submission.go` `localPart == authedLocal`); it
    therefore never exercises `assertMailFromOwned`. Here the envelope sender
    is a *different* local-part the same actor owns, so `Mail()` calls
    `assertMailFromOwned` → `resolve_recipient` against the **real nest** →
    the resolver returns a `Resolved` mailbox whose `actor_id` equals the
    authenticated actor → the MTA accepts (250) and delivers. macOS Mail's
    "From" selector sets exactly this kind of owned-alias envelope sender.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    login = f"{handle.submission_sender_local}@{domain}"
    owned_alias = f"{handle.submission_sender_owned_alias_local}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    # Unique per-run marker so the delivery assertion reads THIS message off
    # the session-scoped stub MX (same per-token pattern as the round-trip).
    token = f"sub-alias-{int(time.time() * 1000)}"

    body_lines = [
        f"From: Sender Alias <{owned_alias}>",
        f"To: {external_rcpt}",
        f"Subject: owned-alias submission {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Hello from the owned-alias submission test.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        # AUTH is the login identity; only the envelope sender is the alias.
        _smtp_auth_plain(conn, login, password, deadline)
        # MAIL FROM an address the actor OWNS but that is NOT the login handle:
        # accepted only because resolve_recipient maps it back to this actor.
        conn.cmd(f"MAIL FROM:<{owned_alias}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"the owned-alias submission did not complete end-to-end "
        f"(bridge log: {handle.log_file})"
    )


@pytest.mark.feature("standard-mail-apps")
def test_submission_mail_from_other_actor_rejected(mail_bridge_mta):
    """MAIL FROM an address owned by a DIFFERENT actor is rejected 550 5.7.1.

    The impersonation the ownership check exists to stop: the sender
    authenticates as its own identity, then tries to send as the recipient
    actor's address (`recipient@domain`, owned by a different actor).
    `Mail()` routes it through `assertMailFromOwned` (local-part ≠ the login
    handle), `resolve_recipient` resolves it to the recipient's actor_id ≠
    the authenticated sender, so the MTA rejects with 550 5.7.1 "Sender not
    authorized for this address" (`submission.go` `assertMailFromOwned`) —
    the address is local and syntactically fine, so this proves the
    *ownership* branch specifically, not the non-local-domain 553 guard.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    login = f"{handle.submission_sender_local}@{domain}"
    other_actor_addr = f"{handle.recipient_local_part}@{domain}"
    password = handle.submission_credential.decode()

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        _smtp_auth_plain(conn, login, password, deadline)
        # Not the authenticated actor's address → 550 5.7.1 at MAIL FROM.
        conn.cmd(f"MAIL FROM:<{other_actor_addr}>", "550", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("standard-mail-apps")
def test_submission_from_header_other_actor_rejected(mail_bridge_mta):
    """A `From:` HEADER naming an address owned by a DIFFERENT actor is
    rejected 550 5.7.1 at DATA, with the sender's OWN envelope.

    The header-side twin of `test_submission_mail_from_other_actor_rejected`
    (mail-multidomain.md § From: header ownership): the envelope passes as the
    login handle, so `Mail()` is satisfied; the RFC 5322 `From:` names the
    recipient actor's address instead. The signer would key DKIM `d=` on that
    header's domain and a receiver's DMARC would align it — so `Data()` holds
    the header to the same ownership predicate (`assertSenderOwned` →
    `resolve_recipient` against the **real nest** → the recipient's actor_id ≠
    the authenticated sender) and refuses before anything is signed, filed or
    enqueued. The 550 at the data terminator IS the witness: a refused
    submission enqueues nothing, and the Go unit tests pin that nothing is
    filed either — a timed "nothing reached the stub MX" wait would only add
    a wall-clock assertion (convention 14).
    """
    handle = mail_bridge_mta
    domain = handle.domain
    login = f"{handle.submission_sender_local}@{domain}"
    other_actor_addr = f"{handle.recipient_local_part}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    token = f"sub-from-forged-{int(time.time() * 1000)}"

    body_lines = [
        f"From: Not Really Them <{other_actor_addr}>",
        f"To: {external_rcpt}",
        f"Subject: forged From header {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "",
        "This must never leave.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        _smtp_auth_plain(conn, login, password, deadline)
        conn.cmd(f"MAIL FROM:<{login}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        # The header names another actor's address → 550 5.7.1 at DATA.
        conn.cmd(".", "550", deadline)
        conn.cmd("QUIT", "221", deadline)


# The DKIM-Signature tag parser moved to ``helpers.mail_wire.dkim_signature_tag``
# so the docker deploy round-trip tests share it (priority #1/#4 — one parser, not
# a copy per test). The local alias keeps every ``_dkim_sig_tag(...)`` call site
# below unchanged.
_dkim_sig_tag = dkim_signature_tag


@pytest.mark.feature("standard-mail-apps", "mail-server")
def test_submission_dkim_signature_verifies(mail_bridge_mta):
    """The outbound DKIM-Signature cryptographically VERIFIES against
    the selector's published public-key TXT — the in-CI proxy for Gmail's
    `dkim=pass` (mail-deployment-vps-critical-path.md Stage 4: send, DKIM-aligned).

    Closes the gap `test_submission_round_trip` defers (it asserts only that a
    DKIM-Signature header is *present*). Here an independent third-party verifier
    (dkimpy, never our `mail-auth` signer) re-derives the body hash + signature
    over the delivered bytes and checks them against the `public_dns_value` nest
    holds for the admin to publish at `<selector>._domainkey.<domain>`. A pass
    means the nest's mint → seal → open → sign chain produced a
    signature a real receiver accepts once that TXT is live (Stage 2 publishes it
    verbatim) — not merely that *some* header was prepended.

    The published value reaches the test via a dkimpy `dnsfunc`, so no real DNS
    query fires. Because dkimpy derives the query name from the signature's own
    `d=`/`s=` tags, a misaligned `d=` would query a name we don't answer and fail
    verify; we additionally assert `d=` equals the From: domain (DMARC alignment,
    single-domain bridge model — d= == From: == primary domain).

    smtp-server.md § Outbound delivery — DKIM signing pipeline (c=relaxed/relaxed).
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep (internal dev note)

    handle = mail_bridge_mta
    domain = handle.domain
    assert handle.dkim_public_dns_value, (
        "fixture read no DKIM record (handle.dkim_public_dns_value unset) — "
        "the verify test cannot run without the published TXT value"
    )
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    token = f"dkimverify-{int(time.time() * 1000)}"

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()

    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {external_rcpt}",
        f"Subject: DKIM verify {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Body the DKIM signature commits to.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"outbound delivery did not complete (bridge log: {handle.log_file})"
    )

    # Feed dkimpy the published TXT via a custom dnsfunc — no real DNS query
    # fires. dkimpy queries the name it derives from the signature's own d=/s=
    # tags; we answer ONLY for the aligned `<selector>._domainkey.<domain>.`
    # so a misaligned signature returns None → verify fails.
    expected_query = f"{handle.dkim_selector}._domainkey.{domain}.".encode()
    seen_queries: list[bytes] = []

    def dnsfunc(name, timeout=5):
        seen_queries.append(name)
        return handle.dkim_public_dns_value if name == expected_query else None

    verified = dkim.verify(received, dnsfunc=dnsfunc)
    assert verified, (
        "dkimpy rejected the DKIM-Signature — the delivered message "
        "would fail `dkim=pass` at a real receiver (Gmail). dnsfunc queries: "
        f"{seen_queries!r}. First 600 bytes:\n{received[:600]!r}"
    )
    assert expected_query in seen_queries, (
        f"dkimpy never queried {expected_query!r}; the signature's d=/s= are not "
        f"aligned to the provisioned selector. Queries: {seen_queries!r}"
    )

    # DMARC alignment: the d= signing domain must equal the From: header domain.
    d_tag = _dkim_sig_tag(received, "d")
    from_domain = sender.split("@", 1)[1]
    assert d_tag == from_domain, (
        f"DKIM d= ({d_tag!r}) is not aligned to the From: domain ({from_domain!r}) "
        "— DMARC would fail DKIM alignment"
    )


@pytest.mark.feature("mail-server")
def test_submission_dkim_per_domain_from_selects_second_domain_key(mail_bridge_mta):
    """Per-domain DKIM From-domain selection (mail-multidomain.md § Signing-key
    selection at outbound time, RFC 6376 §3.6): a deployment hosting TWO local
    domains, each with its OWN nest-held DKIM key, signs a submission with the
    key of the **From: header domain** — not the primary's.

    The nest holds one key per active mail_domains row and, at the outbound
    hand-out, signs with the key of the From: header domain. Here the authenticated sender submits with its
    From: header on the SECOND (non-primary) domain; the envelope MAIL FROM uses
    the same authed local-part on that domain (the submission MAIL-FROM gate
    requires localPart == authed identity and domain ∈ local_domains, not that
    the domain equals the authed identity's). An independent verifier (dkimpy)
    re-derives the signature against the SECOND domain's published key record at
    `default._domainkey.second.test` — a DISTINCT key from the primary's at
    `default._domainkey.fauna.test` — and only that name is answered, so a pass
    proves the nest selected the second domain's OWN key (not the primary's);
    `d=` asserts the domain alignment. Both domains use the conventional
    `default` selector label (no per-domain selector-setting RPC exists yet —
    deferred rotation track), so the per-domain distinction is the key + record,
    not the `s=` value. The single-domain twin
    (`test_submission_dkim_signature_verifies`) proves From=primary → primary's
    key — together they prove the set routes per From-domain, not one key for
    all domains.
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep (internal dev note)

    handle = mail_bridge_mta
    second_domain = handle.second_domain
    assert second_domain and handle.second_dkim_public_dns_value, (
        "fixture read no DKIM record for the second domain "
        "(handle.second_dkim_public_dns_value unset) — the per-domain verify "
        "test cannot run"
    )
    primary = handle.domain
    auth_local = handle.submission_sender_local
    # AUTH on the primary identity; From: + envelope on the SECOND domain. The
    # MAIL-FROM gate accepts <auth_local>@<any local domain>, and DKIM selects
    # on the From: header domain → the second domain's key must sign.
    auth_user = f"{auth_local}@{primary}"
    second_addr = f"{auth_local}@{second_domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    token = f"perdom-{int(time.time() * 1000)}"

    auth_plain = base64.b64encode(
        b"\x00" + auth_user.encode() + b"\x00" + password.encode()
    ).decode()

    body_lines = [
        f"From: Sender <{second_addr}>",
        f"To: {external_rcpt}",
        f"Subject: per-domain DKIM {token}",
        f"Message-ID: <{token}@{second_domain}>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Body the second domain's DKIM signature commits to.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    # TLS/SNI uses the primary domain (the bridge's single cert); the From: header
    # — not the TLS host — drives DKIM domain selection.
    with _connect_submission_tls(handle.submission_port_465, primary) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {primary}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{second_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"outbound delivery did not complete (bridge log: {handle.log_file})"
    )

    # dkimpy queries the name it derives from the signature's own d=/s= tags; we
    # answer ONLY for the SECOND domain's selector, so a signature that used the
    # primary's key (d=primary) queries a name we don't answer → verify fails.
    expected_query = (
        f"{handle.second_dkim_selector}._domainkey.{second_domain}.".encode()
    )
    seen_queries: list[bytes] = []

    def dnsfunc(name, timeout=5):
        seen_queries.append(name)
        return (
            handle.second_dkim_public_dns_value
            if name == expected_query
            else None
        )

    verified = dkim.verify(received, dnsfunc=dnsfunc)
    assert verified, (
        "dkimpy rejected the signature — the second domain was NOT signed with "
        "its own key (it would carry the primary's d=/s= or none). dnsfunc "
        f"queries: {seen_queries!r}. First 600 bytes:\n{received[:600]!r}"
    )
    assert expected_query in seen_queries, (
        f"dkimpy never queried {expected_query!r}; the signature's d=/s= are not "
        f"aligned to the second domain's selector. Queries: {seen_queries!r}"
    )

    # The d=/s= tags must name the SECOND domain + its selector — not the primary.
    d_tag = dkim_signature_tag(received, "d")
    s_tag = dkim_signature_tag(received, "s")
    assert d_tag == second_domain, (
        f"DKIM d= ({d_tag!r}) is not the From: header domain ({second_domain!r}) "
        "— per-domain selection signed with the wrong domain's key"
    )
    assert s_tag == handle.second_dkim_selector, (
        f"DKIM s= ({s_tag!r}) is not the second domain's selector "
        f"({handle.second_dkim_selector!r})"
    )


def _submit_dkim_signable(
    handle, *, sni_host, from_addr, auth_plain, external_rcpt, token,
):
    """Submit one DKIM-signable message From ``from_addr`` over implicit-TLS
    submission (465) and return the bytes the stub external MX received for
    ``token`` (``None`` if it didn't arrive in 15 s).

    SNI uses the bridge's single cert host (``sni_host`` = the primary domain);
    the From: header — not the TLS host — drives the nest's per-domain DKIM
    selection (RFC 6376 §3.6), so a From on a non-primary local domain is signed
    with that domain's own key. Same per-token capture pattern as the other
    outbound tests' ``_wait_for_tagged``.
    """
    body = "\r\n".join([
        f"From: Sender <{from_addr}>",
        f"To: {external_rcpt}",
        f"Subject: force-rotate DKIM {token}",
        f"Message-ID: <{token}@{from_addr.split('@', 1)[1]}>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Body the rotated DKIM signature commits to.",
    ]) + "\r\n"
    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, sni_host) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {sni_host}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{from_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)
    return _wait_for_tagged(handle.stub_mx, token, timeout=15.0)


def _wait_until_signed_with_selector(
    handle, *, sni_host, from_addr, auth_plain, external_rcpt, selector,
    timeout=45.0,
):
    """Poll-submit From ``from_addr`` until the delivered message's DKIM
    signature carries ``s=<selector>``; ``pytest.fail`` after ``timeout``.

    The bridge applies a ``config_changed`` push (a freshly-added local domain)
    asynchronously, so this absorbs the brief propagation window: during it a
    submission can be rejected (the new domain isn't yet in ``local_domains`` →
    MAIL FROM ``550``, an ``AssertionError``) — retried, as is a message signed
    with the previous selector. The assertion is on the observable end-state
    (the signature's ``s=`` tag, which the nest stamps at the outbound
    hand-out), not any log line, so it stays decoupled from internals.
    """
    deadline = time.monotonic() + timeout
    last = "<no message delivered>"
    attempt = 0
    while time.monotonic() < deadline:
        attempt += 1
        token = f"rotate-{selector}-{attempt}-{secrets.token_hex(4)}"
        try:
            received = _submit_dkim_signable(
                handle, sni_host=sni_host, from_addr=from_addr,
                auth_plain=auth_plain, external_rcpt=external_rcpt, token=token,
            )
        except (AssertionError, OSError) as exc:
            last = f"submission rejected (propagation window): {exc}"
            time.sleep(1.0)
            continue
        if received is None:
            last = "message not delivered to stub MX within 15s"
            time.sleep(1.0)
            continue
        s = dkim_signature_tag(received, "s")
        if s == selector:
            return received
        last = f"signed s={s!r} (waiting for s={selector!r})"
        time.sleep(1.0)
    pytest.fail(
        f"no delivered From:{from_addr} message was signed s={selector} within "
        f"{timeout:.0f}s — the nest did not sign with the domain's active "
        f"selector at the hand-out (last: {last}). Bridge log: {handle.log_file}"
    )


def _assert_dkim_selector(received, domain, selector, public_dns_value, dkim_mod):
    """dkimpy-verify ``received`` answering ONLY the ``(domain, selector)``
    record, then assert its d=/s= tags name that domain + selector.

    Answering only this one name means a signature under any other
    selector/domain queries a name the ``dnsfunc`` returns ``None`` for and
    fails to verify — so a pass proves the signature used exactly this key, not
    merely that *some* valid signature is present.
    """
    expected_query = f"{selector}._domainkey.{domain}.".encode()
    seen = []

    def dnsfunc(name, timeout=5):
        seen.append(name)
        return public_dns_value if name == expected_query else None

    verified = dkim_mod.verify(received, dnsfunc=dnsfunc)
    assert verified, (
        f"dkimpy rejected the signature for s={selector} d={domain} — the "
        f"nest did not sign with the {selector!r} key. dnsfunc queries: "
        f"{seen!r}. First 600 bytes:\n{received[:600]!r}"
    )
    assert expected_query in seen, (
        f"dkimpy never queried {expected_query!r}; the signature's d=/s= are "
        f"not aligned to {selector}._domainkey.{domain}. Queries: {seen!r}"
    )
    d_tag = dkim_signature_tag(received, "d")
    s_tag = dkim_signature_tag(received, "s")
    assert d_tag == domain, (
        f"DKIM d= ({d_tag!r}) is not the From: header domain ({domain!r})"
    )
    assert s_tag == selector, (
        f"DKIM s= ({s_tag!r}) is not the active selector ({selector!r})"
    )


def _wait_for_minted_selector(admin_ws_factory, domain, expected_selector, timeout=45.0):
    """Poll ``fauna.bridges.list_dkim_selectors(domain)`` until the nest-minted
    ``expected_selector`` appears with a populated Ed25519 public DNS value;
    return that value. ``pytest.fail`` after ``timeout``.

    The scheduled rotation-mint (``run_scheduled_dkim_rotation_mint``) runs on
    the nest's background-expiry tick, accelerated to a few seconds in tests via
    ``FAUNA_EXPIRY_INTERVAL_SECS`` (``tests/common/nest.py``), so this absorbs the
    up-to-one-tick wait without coupling to the cadence. The assertion is on the
    observable end-state (the selector listed by nest), not any log line.
    """
    deadline = time.monotonic() + timeout
    seen: list[str] = []
    while time.monotonic() < deadline:
        with admin_ws_factory() as a:
            reply = a.call("fauna.bridges.list_dkim_selectors", {"domain": domain})
        selectors = {s["selector"]: s for s in reply.get("selectors", [])}
        seen = sorted(selectors)
        info = selectors.get(expected_selector)
        if info and info.get("public_dns_value", "").startswith("v=DKIM1; k=ed25519;"):
            return info["public_dns_value"]
        time.sleep(1.0)
    pytest.fail(
        f"nest never minted DKIM selector {expected_selector!r} for {domain} "
        f"within {timeout:.0f}s (saw selectors {seen!r}) — "
        f"run_scheduled_dkim_rotation_mint did not provision the due domain's "
        f"rotation key"
    )


@pytest.mark.feature("mail-server")
def test_force_rotate_dkim_flips_active_selector_live(mail_bridge_mta, nest_instance):
    """DKIM rotation capstone (mail-multidomain.md § Rotation — "Emergency
    rotation" + § Signing-key selection): ``fauna.bridges.force_rotate_dkim``
    flips a domain's **active** selector and the next outbound leaves signed
    ``s=<new>`` with **no restart** — nest's active-selector flip
    (``force_rotate_dkim_handler`` → ``mail_domains.dkim_selector`` +
    ``config_changed``) composed with the nest's signing at the outbound
    hand-out.

    Uses a DEDICATED ``rotate.test`` domain rather than the session's
    primary/second domains: ``mail_bridge_mta`` is session-scoped (one nest +
    one bridge shared across the file), and an active-selector flip is a
    PERSISTENT nest-state mutation, so flipping the primary would break every
    sibling test asserting its ``s=default`` (``test_submission_dkim_signature_
    verifies`` et al.). ``rotate.test`` is touched by no other test, keeping the
    flip order-independent.

    Flow — a genuine live flip, default → <YYYYMM>:
      1. ``add_local_domain(rotate.test)`` — the nest mints the domain's
         ``default`` key. The running bridge hot-applies the new domain (no
         restart) and From:rotate.test leaves signed ``s=default``, dkimpy-
         verified against the record the nest publishes for it.
      2. ``set_dkim_rotation_days(rotate.test, 0)`` makes the domain due, the
         nest mints a SECOND key under this month's ``<YYYYMM>`` selector, then
         ``force_rotate_dkim(rotate.test)`` flips the active selector to that
         newest one (skipping the 24 h peer-cache wait) and pushes
         ``config_changed``.
      3. The next From:rotate.test outbound leaves signed ``s=<YYYYMM>`` —
         dkimpy-verified against the new record. A DISTINCT ``s=`` value flips
         LIVE under a running bridge, and the same test holds both sides of the
         flip.
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep (internal dev note)
    from conftest import _published_dkim_record
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    primary = handle.domain  # the bridge's single TLS cert host → SNI
    rotate_domain = "rotate.test"
    old_selector = "default"
    # The nest mints `<YYYYMM>` for the current UTC month — the exact shape of the
    # nest-side `dkim_rotation_selector(now)` (`format!("{year:04}{month:02}")`).
    new_selector = time.strftime("%Y%m", time.gmtime())

    # AUTH on the primary identity; From: + envelope on rotate.test. The
    # submission MAIL-FROM gate accepts <auth_local>@<any local domain>, and
    # DKIM selects on the From: header domain → rotate.test's active key signs.
    auth_local = handle.submission_sender_local
    auth_user = f"{auth_local}@{primary}"
    password = handle.submission_credential.decode()
    auth_plain = base64.b64encode(
        b"\x00" + auth_user.encode() + b"\x00" + password.encode()
    ).decode()
    from_addr = f"{auth_local}@{rotate_domain}"
    external_rcpt = "recipient@external.test"

    admin = nest_instance["admin"]

    def _admin_ws():
        # A fresh, short-lived Admin WS-RPC client per call-burst (no long idle
        # while the retry-submit loops run — matches the fixture's per-use
        # client construction).
        return WsRpcAdminClient(
            handle.nest_url,
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )

    # ── BEFORE: add the domain (the nest mints its default key), prove s=default ──
    with _admin_ws() as a:
        a.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": rotate_domain,
                "mta_sts_cert_mode": "per_host",
            },
        )
    old_dns = _published_dkim_record(nest_instance, rotate_domain, old_selector)
    received_before = _wait_until_signed_with_selector(
        handle, sni_host=primary, from_addr=from_addr, auth_plain=auth_plain,
        external_rcpt=external_rcpt, selector=old_selector,
    )
    _assert_dkim_selector(received_before, rotate_domain, old_selector, old_dns, dkim)

    # ── FLIP: the nest mints the second selector's key, then force_rotate ──
    with _admin_ws() as a:
        # 0 days = always due → the next mint tick acts immediately.
        a.call(
            "fauna.bridges.set_dkim_rotation_days",
            {"domain": rotate_domain, "rotation_days": 0},
        )
    new_dns = _wait_for_minted_selector(_admin_ws, rotate_domain, new_selector)
    assert new_dns != old_dns, (
        f"the nest published the same DKIM record for {new_selector!r} as for "
        f"{old_selector!r} — a rotation must mint a distinct key"
    )
    with _admin_ws() as a:
        a.force_rotate_dkim(rotate_domain)

    # ── AFTER: the next outbound is signed s=<YYYYMM>, no restart ──
    received_after = _wait_until_signed_with_selector(
        handle, sni_host=primary, from_addr=from_addr, auth_plain=auth_plain,
        external_rcpt=external_rcpt, selector=new_selector,
    )
    _assert_dkim_selector(received_after, rotate_domain, new_selector, new_dns, dkim)


@pytest.mark.feature("mail-server")
def test_scheduled_rotation_mints_new_selector_nestside(mail_bridge_mta, nest_instance):
    """Nest-side scheduled DKIM rotation (mail-multidomain.md § Rotation —
    "Scheduled rotation" + § Implementation status "Open follow-on"): a
    ``dkim_rotation_due`` domain gets a fresh ``<YYYYMM>`` selector **minted
    nest-side** (``run_scheduled_dkim_rotation_mint``) and held by the nest, and
    the active selector then flips onto the nest-minted key and outbound mail
    leaves signed ``s=<YYYYMM>`` — no client, no admin action mints the key. The
    domain starts on the ``default`` key the nest minted when it was added.

    The producer half of ``test_force_rotate_dkim_flips_active_selector_live``:
    that test holds both sides of the flip to a verified signature; this one
    pins the MINT — the due tick produces this month's selector, never
    ``default``, on a domain with no rotation in flight.

    Dedicated ``mintrot.test`` domain (``mail_bridge_mta`` is session-scoped and an
    active-selector flip is persistent state — same isolation rationale as the
    capstone's ``rotate.test``). Flow:
      1. ``add_local_domain(mintrot.test)`` — the nest mints its ``default`` key
         (baseline newest == active == ``default``, so the mint's "no rotation in
         flight" gate fires).
      2. ``set_dkim_rotation_days(mintrot.test, 0)`` — the per-domain accelerated-
         rotation lever (§ Selector) — makes the domain due immediately.
      3. The background rotation-mint tick (accelerated via ``FAUNA_EXPIRY_INTERVAL_
         SECS``) mints a fresh nest-held ``<YYYYMM>`` Ed25519 key. Poll
         ``list_dkim_selectors`` until it appears (assert it's this month's
         ``<YYYYMM>``, not ``default``).
      4. ``force_rotate_dkim`` flips onto the nest-minted selector (skipping the
         24 h warmup, as the capstone does) and the next outbound leaves signed
         ``s=<YYYYMM>`` — dkimpy-verified against the MINTED record. The
         signature verifying proves the nest signs with the key whose public
         half it publishes.
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep (internal dev note)
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    primary = handle.domain  # the bridge's single TLS cert host → SNI
    rotate_domain = "mintrot.test"
    # The nest mints `<YYYYMM>` for the current UTC month — the exact shape of the
    # nest-side `dkim_rotation_selector(now)` (`format!("{year:04}{month:02}")`).
    expected_selector = time.strftime("%Y%m", time.gmtime())

    auth_local = handle.submission_sender_local
    auth_user = f"{auth_local}@{primary}"
    password = handle.submission_credential.decode()
    auth_plain = base64.b64encode(
        b"\x00" + auth_user.encode() + b"\x00" + password.encode()
    ).decode()
    from_addr = f"{auth_local}@{rotate_domain}"
    external_rcpt = "recipient@external.test"

    admin = nest_instance["admin"]

    def _admin_ws():
        return WsRpcAdminClient(
            handle.nest_url,
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )

    # ── BEFORE: add the domain (nest-held default key); make it rotation-due ──
    with _admin_ws() as a:
        a.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": rotate_domain,
                "mta_sts_cert_mode": "per_host",
            },
        )
        # 0 days = always due → the next mint tick acts immediately.
        a.call(
            "fauna.bridges.set_dkim_rotation_days",
            {"domain": rotate_domain, "rotation_days": 0},
        )

    # ── MINT: the nest provisions the fresh <YYYYMM> key on its scheduled tick ──
    minted_dns = _wait_for_minted_selector(_admin_ws, rotate_domain, expected_selector)
    assert expected_selector != "default"

    # ── FLIP + VERIFY: force_rotate onto the nest-minted key; signed s=<YYYYMM> ──
    with _admin_ws() as a:
        a.force_rotate_dkim(rotate_domain)
    received = _wait_until_signed_with_selector(
        handle, sni_host=primary, from_addr=from_addr, auth_plain=auth_plain,
        external_rcpt=external_rcpt, selector=expected_selector,
    )
    _assert_dkim_selector(received, rotate_domain, expected_selector, minted_dns, dkim)


@pytest.mark.feature("mail-server")
def test_submission_multipart_dkim_verifies(mail_bridge_mta):
    """A multipart/mixed message with a base64 attachment is delivered and its
    DKIM signature still cryptographically verifies + aligns.

    `test_submission_dkim_signature_verifies` proves the crypto verify over a
    trivial single-part text/plain body; a real send to Gmail (Stage 5) is
    multipart with attachments. The DKIM body-hash is computed over the *whole*
    body (boundaries, nested part headers, the multi-line base64, the closing
    delimiter) under relaxed canonicalization, so this is the proof that the
    strip → sign → enqueue → outbound-worker → MX-deliver → stub-capture path
    preserves the body byte-for-byte through realistic MIME — i.e. a multipart
    Gmail send won't fail DKIM at Stage 5 for a body-mangling reason. Same
    fixture + inline-dnsfunc verification shape as the single-part test above.
    """
    import dkim  # dkimpy — independent verifier; fleet venv dep (internal dev note)

    handle = mail_bridge_mta
    domain = handle.domain
    assert handle.dkim_public_dns_value, (
        "fixture read no DKIM record (handle.dkim_public_dns_value unset) — "
        "the verify test cannot run without the published TXT value"
    )
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"
    token = f"mpart-{int(time.time() * 1000)}"

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()

    # A multi-line base64 attachment payload (64-char lines, CRLF-joined — the
    # SMTP wire is CRLF and the DKIM body-hash is CRLF-sensitive).
    raw_attachment = bytes(range(256)) * 3  # 768 bytes → several b64 lines
    b64 = base64.b64encode(raw_attachment).decode()
    b64_lines = [b64[i : i + 64] for i in range(0, len(b64), 64)]
    boundary = "==fauna-e2e-mpart-boundary=="

    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {external_rcpt}",
        f"Subject: multipart dkim {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        f'Content-Type: multipart/mixed; boundary="{boundary}"',
        "",
        f"--{boundary}",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Multipart body with an attachment; the DKIM body-hash must survive.",
        f"--{boundary}",
        'Content-Type: application/octet-stream; name="payload.bin"',
        "Content-Transfer-Encoding: base64",
        'Content-Disposition: attachment; filename="payload.bin"',
        "",
        *b64_lines,
        f"--{boundary}--",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"outbound delivery did not complete (bridge log: {handle.log_file})"
    )
    # The attachment must survive intact (a mangled body also fails the DKIM
    # body-hash, but assert it directly for a clearer diagnosis).
    assert b64_lines[0].encode() in received, (
        "delivered multipart message lost its base64 attachment payload — the "
        f"body was altered in transit. First 800 bytes:\n{received[:800]!r}"
    )

    expected_query = f"{handle.dkim_selector}._domainkey.{domain}.".encode()
    seen_queries: list[bytes] = []

    def dnsfunc(name, timeout=5):
        seen_queries.append(name)
        return handle.dkim_public_dns_value if name == expected_query else None

    verified = dkim.verify(received, dnsfunc=dnsfunc)
    assert verified, (
        "dkimpy rejected the multipart message's DKIM-Signature — the body-hash "
        "over the multipart body did not match, i.e. the body was mangled "
        f"between signing and delivery. dnsfunc queries: {seen_queries!r}. "
        f"First 800 bytes:\n{received[:800]!r}"
    )
    d_tag = _dkim_sig_tag(received, "d")
    from_domain = sender.split("@", 1)[1]
    assert d_tag == from_domain, (
        f"DKIM d= ({d_tag!r}) is not aligned to the From: domain ({from_domain!r})."
    )


@pytest.mark.feature("standard-mail-apps")
def test_submission_strips_received_headers(mail_bridge_mta):
    """Internal `Received:` headers are stripped before the message reaches
    the external MX (smtp-server.md § Outbound delivery).

    Full-stack tier_3 proof of the privacy invariant: a submitted message
    carrying `Received:` headers (an internal hostname + IP, plus a folded
    continuation line) is DKIM-signed and delivered to the stub external MX
    with every whole `Received:` header gone — the submitter's IP and our
    internal hostnames never reach the recipient. Look-alike headers
    (`X-Received-By`, `Received-SPF`) survive: only whole `Received:` fields
    are stripped, not substring matches. The strip is the shared-Rust pure
    fn (`fauna_mail::outbound::received_strip`) over UniFFI, called in the
    Go submission `Data` hook before signing.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    external_rcpt = "recipient@external.test"

    # Unique per-run marker so the assertion reads THIS message off the
    # session-scoped stub MX, not an earlier test's (see round-trip note).
    token = f"strip-{int(time.time() * 1000)}"

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()

    # Two leaky Received headers (the second with an RFC 5322 §2.2.3
    # continuation line) plus two look-alike headers that must NOT be
    # stripped.
    body_lines = [
        "Received: from sketchy.internal (10.0.0.5) by mua.example.test",
        "Received: from mua.local",
        "\tby relay.internal with ESMTP id abc123",
        "X-Received-By: keepme",
        "Received-SPF: pass",
        f"From: Sender <{sender}>",
        f"To: {external_rcpt}",
        f"Subject: strip-received e2e {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Internal Received headers must not reach the recipient.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — "
        f"outbound delivery did not complete (bridge log: {handle.log_file})"
    )
    for leak in (b"sketchy.internal", b"10.0.0.5", b"mua.local", b"relay.internal"):
        assert leak not in received, (
            f"delivered message still leaks {leak!r} — internal Received: header "
            f"not stripped. First 600 bytes:\n{received[:600]!r}"
        )
    for keep in (b"X-Received-By: keepme", b"Received-SPF: pass"):
        assert keep in received, (
            f"delivered message dropped {keep!r} — over-stripped (substring match). "
            f"First 600 bytes:\n{received[:600]!r}"
        )


@pytest.mark.feature("standard-mail-apps")
def test_submission_partial_local_failure_delivers_rest(mail_bridge_mta):
    """One invalid local recipient is rejected at RCPT TO (550) and the
    message still delivers to every accepted recipient.

    Per the nest mail-bridge recipient-routing work, local recipients are resolved at RCPT TO via
    the real nest `validate_recipient` (smtp-server.md § Recipient handling
    on submission). In one transaction we RCPT a valid local (the sender's
    own provisioned address → 250), an unknown local (`ghost@domain`, which
    nest rejects → 550 5.1.1 on *that* RCPT only), and an external recipient
    (250). DATA returns 250 and the external message reaches the stub MX —
    proving the bad local did not short-circuit the whole DATA. This is the
    tier_3 wire proof that complements the Go-unit coverage in
    `internal/mta/submission_test.go`.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    ghost = f"ghost-{int(time.time() * 1000)}@{domain}"
    external_rcpt = "recipient@external.test"
    # Unique per-run marker so the assertion reads THIS message off the
    # session-scoped stub MX, not an earlier test's (see round-trip note).
    token = f"part-{int(time.time() * 1000)}"

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()

    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {sender}, {ghost}, {external_rcpt}",
        f"Subject: partial-failure delivers the rest {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "One of these recipients is bogus; the others must still go through.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        # Valid local (the sender's own provisioned address) → accepted.
        conn.cmd(f"RCPT TO:<{sender}>", "250", deadline)
        # Unknown local → rejected at RCPT TO with 550 5.1.1, this RCPT only.
        conn.cmd(f"RCPT TO:<{ghost}>", "550", deadline)
        # External → accepted (existence unknowable at submission time).
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        # DATA still succeeds and delivers to the accepted recipients.
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 15s — the "
        f"bad local RCPT wrongly short-circuited the whole DATA (bridge log: "
        f"{handle.log_file})"
    )


@pytest.mark.feature("mail-server")
def test_outbound_bounce_on_bad_external_recipient(mail_bridge_mta, nest_instance):
    """A submission to an external recipient the remote MX permanently
    rejects (550) produces an RFC 3464 NDR bounce back to the sender — and
    because the submission sender is an in-domain actor, that bounce is SEALED
    into the sender's local INBOX, never relayed back out over the MX.

    T1.2 — `smtp-server.md` § Permanent-failure bounce generation. The stub
    external MX 550-rejects any `nonexistent*@external.test` recipient at
    RCPT TO; the bridge's outbound worker classifies the 5xx as permanent
    (`outbound.go:classify`) and calls `mark_outbound_bounced`. nest's handler
    (`outbound_bounce::generate_permfail_bounce`) builds the DSN and submits it
    null-sender back to the original sender through the `submit_outbound`
    chokepoint. The submission sender is always an in-domain actor
    (`sender@<domain>` — a submission is authenticated), so `submit_outbound`'s
    in-domain partition SEALS the DSN straight into the sender's sealed INBOX
    instead of MX-relaying it — the "in-domain direct-enqueue self-loop" fix
    (2026-06-06): routing a bounce-to-self through the MX would
    hairpin and 554-bounce at the inbound HELO-identity check on a containerized
    deploy. This test read the DSN off the external wire until that fix moved the
    sink to the local INBOX; it now asserts the sealed local delivery.

    We assert (1) the sender's INBOX gains exactly one row — the sealed DSN (the
    sender's own "Sent" copy lands in the `Sent` mailbox, so querying
    `mailbox='INBOX'` isolates the bounce); and (2) NO `multipart/report`
    carrying this message's token is relayed out to the stub MX — the regression
    guard for the self-loop hairpin. The DSN *body* shape (multipart/report,
    Status: 5.1.1, addressed to the sender) is covered by the Rust units
    `outbound_bounce::tests::{clean_permfail_enqueues_dsn_and_records_bounce,
    dsn_to_in_domain_sender_delivers_locally_no_relay}`; the sealed copy here is
    ciphertext, so we assert the cross-binary seal-and-deliver integration those
    units cannot reach.

    Full-stack tier_3 via the production Go-bridge path. (The former in-nest
    `/api/v1/test/outbound/*` `/drain` orchestration was deleted in the I6
    cutover; the production Go-bridge path is now the only outbound send path.)
    """
    handle = mail_bridge_mta
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    bad_rcpt = "nonexistent@external.test"
    sender_actor = handle.submission_sender_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]

    def _sender_inbox_count():
        """Rows in the sender actor's sealed INBOX. The sender's own "Sent"
        copy is sealed into the `Sent` mailbox, so scoping to `INBOX` isolates
        the inbound bounce (`seal_and_ingest_local` delivers a to-sender DSN to
        INBOX; `seal_and_store_sent_copy` delivers the Sent copy to `Sent`)."""
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute(
                "SELECT COUNT(*) FROM bridge_imap_messages"
                " WHERE actor_id = ? AND mailbox = 'INBOX'",
                (sender_actor,),
            ).fetchone()
            return n
        finally:
            conn.close()

    before_inbox = _sender_inbox_count()
    # Queue high-water mark before the trigger — the relay guard below scopes to
    # rows this test could have caused (the fixture is session-scoped).
    before_queue_id = _max_queue_id(handle.nest_url)

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()
    token = f"t12bounce-{int(time.time() * 1000)}"
    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {bad_rcpt}",
        "Subject: T1.2 bounce on bad external recipient",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This message is addressed to a recipient the remote MX rejects.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        # Accepted at submission time — recipient existence is unknowable
        # until the outbound worker reaches the remote MX.
        conn.cmd(f"RCPT TO:<{bad_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # Outbound attempt 1 → stub 550s the bad recipient → bridge classifies the
    # 5xx permanent, bounces, and calls mark_outbound_bounced. nest's handler
    # then SEALS the DSN synchronously into the in-domain sender's INBOX (via the
    # submit_outbound in-domain partition — no MX relay, no outbound-queue poll
    # cycle), so it lands within a beat of the bounce, not on the 30 s poll.
    inbox_after = before_inbox
    poll_deadline = time.monotonic() + 30.0
    while time.monotonic() < poll_deadline:
        inbox_after = _sender_inbox_count()
        if inbox_after >= before_inbox + 1:
            break
        time.sleep(0.25)
    assert inbox_after == before_inbox + 1, (
        "the permanent-failure DSN was not sealed into the sender's INBOX within "
        "30s — nest's mark_outbound_bounced handler did not deliver the bounce "
        f"locally (sender INBOX rows {before_inbox} → {inbox_after}; bridge log: "
        f"{handle.log_file})"
    )

    # Regression guard for the in-domain self-loop hairpin: this bounce to an
    # in-domain sender must NOT be relayed back out over the MX (it would
    # 554-bounce at the inbound HELO-identity check on a containerized deploy).
    #
    # Asserted on the ENQUEUE side, anchored to the original row going terminal:
    # `generate_to_sender_ndr` calls `submit_outbound` — the one place the DSN
    # could be enqueued for relay — and only then marks the row bounced, so a
    # terminal row proves the partition decision has already been made. See
    # `_await_bounce_decided`. This replaced a `sleep(2.0)` that could not have
    # caught the regression anyway (a relayed DSN needs a drain cycle, and the
    # outbound worker's `PollInterval` is 30 s).
    bounced = _await_bounce_decided(
        handle.nest_url, msgid=f"<{token}@{domain}>", recipient=bad_rcpt
    )
    assert bounced["status"] == "bounced", (
        "the row should end `bounced` (the stub 550s the recipient), not "
        f"{bounced['status']!r} — reason: {bounced['last_error']!r}"
    )
    relayed = _hairpinned_dsn_rows(handle.nest_url, before_queue_id, domain)
    assert relayed == [], (
        "regression: this bounce's DSN was ENQUEUED for MX relay instead of "
        "being sealed into the sender's INBOX (the in-domain self-loop hairpin "
        f"is back) — null-sender rows enqueued by this test: "
        f"{[(r['original_msgid'], r['recipient']) for r in relayed]}"
    )

    # The delivery-side twin of the same guard, now free of any wait. The
    # session-scoped stub accumulates other tests' wire traffic, so scope it to
    # THIS message's token (the DSN's message/rfc822-headers part carries the
    # original Message-ID).
    for msg in handle.stub_mx.messages():
        if token.encode() in msg:
            assert b"multipart/report" not in msg.lower(), (
                "regression: this bounce's DSN was relayed out over the MX (the "
                "in-domain self-loop hairpin is back) — it must be sealed locally "
                f"into the sender's INBOX instead:\n{msg[:400]!r}"
            )


def _advance_outbound_clock(nest_url: str, advance_seconds: int) -> None:
    """Fast-forward nest's outbound retry clock past a boundary.

    Hits `POST /api/v1/test/outbound/clock` (gated on `--features test-hooks`
    alone, which `build_node()` enables), freezing the production outbound
    path's `AppState::outbound_now()` at `now + advance_seconds`. Lets the
    test cross the 4 h delay-warning mark without waiting hours.
    """
    body = json.dumps({"advance": advance_seconds}).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/clock",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"outbound clock hook returned {resp.status}"




def _outbound_queue(nest_url: str) -> list:
    """Snapshot nest's `outbound_mail_queue` rows (any status), ordered by id.

    `GET /api/v1/test/outbound/queue`, same `test-hooks` gate. Projects
    scheduling state only — `attempt_count`, `next_attempt_at`,
    `delay_warned_at`, `status` — never message bodies.
    """
    req = urllib.request.Request(f"{nest_url}/api/v1/test/outbound/queue", method="GET")
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"outbound queue hook returned {resp.status}"
        return json.loads(resp.read().decode())["rows"]


def _attempts_for(nest_url: str, recipient: str):
    """The `attempt_count` of the queued row addressed to `recipient`, or None."""
    for row in _outbound_queue(nest_url):
        if row["recipient"] == recipient:
            return row["attempt_count"]
    return None


def _msgid_key(msgid: str) -> str:
    """A Message-ID normalized for comparison against `original_msgid`.

    ⚠ The queue stores the two provenances of a msgid differently, and the
    difference is invisible until an assertion silently never matches: a msgid
    **parsed from a message header** is stored bare (`t12bounce-…@fauna.test`),
    while one nest **mints itself** keeps its angle brackets (`<delay-…>`,
    `<bounce-…>` — see `_count_delay_warning_rows`). Callers hold the header
    form, so normalizing both sides is what makes a lookup work for either.
    """
    return msgid.strip().lstrip("<").rstrip(">")


def _await_bounce_decided(nest_url: str, msgid: str, recipient: str) -> dict:
    """Barrier: wait until the queue row for `(msgid, recipient)` is terminal.

    The causal anchor every "the DSN must NOT be relayed out" guard rests on
    (`e2e-conventions.md` § convention 14 — negative asserts anchor to causal
    order, never to a settle window). nest's `mark_outbound_bounced` handler
    runs the **entire** NDR generation before it flips the row off `pending`,
    and `submit_outbound` — the single chokepoint where a DSN could ever be
    enqueued for MX relay — is inside that span:

      * to-sender NDR: `submit_outbound` (`outbound_bounce.rs:174-200`) then
        `mark_outbound_bounced_with_reason` (`:206`);
      * forwarder NDR: `seal_and_ingest_local` (`:321-337`) then
        `mark_outbound_bounced_with_reason` (`:343`);
      * both of the handler's own fallback arms mark the row bounced too
        (`bridge_routing_handlers.rs:6300-6330`), so the status leaves
        `pending` on every path.

    So a row observed terminal proves that message's relay decision has already
    been made, and any row it would have enqueued is **already in the queue**.
    "No DSN was relayed out" becomes a statement about state rather than about
    elapsed time — and it is strictly stronger than reading the stub MX, which
    could only see a relayed DSN after a further outbound drain cycle (the
    worker's `PollInterval` is 30 s, so the 2 s settle windows this replaced
    could not reliably have caught the regression at all).

    Returns the terminal row, so a caller can assert on the status it reached.
    """
    return wait_until(
        lambda: next(
            (
                row
                for row in _outbound_queue(nest_url)
                if _msgid_key(row["original_msgid"]) == _msgid_key(msgid)
                and row["recipient"] == recipient
                and row["status"] != "pending"
            ),
            None,
        ),
        MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: (
            f"no terminal outbound row for msgid={msgid!r} recipient={recipient!r}; "
            f"queue={[(r['original_msgid'], r['recipient'], r['status']) for r in _outbound_queue(nest_url)]}"
        ),
    )


def _hairpinned_dsn_rows(nest_url: str, since_id: int, domain: str) -> list:
    """Rows enqueued after `since_id` that are self-generated mail relayed back
    into OUR OWN domain — i.e. the self-loop hairpin, in any of its shapes.

    Three properties make this the precise signature, all read off the
    projection:

      * **null envelope sender** — a DSN is null-sender by RFC 5321 §4.5.5, and
        nest builds every one that way (`outbound_bounce.rs:178`).
      * **recipient in our primary domain** — the old forwarder design
        addressed the NDR to `SRS0=…@<primary-domain>`, and the in-domain
        self-loop addresses the local sender directly; both land here.
      * **on `outbound_mail_queue` at all** — a queue row is by construction one
        `submit_outbound` chose to **relay**, since its in-domain partition
        delivers locally and enqueues nothing
        (`bridge_routing_handlers.rs:6553-6591`).

    `<delay-…>` msgids are excluded: a delay warning is also null-sender, and
    the outbound worker may legitimately emit one for a *sibling* test's row
    while this test's window is open (`_count_delay_warning_rows` is its own
    assertion elsewhere in this file).
    """
    suffix = f"@{domain}".lower()
    return [
        row
        for row in _outbound_queue(nest_url)
        if row["id"] > since_id
        and row["original_sender"] == ""
        and not _msgid_key(row["original_msgid"]).startswith("delay-")
        and row["recipient"].lower().endswith(suffix)
    ]


def _max_queue_id(nest_url: str) -> int:
    """Highest `outbound_mail_queue` row id right now, or 0 on an empty queue.

    The session-scoped fixture accumulates other tests' rows, so a guard scopes
    to rows this test could have caused by taking this reading first.
    """
    return max((row["id"] for row in _outbound_queue(nest_url)), default=0)


def _count_delay_warning_rows(nest_url: str, sender: str) -> int:
    """Delay-warning DSN rows nest has ENQUEUED back to `sender`.

    The enqueue side of the once-per-message gate: `enqueue_delay_warning`
    inserts a null-envelope-sender row whose `original_msgid` is minted as
    `<delay-{now}-{row id}@{mta}>`. Counting these sees a duplicate warning the
    moment nest decides to emit it, without waiting for the extra drain cycle
    its delivery to the stub MX would need.
    """
    return sum(
        1
        for row in _outbound_queue(nest_url)
        if row["original_msgid"].startswith("<delay-") and row["recipient"] == sender
    )


def _count_delayed(stub_mx) -> int:
    return sum(1 for m in stub_mx.messages() if b"action: delayed" in m.lower())


@pytest.mark.feature("mail-server")
def test_outbound_delay_warning_emitted_once(mail_bridge_mta):
    """A submission to a recipient whose MX keeps 4xx-tempfailing produces
    exactly one RFC 3464 `Action: delayed` / `Status: 4.4.7` delay-warning
    DSN back to the sender once delivery has been pending past the 4 h mark.

    T1.3 — `smtp-server.md` § Retry schedule (:374): "A delay warning is
    generated at the 4-hour mark per recipient … per-recipient + once-per-
    message". nest (not the bridge) owns the retry curve and emits the
    warning on the production Go-bridge → `mark_outbound_failed` path. The
    stub external MX 451-rejects `tempfail*@external.test` at RCPT TO, so the
    worker classifies the failure as transient and reports
    `mark_outbound_failed`; nest applies the curve and, once
    `outbound_now() - created_at >= 4h` (fast-forwarded via the `test-hooks`
    clock endpoint), enqueues the delayed DSN null-sender back to the sender.
    The fixture routes the primary domain at the same stub, so the DSN is
    read off the wire. The tempfailing recipient is rejected at RCPT (no
    DATA), so the only DATA the stub captures is the warning DSN itself.

    Full-stack tier_3 via the production Go-bridge path. (The former in-nest
    `/api/v1/test/outbound/*` `/drain` orchestration was deleted in the I6
    cutover; the production Go-bridge path is now the only outbound send path.)
    """
    handle = mail_bridge_mta
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    tempfail_rcpt = "tempfail@external.test"

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()
    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {tempfail_rcpt}",
        "Subject: T1.3 delay warning on a tempfailing recipient",
        f"Message-ID: <t13delay-{int(time.time() * 1000)}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This message is addressed to a recipient whose MX keeps tempfailing.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        # Accepted at submission time; the transient failure only shows up
        # when the outbound worker reaches the remote MX.
        conn.cmd(f"RCPT TO:<{tempfail_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # Attempt 1 fires immediately (submission Data triggers the worker) and
    # the stub 451s the RCPT → mark_outbound_failed. Let it land so the row's
    # created_at is set on the real clock before we freeze + fast-forward.
    time.sleep(1.0)
    _advance_outbound_clock(handle.nest_url, 4 * 3600 + 120)

    # The worker re-polls within its PollInterval; the next attempt crosses
    # the 4 h boundary and nest enqueues the 4.4.7 delayed DSN, which the
    # worker then delivers to the stub (primary domain routed there).
    dsn = None
    poll_deadline = time.monotonic() + 75.0
    while time.monotonic() < poll_deadline:
        for msg in handle.stub_mx.messages():
            low = msg.lower()
            if b"multipart/report" in low and b"action: delayed" in low:
                dsn = msg
                break
        if dsn is not None:
            break
        time.sleep(0.5)
    assert dsn is not None, (
        "stub MX received no delayed DSN within 75s — nest's mark_outbound_"
        f"failed handler did not emit the 4 h delay-warning (bridge log: {handle.log_file})"
    )

    text = dsn.decode("utf-8", errors="replace").lower()
    assert "status: 4.4.7" in text, f"delay warning missing 4.4.7 status:\n{dsn[:600]!r}"
    assert sender.lower() in text, (
        f"delay warning not addressed back to the sender {sender!r}:\n{dsn[:600]!r}"
    )

    # Once-per-message: fast-forward again so the original row is due for a
    # further attempt, poke the worker to run that attempt NOW, and assert no
    # second delayed DSN was emitted (delay_warned_at gate).
    #
    # The barrier is the row's own `attempt_count`, not elapsed time
    # (convention 14): `mark_outbound_failed_handler` runs the delay-warning
    # enqueue INSIDE its `FailedDecision::Retry` arm and only then calls
    # `mark_outbound_attempt`, which bumps the count — so a count that has
    # advanced proves this attempt's warn decision already ran to completion.
    # Waiting on that instead of a poll interval is what makes the negative
    # assert a statement about state rather than about the clock.
    before = _attempts_for(handle.nest_url, tempfail_rcpt)
    assert before is not None, (
        f"no queued outbound row addressed to {tempfail_rcpt!r} before the second "
        "attempt — the tempfailing message left the queue unexpectedly"
    )
    _advance_outbound_clock(handle.nest_url, 2 * 3600)
    _poke_outbound_bridge(handle.nest_url)
    wait_until(
        lambda: (_attempts_for(handle.nest_url, tempfail_rcpt) or 0) > before,
        MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: (
            f"attempt_count for {tempfail_rcpt} is still "
            f"{_attempts_for(handle.nest_url, tempfail_rcpt)} (was {before}) — the "
            f"poked drain cycle never reported an attempt (bridge log: {handle.log_file})"
        ),
    )

    # Enqueue side: nest decided, this attempt, whether to warn again.
    assert _count_delay_warning_rows(handle.nest_url, sender) == 1, (
        "nest enqueued more than one delay-warning DSN — the 4 h warning must be "
        "emitted once per message, not on every subsequent attempt "
        "(delay_warned_at gate)"
    )
    # Delivery side: and no second one reached the sender over the wire.
    assert _count_delayed(handle.stub_mx) == 1, (
        "more than one delayed DSN reached the sender — the 4 h delay-warning "
        "must be emitted once per message, not on every subsequent attempt"
    )


# ── MTA-STS outbound enforcement (T2.1a) ───────────────────────────────


def _script_mta_sts(nest_url: str, body: dict) -> None:
    """Install a scripted MTA-STS lookup for a domain.

    Hits `POST /api/v1/test/outbound/mta-sts` (gated on `--features
    test-hooks` alone, which `build_node()` enables). The handler inserts
    the scripted outcome into `AppState::mta_sts_override`, keyed by
    `domain`, and `fetch_mta_sts_policy_handler` consults it ahead of the
    production fetcher — so the bridge's per-delivery
    `fauna.bridges.fetch_mta_sts_policy(domain)` returns exactly this.
    """
    payload = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/mta-sts",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"mta-sts hook returned {resp.status}"


def _submit(handle, external_rcpt: str, token: str) -> None:
    """AUTH PLAIN on 465 → submit a one-recipient message tagged `token`.

    `token` is embedded in the Subject and Message-ID so the stub MX's
    accumulated message list (the fixture is session-scoped, so prior
    tests' delivered mail is still present) can be scanned for *this*
    message specifically — never a global `wait_for_message`, which would
    return an unrelated earlier delivery and silently false-pass.
    """
    domain = handle.domain
    sender = f"{handle.submission_sender_local}@{domain}"
    password = handle.submission_credential.decode()
    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()
    body_lines = [
        f"From: Sender <{sender}>",
        f"To: {external_rcpt}",
        f"Subject: MTA-STS enforcement test {token}",
        f"Message-ID: <{token}@{domain}>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"MTA-STS enforcement e2e marker {token}.",
    ]
    body = "\r\n".join(body_lines) + "\r\n"

    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth_plain}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        # External recipient → accepted at submission (existence unknowable
        # until the outbound worker reaches the remote MX).
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(body.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _assert_not_delivered(stub_mx, token: str, window: float) -> None:
    """Assert no message tagged `token` reaches the stub within `window`.

    A bounded-time negative: the submission triggers an immediate worker
    poll, so a would-be delivery lands within a couple of seconds; this
    window is generously above that. The session-scoped stub accumulates
    every test's mail (and background DSN re-deliveries to the primary
    domain), hence the per-token filter — only *this* message's absence
    is asserted.
    """
    deadline = time.monotonic() + window
    while time.monotonic() < deadline:
        msg = _find_tagged(stub_mx, token)
        assert msg is None, (
            f"a message tagged {token!r} reached the stub MX — MTA-STS "
            f"enforcement failed to refuse delivery. First 400 bytes:\n{msg[:400]!r}"
        )
        time.sleep(0.5)


@pytest.mark.feature("mail-server")
def test_outbound_mta_sts_enforce_refuses_mismatched_mx(mail_bridge_mta):
    """An `enforce` policy whose `mx:` list does not match the connected
    host refuses delivery — the message never reaches the stub MX.

    T2.1a — `smtp-server.md` § MX resolution (:452): under MTA-STS enforce,
    a connected MX host that matches no `mx:` pattern is refused per-host;
    once every candidate is exhausted the worker returns a TemporaryError
    and nest reschedules — never an immediate bounce, never a plaintext
    delivery. The stub binds `127.0.0.1`, so the host the matcher
    (`fauna_ffi.MtaStsMxMatches`) sees is `127.0.0.1`, which does not match
    `mail.notmatching.test`. End-to-end: real submit → real nest
    fetch_mta_sts_policy RPC (scripted via the test-hook) → real Go per-host
    enforce decision → real stub MX (which records nothing).
    """
    handle = mail_bridge_mta
    token = f"mtasts-enforce-mismatch-{int(time.time() * 1000)}"
    external_rcpt = f"{token}@external.test"

    # Policy MUST be installed before submit — the bridge fetches it at
    # delivery time (the submission Data triggers the worker immediately).
    _script_mta_sts(
        handle.nest_url,
        {
            "domain": "external.test",
            "outcome": "found",
            "mode": "enforce",
            "mx": ["mail.notmatching.test"],
            "max_age_secs": 604800,
        },
    )
    _submit(handle, external_rcpt, token)

    # Bounded negative: 127.0.0.1 matches no mx: pattern → every host is
    # enforce-refused → reschedule, no delivery.
    _assert_not_delivered(handle.stub_mx, token, window=10.0)


@pytest.mark.feature("mail-server")
def test_outbound_mta_sts_enforce_requires_starttls(mail_bridge_mta):
    """An `enforce` policy whose `mx:` list DOES match still refuses when the
    matched MX offers no STARTTLS — enforce demands TLS, never plaintext.

    T2.1a — `smtp-server.md` § MX resolution: an `enforce` mode that matches
    the host flips the send path to `TLSRequired` (RFC 8461). The stub MX
    advertises no STARTTLS extension, so the bridge returns a TemporaryError
    ("remote MX does not offer STARTTLS") rather than downgrading to
    plaintext; nest reschedules and nothing reaches the stub. This is the
    no-downgrade half of enforcement, complementing the mx-mismatch refusal.
    The host is `127.0.0.1`, which the policy `mx:["127.0.0.1"]` matches, so
    enforcement reaches the TLS-required gate (not the mx-mismatch refusal).
    """
    handle = mail_bridge_mta
    token = f"mtasts-enforce-notls-{int(time.time() * 1000)}"
    external_rcpt = f"{token}@external.test"

    _script_mta_sts(
        handle.nest_url,
        {
            "domain": "external.test",
            "outcome": "found",
            "mode": "enforce",
            "mx": ["127.0.0.1"],
            "max_age_secs": 604800,
        },
    )
    _submit(handle, external_rcpt, token)

    # Bounded negative: host matches, but enforce requires STARTTLS and the
    # stub offers none → TemporaryError, reschedule, no plaintext delivery.
    _assert_not_delivered(handle.stub_mx, token, window=10.0)


@pytest.mark.feature("mail-server")
def test_outbound_mta_sts_testing_mode_delivers(mail_bridge_mta):
    """A `testing`-mode policy with the SAME mx-mismatch as the enforce test
    logs the mismatch but delivers over plaintext.

    T2.1a positive control — `smtp-server.md` § MX resolution (:453): under
    MTA-STS *testing*, an mx-policy mismatch is recorded and delivery
    proceeds opportunistically (plaintext, since the stub offers no
    STARTTLS). The `mx:` list (`mail.notmatching.test`) is identical to the
    enforce-refusal test above; the ONLY difference is `mode`, which
    isolates enforcement as the cause of the two negatives and guards
    against a false-pass where the harness never delivers at all.
    """
    handle = mail_bridge_mta
    token = f"mtasts-testing-deliver-{int(time.time() * 1000)}"
    external_rcpt = f"{token}@external.test"

    _script_mta_sts(
        handle.nest_url,
        {
            "domain": "external.test",
            "outcome": "found",
            "mode": "testing",
            "mx": ["mail.notmatching.test"],
        },
    )
    _submit(handle, external_rcpt, token)

    received = _wait_for_tagged(handle.stub_mx, token, timeout=15.0)
    assert received is not None, (
        "stub external MX received no message tagged "
        f"{token!r} within 15s — testing-mode MTA-STS must log the mx "
        f"mismatch but still deliver (bridge log: {handle.log_file})"
    )


# ── DANE/TLSA outbound enforcement (T2.1b) ─────────────────────────────


def _script_tlsa(nest_url: str, mx_host: str, records: list[dict]) -> None:
    """Install a scripted DANE/TLSA lookup for `mx_host`.

    Hits `POST /api/v1/test/outbound/tlsa` (gated on `--features test-hooks`,
    which `build_node()` enables). The handler inserts the records into
    `AppState::tlsa_override`, keyed by `mx_host`, and `fetch_tlsa_handler`
    consults it ahead of the production DNSSEC resolver — so the bridge's
    per-host `fauna.bridges.fetch_tlsa(mx_host)` returns exactly these.
    Each record is `{usage, selector, matching, data_hex}`.
    """
    payload = json.dumps({"mx_host": mx_host, "records": records}).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/tlsa",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"tlsa hook returned {resp.status}"


@pytest.mark.feature("mail-server")
def test_outbound_dane_pin_match_delivers(mail_bridge_mta):
    """A published TLSA record matching the MX's served cert pins the
    handshake and delivery succeeds over the verified-DANE TLS channel.

    T2.1b — `smtp-server.md` § DANE: nest fetches the DNSSEC-secure TLSA
    records (scripted via the test-hook), the bridge STARTTLSes to the MX and
    verifies the presented chain against them via `fauna_ffi.DaneChainMatches`
    inside `VerifyPeerCertificate`. The TLS stub MX (127.0.0.2) serves a
    self-signed cert; we publish a DANE-EE (usage 3) / full-cert (selector 0)
    / SHA-256 (matching 1) record = sha256(cert DER), so the pin matches and
    the message lands. End-to-end: real submit → real nest fetch_tlsa RPC →
    real Go DANE pin over a real STARTTLS handshake → real stub MX.
    """
    handle = mail_bridge_mta
    token = f"dane-pin-match-{int(time.time() * 1000)}"
    external_rcpt = f"{token}@{handle.dane_domain}"
    cert_sha256 = hashlib.sha256(handle.tls_stub_mx.cert_der).hexdigest()

    # The bridge dials the override target `127.0.0.2:<port>` → the matcher
    # host is the bare `127.0.0.2`. Publish a matching DANE-EE record there.
    _script_tlsa(
        handle.nest_url,
        "127.0.0.2",
        [{"usage": 3, "selector": 0, "matching": 1, "data_hex": cert_sha256}],
    )
    _submit(handle, external_rcpt, token)

    received = _wait_for_tagged(handle.tls_stub_mx, token, timeout=15.0)
    assert received is not None, (
        f"TLS stub MX received no message tagged {token!r} within 15s — a "
        "matching DANE/TLSA pin must allow delivery over the verified TLS "
        f"channel (bridge log: {handle.log_file})"
    )


@pytest.mark.feature("mail-server")
def test_outbound_dane_pin_mismatch_refuses(mail_bridge_mta):
    """A published TLSA record that does NOT match the MX's served cert hard-
    fails the handshake — the message is never delivered (no plaintext
    fallback).

    T2.1b — `smtp-server.md` § DANE: a DANE-pinned host whose presented chain
    matches no published TLSA record is refused; the bridge returns a
    TemporaryError and nest reschedules — never a plaintext downgrade, never
    an immediate bounce. This is the security-critical behavior and needs no
    trusted CA: `VerifyPeerCertificate` runs `DaneChainMatches` against a
    deliberately wrong hash and rejects the handshake. The positive control
    (`test_outbound_dane_pin_match_delivers`) shares the same stub + path and
    differs only in the published hash, isolating the pin as the cause.
    """
    handle = mail_bridge_mta
    token = f"dane-pin-mismatch-{int(time.time() * 1000)}"
    external_rcpt = f"{token}@{handle.dane_domain}"
    wrong_hash = hashlib.sha256(b"not the stub cert").hexdigest()

    _script_tlsa(
        handle.nest_url,
        "127.0.0.2",
        [{"usage": 3, "selector": 0, "matching": 1, "data_hex": wrong_hash}],
    )
    _submit(handle, external_rcpt, token)

    # Bounded negative: the pin mismatch fails the handshake → TemporaryError
    # → reschedule, no delivery.
    _assert_not_delivered(handle.tls_stub_mx, token, window=10.0)


# ── TLSRPT outbound reporter (T2.4) ────────────────────────────────────


def _record_tls_attempt(
    nest_url: str, recipient_domain: str, mx_host: str, result_type: str | None
) -> None:
    """Seed one per-attempt TLS outcome into the production TLSRPT aggregator.

    Hits `POST /api/v1/test/outbound/record_tls_attempt` (gated on `--features
    test-hooks`, which `build_node()` enables — the non-legacy
    `outbound_tlsrpt_test_hook` module). The handler runs the same
    `policy_for_attempt` reconstruction the WS-RPC `report_tls_attempt`
    handler does (here: `not_published` + no DANE → the `no-policy-found`
    bucket) and records the outcome. `result_type=None` is a successful TLS
    session; a token is an RFC 8460 §4.3 failure.

    Seeded directly rather than via a live outbound delivery because the
    submission Sent-copy path (`fauna_recipient.go::dispatchFaunaRecipients`)
    is pre-existing-red on this branch (`451 4.7.0`, unrelated to TLSRPT), so a
    real submit→enqueue→deliver round-trip can't reach the outbound worker
    here. The Go-side classification + the report_tls_attempt wire are covered
    by `internal/mta/outbound_tlsrpt_test.go` and the nest handler unit tests.
    """
    body: dict = {"recipient_domain": recipient_domain, "mx_host": mx_host}
    if result_type is not None:
        body["result_type"] = result_type
    payload = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/record_tls_attempt",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"record_tls_attempt hook returned {resp.status}"


def _dump_aggregator(nest_url: str) -> list[dict]:
    """The in-memory production TLSRPT aggregator buckets (per recipient
    domain) the next emit pass will consume."""
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/tlsrpt_aggregator", method="GET"
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"tlsrpt_aggregator hook returned {resp.status}"
        return json.loads(resp.read())["domains"]


def _aggregator_domain(nest_url: str, domain: str) -> dict | None:
    for d in _dump_aggregator(nest_url):
        if d["domain"] == domain:
            return d
    return None


def _emit_tlsrpt_now(nest_url: str, now: int, rua: dict) -> None:
    """Fire exactly one `run_one_emit_pass` at the scripted epoch `now` with a
    stub policy fetcher returning `rua` per recipient domain (a recorded
    domain absent from `rua` is skipped + cleared)."""
    payload = json.dumps({"now": now, "rua": rua}).encode()
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/emit_tlsrpt_now",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"emit_tlsrpt_now hook returned {resp.status}"


def _tlsrpt_persisted(nest_url: str) -> list[dict]:
    """The persisted `tlsrpt_outbound_reports` rows (one per shipped
    transport)."""
    req = urllib.request.Request(
        f"{nest_url}/api/v1/test/outbound/tlsrpt_persisted", method="GET"
    )
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"tlsrpt_persisted hook returned {resp.status}"
        return json.loads(resp.read())["rows"]


def test_outbound_tlsrpt_emit_persists_aggregated_report(mail_bridge_mta):
    """Nest-side TLSRPT outbound emit pipeline end-to-end against a live nest
    (T2.4 N1+N2+N2c): per-attempt outcomes of *differing* result-types recorded
    for a recipient domain aggregate into one RFC 8460 §4.4 bucket; firing
    `run_one_emit_pass` at a scripted day-boundary fetches the domain's `rua=`
    policy, builds the report, and persists a `tlsrpt_outbound_reports` row,
    then clears the day's bucket.

    Exercises the full nest binary (real router + `AppState` + on-disk DB) via
    the production-faithful seed hook (`policy_for_attempt` → the production
    `AppState.email.tlsrpt_aggregator`), the `NullTlsrptHttpPoster` (always-200
    → an `https:` rua persists without an HTTP server and with no outbound-queue
    side effect), and the daily emitter's drain/fetch/build/persist/clear loop.

    The Go-MTA → `report_tls_attempt` WS half is NOT exercised here: the
    submission Sent-copy path is pre-existing-red on this branch (see
    `_record_tls_attempt`), so a live outbound delivery can't reach the worker.
    That half is covered by `internal/mta/outbound_tlsrpt_test.go` (classifier +
    deliverOne report-flow) and the nest `report_tls_attempt` handler unit tests.
    """
    nest = mail_bridge_mta.nest_url
    domain = "tlsrpt-e2e.test"  # isolated: the seed hook needs no MX routing.

    # Three attempts of differing result-types → one `no-policy-found` bucket
    # with one successful TLS session and two distinct failure tokens. (No live
    # outbound delivery has happened — submission is red — so the aggregator
    # holds only what this test seeds.)
    _record_tls_attempt(nest, domain, "mx.tlsrpt-e2e.test", None)
    _record_tls_attempt(nest, domain, "mx.tlsrpt-e2e.test", "starttls-not-supported")
    _record_tls_attempt(nest, domain, "mx.tlsrpt-e2e.test", "validation-failure")

    snap = _aggregator_domain(nest, domain)
    assert snap is not None, f"aggregator missing {domain}: {_dump_aggregator(nest)!r}"
    assert len(snap["policies"]) == 1, f"expected one bucket, got {snap['policies']!r}"
    bucket = snap["policies"][0]
    assert bucket["policy_type"] == "no-policy-found", bucket
    assert bucket["total_success"] == 1, bucket
    assert bucket["total_failure"] == 2, bucket
    failures = {f["result_type"]: f["count"] for f in bucket["failures"]}
    assert failures == {"starttls-not-supported": 1, "validation-failure": 1}, failures

    # Fire one daily-emit pass at a scripted day-boundary epoch with an https
    # rua (Null poster → 200, persists; no outbound-queue enqueue, no
    # background re-delivery). The pass drains, builds the report, persists, and
    # clears the domain.
    now = 1_780_000_000  # arbitrary fixed day-boundary epoch (2026-05-28 UTC)
    expected_date = datetime.datetime.fromtimestamp(
        now, tz=datetime.timezone.utc
    ).strftime("%Y-%m-%d")
    _emit_tlsrpt_now(nest, now, {domain: [f"https://reports.{domain}/v1"]})

    rows = [r for r in _tlsrpt_persisted(nest) if r["recipient_domain"] == domain]
    assert rows, f"no persisted TLSRPT report row for {domain}: {_tlsrpt_persisted(nest)!r}"
    assert any(r["transport"] == "https" for r in rows), rows
    assert all(r["payload_len"] > 0 for r in rows), rows
    assert all(r["report_date"] == expected_date for r in rows), rows

    # The emit consumed (cleared) the day's bucket for the domain.
    assert _aggregator_domain(nest, domain) is None, (
        f"emit pass did not clear {domain} from the aggregator"
    )


def _rcpt_status(conn, rcpt_addr: str, deadline: float) -> str:
    """Send one `RCPT TO` and return its 3-digit reply code without asserting.

    `_SmtpConn.cmd`/`.expect` raise on a code mismatch, but the hot-reload test
    *polls* the RCPT verdict (550 → 250 → 550) so it needs the actual code, not a
    pass/fail. Reads reply lines until the non-continuation line (a space, not a
    hyphen, in column 4) and returns its leading code.
    """
    conn.send_raw((f"RCPT TO:<{rcpt_addr}>\r\n").encode())
    while True:
        line = conn.recv_line(deadline)
        # Continuation lines are "NNN-..."; the final line is "NNN ..." or "NNN".
        if len(line) < 4 or line[3] != "-":
            return line[:3]


@pytest.mark.feature("admin-mail-policy", "admin-dns-and-certificates")
def test_mta_local_domains_hot_reloads_without_restart(mail_bridge_mta, nest_instance):
    """Adding a local domain hot-applies to the MTA's inbound RCPT gate with no
    restart — the full-stack proof of MTA-side `config_changed` hot-reload
    (nest mail-bridge hot-reload work, Slice 2; mail-policy-config.md § Architectural rules
    — "the bridge MUST apply changes at the next request boundary without a
    restart"; smtp-server.md § local-domains RCPT gate).

    Flow on real binaries: a `RCPT TO:<postmaster@<new>>` for a not-yet-local
    domain is rejected `550 5.7.1` at the bridge's OWN `containsFoldDomain`
    local-domains gate (`server.go` Rcpt) — before any nest call. Admin WS-RPC
    `fauna.bridges.add_local_domain` writes the `mail_domains` row and fans
    `fauna.bridges.config_changed{reason: local_domains}`
    (`bridge_routing_handlers.rs`). The Go MTA's `wsrpc.ConfigReloader` re-fetches
    the whole snapshot off the reader goroutine → the shared MTA config holder
    hot-swaps `LocalDomains` → the NEXT inbound connection's `NewSession` reads it
    → `postmaster@<new>` now passes the local-domains gate and `validate_recipient`
    role-routes it to the admin → `250`. Remove the domain → the gate hot-reverts →
    `550` again. The bridge process is never restarted (same PID throughout).

    `postmaster@` is a reserved role address (`validate_recipient` routes it to the
    admin actor for ANY local domain — `aliases::classify_role_address` is
    local-part-only), so the 250 needs no per-domain user provisioning; the only
    variable under test is whether the domain is in the hot-applied LocalDomains
    list. The new domain is non-primary (the fixture's `fauna.test` is primary), so
    it removes cleanly; removal is in `finally` so a stray domain can't leak into
    the session-scoped fixture's later tests.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_mta
    admin = nest_instance["admin"]
    new_domain = f"hotadd-{secrets.token_hex(4)}.test"
    rcpt = f"postmaster@{new_domain}"

    def _admin_call(kind: str, payload: dict) -> None:
        ws = WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with ws:
            ws.call(kind, payload)

    def _add_domain() -> None:
        _admin_call(
            "fauna.bridges.add_local_domain",
            {
                "domain": new_domain,
                "mta_sts_cert_mode": "per_host",
            },
        )

    def _remove_domain() -> None:
        _admin_call("fauna.bridges.remove_local_domain", {"domain": new_domain})

    def _rcpt_code() -> str:
        """One fresh inbound connection (LocalDomains binds at NewSession), through
        MAIL FROM, returning the RCPT verdict code. external.test fails-open the
        sender-domain check; QUIT closes cleanly so the per-IP rate budget recycles."""
        d = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, d) as conn:
            conn.cmd("MAIL FROM:<sender@external.test>", "250", d)
            code = _rcpt_status(conn, rcpt, d)
            conn.cmd("QUIT", "221", d)
            return code

    def _poll_rcpt_code(want: str, why: str) -> None:
        # config_changed propagates async and binds only on the NEXT connection;
        # poll fresh connections until the gate flips (or fail loudly).
        deadline = time.monotonic() + 50.0
        got = None
        while time.monotonic() < deadline:
            got = _rcpt_code()
            if got == want:
                return
            time.sleep(1.0)
        raise AssertionError(
            f"MTA never hot-applied the local-domains change: RCPT to {rcpt} "
            f"stayed {got!r}, expected {want!r} ({why})"
        )

    pid_before = handle.proc.pid

    # 1) Baseline: domain not local → bridge's own local-domains gate → 550 relay.
    assert _rcpt_code() == "550", (
        f"baseline: RCPT to the not-yet-local {rcpt} must be 550 (relay denied) "
        "at the bridge's local-domains gate"
    )

    # 2) Admin adds the domain → config_changed{local_domains}.
    _add_domain()
    try:
        # 3) The MTA hot-applies the new LocalDomains list → RCPT now 250.
        _poll_rcpt_code("250", "add_local_domain config_changed hot-apply")
        # No restart: the same live process served throughout (hot-reload, not
        # respawn — the product invariant this whole track upholds).
        assert handle.proc.poll() is None, "bridge process exited during the test"
        assert handle.proc.pid == pid_before, "bridge process was restarted (PID changed)"
    finally:
        _remove_domain()

    # 4) After removal, the gate hot-reverts → 550 again (no restart).
    _poll_rcpt_code("550", "remove_local_domain config_changed hot-revert")
    assert handle.proc.pid == pid_before, "bridge process was restarted (PID changed)"


def _read_final_reply(conn, deadline: float) -> str:
    """Read reply lines until a non-continuation arrives — WITHOUT asserting a code.

    `_SmtpConn.expect` asserts the code it was handed, which is exactly what we
    must not do here: the whole point of the oversized-message test is to
    *discover* which of {deliver, defer, bounce, silently-drop} the stack picks.
    """
    while True:
        line = conn.recv_line(deadline)
        if len(line) >= 4 and line[3] == "-":
            continue
        return line


@pytest.mark.feature("mail-server")
def test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost(
    mail_bridge_mta, nest_instance
):
    """A ~3 MiB inbound message must never be ACKed with 250 and then lost.

    The contradiction this pins (`mailbox-migration.md` § Implementation status,
    `architecture/transport-connection.md` § Abuse posture): the SMTP perimeter advertises
    `max_message_bytes` = 50 MB (`bridge_routing.rs:1822`), but the MTA hands the
    message to nest over the same WS-RPC connection every other kind uses —
    `IngestInboundMailRequest.encrypted_body`, inline — and EVERY WS-RPC message
    is capped at 2 MiB (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`,
    symmetric, with no protocol-level chunking). `test_ws_message_size_cap.py`
    already proved the cap binds against a real nest: a 3 MiB frame is refused by
    the *transport* — the connection is closed and the dispatcher never sees it.

    So a message between ~2 MiB and 50 MB is accepted at the perimeter and has no
    route to storage. What nobody had established is the **consequence**: whether
    such a message bounces, defers forever, or is silently dropped. This test
    establishes it, and pins the one outcome that is never acceptable.

    The assertion is the safety property, not a guess at the mechanism: if the
    MTA ACKs `.` with 250, the message MUST be in nest — a 250 tells the sending
    server "delivered", and it will never retry. A 250 with nothing stored is
    silent, unrecoverable mail loss at the live alpha perimeter. A 4xx/5xx is a
    typed rejection: the sender is told, and retries or bounces. That is the
    behaviour `mailbox-migration.md` requires ("never accepted-then-undeliverable").

    OBSERVED (2026-07-12, first run): the MTA answered end-of-DATA with
    `451 4.7.0 Mail ingestion temporarily unavailable; try again later` and stored
    nothing — the stack **deferred**, transient code for a permanent condition, so
    a real sender would have retried the doomed message for days.

    RESOLVED (2026-07-12): first by clamping the perimeter to the inline ceiling and
    answering a permanent `552 5.3.4` (honest, but a 1.5 MB cap on every mailbox),
    and then properly — the bulk-plane reference legs. A sealed body over the frame
    is now staged on the byte plane and the ingest RPC carries only its chunk hashes,
    so this 3 MiB message DELIVERS.

    The safety property this test was built to defend never changed — *a 250 must
    mean stored* — but it used to be vacuous here, because the message could not be
    stored at all and so was correctly refused. It is now reachable, which makes this
    the test that proves the ~2 MiB–8 MB band, once a silent-mail-loss hazard, is
    genuinely deliverable. A regression that re-refuses this message fails the first
    assertion; a regression that ACKs it without storing it fails the second — which
    is the failure mode that would lose a real user's mail.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    db_path = nest_instance["db_path"]

    def _stored() -> int:
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            return db.execute("SELECT COUNT(*) FROM bridge_imap_messages").fetchone()[0]
        finally:
            db.close()

    before = _stored()

    # ~3 MiB: comfortably over the 2 MiB WS-RPC cap, comfortably under the
    # perimeter's 50 MB. No line starts with "." so SMTP dot-stuffing is a no-op.
    filler_line = "x" * 76 + "\r\n"
    filler = filler_line * ((3 * 1024 * 1024) // len(filler_line))
    headers = "\r\n".join(
        [
            "From: External Sender <sender@external.test>",
            f"To: {recipient_addr}",
            "Subject: oversized inbound (WS-RPC cap probe)",
            f"Message-ID: <big-{int(time.time() * 1000)}@external.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "",
        ]
    )
    payload = (headers + filler).encode()
    assert len(payload) > 2 * 1024 * 1024, "the probe must exceed the 2 MiB WS cap"

    deadline = time.monotonic() + 180.0
    with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(payload)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)

    code = reply[:3]
    print(f"\n[ws-cap probe] {len(payload)} byte message -> SMTP reply: {reply!r}")

    # A 3 MiB message is over the frame but well under the product ceiling, so it is
    # deliverable and a rejection is now itself the bug — the reference legs exist
    # precisely so this message is not refused.
    assert code == "250", (
        f"a 3 MiB message must DELIVER by reference over the bulk-byte plane; "
        f"the MTA refused it instead: {reply!r}"
    )

    # 250 means "delivered" to the sending server — it will never retry. So the
    # message has to actually be there. Poll: the ingest is async w.r.t. the ACK
    # only in the failure shapes we are hunting, but give it room regardless.
    stored_deadline = time.monotonic() + 30.0
    while time.monotonic() < stored_deadline:
        if _stored() > before:
            return
        time.sleep(0.5)

    raise AssertionError(
        "SILENT MAIL LOSS: the MTA ACKed a "
        f"{len(payload)}-byte message with {reply!r} — telling the sending server "
        "it was delivered — but nest stored nothing. The message exceeds the 2 MiB "
        "MAX_RPC_WS_MESSAGE_SIZE cap, so the WS-RPC ingest cannot cross, and the "
        "sender will never retry. Mail between ~2 MiB and the perimeter's 50 MB "
        "max_message_bytes is unrecoverably lost."
    )


@pytest.mark.feature("mail-server")
def test_inbound_message_over_the_raw_inline_ceiling_now_delivers(
    mail_bridge_mta, nest_instance
):
    """A message over the raw inline ceiling now DELIVERS instead of being refused.

    This test used to assert the opposite, and the flip is the point
    (`smtp-server.md` § Message size limits). The perimeter's pre-parse clamp used
    to cap raw messages at `MAX_INLINE_RAW_MESSAGE_BYTES` (~1.5 MB) — a hard ceiling
    on every fauna mailbox — because an over-frame body had no route to storage at
    all. Now it clamps to what can actually *rest*, so this message gets in.

    ⚠ Precision, so a later session does not over-read this test: at ~2 MB raw the
    body **still rides INLINE**. The switchover is on the *sealed* size against
    `INLINE_MAIL_REQUEST_BUDGET_BYTES` (2 MiB − 64 KiB), and this filler body seals
    to just under it (its index hint is tiny — the filler is one repeated token). So
    what this pins is the **perimeter clamp**, not the reference path. The reference
    path is proven by its 3 MiB sibling
    (`test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`, whose
    sealed body genuinely exceeds the budget) and end-to-end at 6 MB by
    `test_mail_inbound_to_imap.py::test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte`.

    A regression that re-pins the perimeter to the inline ceiling fails here first.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    db_path = nest_instance["db_path"]

    def _stored() -> int:
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            return db.execute("SELECT COUNT(*) FROM bridge_imap_messages").fetchone()[0]
        finally:
            db.close()

    before = _stored()

    # ~2 MB: over the 1.5 MB inline ceiling and over the 2 MiB frame once sealed,
    # so it can ONLY arrive by reference. Under the product ceiling, so it must.
    filler_line = "x" * 76 + "\r\n"
    filler = filler_line * ((2 * 1000 * 1000) // len(filler_line))
    headers = "\r\n".join(
        [
            "From: External Sender <sender@external.test>",
            f"To: {recipient_addr}",
            "Subject: over the inline ceiling (delivers by reference)",
            f"Message-ID: <ceiling-{int(time.time() * 1000)}@external.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "",
        ]
    )
    payload = (headers + filler).encode()
    assert len(payload) > 1_500_000, "the probe must exceed the 1.5 MB inline ceiling"

    deadline = time.monotonic() + 180.0
    with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(payload)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)

    print(f"\n[inline-ceiling probe] {len(payload)} byte message -> SMTP reply: {reply!r}")
    assert reply.startswith("250"), (
        f"a message over the inline ceiling must now DELIVER (it rides the bulk-byte "
        f"plane by reference), not be refused; got: {reply!r}"
    )
    stored_deadline = time.monotonic() + 30.0
    while time.monotonic() < stored_deadline and _stored() == before:
        time.sleep(0.5)
    assert _stored() > before, (
        f"the MTA ACKed the message with {reply!r} — telling the sending server it was "
        "delivered — but nest stored nothing. A 250 must mean stored."
    )


@pytest.mark.feature("mail-server")
def test_inbound_message_formerly_over_the_at_rest_ceiling_now_delivers(
    mail_bridge_mta, nest_instance
):
    """A ~9 MB message — once refused 552 — now DELIVERS, resting as continuation records.

    The direct change-detector for ceiling retirement (`smtp-server.md` § Message
    size limits). This test used to assert the opposite: a sealed body over the old
    ~8.1 MB at-rest ceiling had to fit one 16 MiB CARv2 record and could not, so it
    was refused a permanent 552. Continuation records dissolved that ceiling — an
    over-cap sealed body now rests as frame-sized part records + a head — so
    `MAX_SEALED_BODY_AT_REST_BYTES` was deleted and `max_message_bytes` (50 MB) is
    the only ceiling. A ~9 MB message is over the old at-rest ceiling and well under
    the 50 MB knob, so it must now be accepted and stored.

    A regression that re-pins the perimeter to the retired at-rest ceiling fails
    here first: the message would be refused 552 instead of stored.
    """
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    db_path = nest_instance["db_path"]

    def _stored() -> int:
        db = sqlite3.connect(db_path, timeout=10.0)
        try:
            return db.execute("SELECT COUNT(*) FROM bridge_imap_messages").fetchone()[0]
        finally:
            db.close()

    before = _stored()

    # ~9 MB: over the old ~8.1 MB at-rest ceiling (so it exercises the
    # continuation-record rest path), well under the 50 MB product ceiling.
    filler_line = "x" * 76 + "\r\n"
    filler = filler_line * ((9 * 1024 * 1024) // len(filler_line))
    headers = "\r\n".join(
        [
            "From: External Sender <sender@external.test>",
            f"To: {recipient_addr}",
            "Subject: formerly over the at-rest ceiling (now delivers)",
            f"Message-ID: <atrest-{int(time.time() * 1000)}@external.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "",
        ]
    )
    payload = (headers + filler).encode()
    assert len(payload) > 8 * 1024 * 1024, "the probe must exceed the old ~8.1 MB at-rest ceiling"

    deadline = time.monotonic() + 240.0
    with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(payload)
        conn.send_raw(b".\r\n")
        reply = _read_final_reply(conn, deadline)

    print(f"\n[retired-at-rest-ceiling probe] {len(payload)} byte message -> SMTP reply: {reply!r}")
    assert reply.startswith("250"), (
        "a ~9 MB message (over the retired at-rest ceiling, under the 50 MB product "
        f"ceiling) must now DELIVER, resting as continuation records; got: {reply!r}"
    )
    stored_deadline = time.monotonic() + 30.0
    while time.monotonic() < stored_deadline and _stored() == before:
        time.sleep(0.5)
    assert _stored() > before, (
        f"the MTA ACKed the message with {reply!r} but nest stored nothing. A 250 must mean stored."
    )
