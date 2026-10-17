"""tier_4 e2e: the family (child-safety) mail gate on the real Docker image.

The family mail gate is the one mail-ingress feature whose enforcement lives partly
in the **Go MTA** (`internal/mailfauna/dsn.go` — the RFC 3464 `DsnCorrelation` parse
+ the bound-4 reply-address extraction) and partly in the nest
(`supervised_mail_verdict` + `db::family`). tier_3 spins a bare `fauna-nest` binary
and drives the nest handlers directly; it can't run the Go MTA's real DSN parse or the
s6-supervised bring-up. This test closes that gap: the guardian gate, driven through
the real image the way inbound mail actually arrives.

Cases (goal: ``docs/goal/behavior/family-safety.md`` § The mail gate — bounds 1-4):

  (a) A ward with ``unknown_sender_mail = reject`` refuses a cold external sender at
      SMTP ``RCPT TO`` with the generic ``550 5.1.1 User unknown`` — byte-identical
      to a nonexistent address, so the refusal never reveals a supervised ward
      (``family-safety.md`` § The mail gate; ``network-exposure.md`` § Rulings F5) —
      while an adult co-recipient still receives (the reject is per-recipient at
      RCPT, not a DATA-stage refusal that would deny the adult too).
  (b) A ``MAIL FROM:<>`` NON-DSN message to a ``hold`` ward lands in the ward's
      ``Guardian Review`` held mailbox and surfaces in the guardian's queue
      (``family-safety.md:172`` — everything with an empty sender that isn't a
      budget-correlated genuine bounce is held).
  (c) A genuine bounce (a ``MAIL FROM:<>`` RFC 3464 report naming the Message-ID of a
      message the ward actually sent) reaches INBOX, while a forged report naming an
      allowlisted ``Final-Recipient`` address but an id the ward never sent is held
      (``family-safety.md:155,:157`` — the id authorizes, the address is a hint).
  (d) A correlated report replayed past the per-id correlation budget
      (remote-recipients + 2) is held (``family-safety.md:161`` bound 1).
  (e) A genuine bounce delivers, the ward replies to it, and the report's ``From:`` is
      NOT thereafter a known sender (``family-safety.md:164`` bound 4 —
      ``guardian_mail_correlated_origins`` declines the auto-seed); a submission-stamped
      Message-ID verifies as Fauna-minted (``family-safety.md:163`` bound 3 — a genuine
      bounce correlates only because its id passes ``is_fauna_minted_msgid``).
  (f) The ward's own mail client reads held mail but cannot move or expunge it — an
      IMAP-observable refusal (``family-safety.md`` § The mail gate: the ward's to
      read, never to relocate).
  (g) A same-nest Fauna sender mailing a ``reject`` ward is refused, typed
      ``guardian_approval_required``, and nothing is delivered — not even to an adult
      co-recipient (§ Guardian policy — the enforcement split).
  (h) Graduation keeps the account, its handle and its mail, and releases what was
      held to INBOX (§ Graduation & transfer — "graduation releases, never drops").

Topology mirrors ``test_mail_security_accept.py``: one user-defined network carrying the
fail-closed ``fakes/{fake_clamd,fake_rspamd}`` scan sidecars (a *held* or *delivered*
message clears ingest's C.7 scan gate). No ``fake_dns`` / DMARC enforcement — the axis
under test is the guardian gate, not the auth perimeter, and inbound is delivered over
loopback (HELO/FCrDNS/sender-MX exempt). Storage committed plaintext (deploy default).
"""

import email.utils
import hashlib
import os
import subprocess
import time
import uuid

import pytest

from .helpers import (
    admin_ws,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    deliver_inbound_attempt_loopback_curl,
    deliver_inbound_raw_loopback_curl,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    provision_mail_recipient,
    _provision_submission_token,
    register_primary_domain,
    relax_spam_policy,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_scanner_sidecars,
    submit_message_tls,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"
CLAIM_CODE = "FAMLY1"
HELD_MAILBOX = "Guardian Review"  # fauna_protocol::email::GUARDIAN_HELD_MAILBOX


# ── Message-ID mint (Python twin of libs/fauna-mail/src/msgid.rs) ─────────────


def fauna_minted_msgid(domain: str = DOMAIN, random12: bytes | None = None) -> str:
    """A Fauna-minted Message-ID the nest's ``is_fauna_minted_msgid`` accepts —
    ``local = hex(random[12] ‖ sha256("fauna-msgid-v1"‖random)[..4])`` (keyless, so a
    test can mint one itself; ``msgid.rs:68-79``). Golden vector cross-checked below."""
    r = random12 if random12 is not None else os.urandom(12)
    tag = hashlib.sha256(b"fauna-msgid-v1" + r).digest()[:4]
    return f"<{(r + tag).hex()}@{domain}>"


def _assert_mint_golden():
    # msgid.rs:153-156 — mint_local(bytes(range(12))) local-part == this.
    local = fauna_minted_msgid("x", bytes(range(12))).split("@")[0].lstrip("<")
    assert local == "000102030405060708090a0b4257c1d5", local


def non_minted_msgid(domain: str = DOMAIN) -> str:
    """A 32-hex Message-ID with a WRONG provenance tag — the shape a third-party MUA
    (MD5 / uuid4().hex) wears; ``is_fauna_minted_msgid`` must reject it (bound 3)."""
    return f"<{uuid.uuid4().hex}@{domain}>"  # random tag ⇒ recompute mismatches w.p. 1-2^-32


# ── Identity setup (guardian / ward / adult over the wire) ────────────────────


def _guardian_ws(nest, guardian):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest["url"], actor_id=guardian["actor_id_bytes"],
        signing_key=bytes(guardian["signing_key"]))


def make_guardian(nest) -> dict:
    """An ordinary (non-supervised, non-suspended) admin-admitted user — the only
    kind ``check_guardian_admissible`` accepts as a guardian_actor."""
    from common.auth import create_actor_and_register

    return create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])


def make_ward(nest, run_seal_helper, *, guardian, local_part: str, password: str,
              with_send: bool = False) -> dict:
    """Admit a WARD guardianed by ``guardian`` and give it a mail identity.

    A guardianship is born ONLY at admission (no wire method links an existing
    account), carried by ``guardian_actor`` on the invite code:
      1. an admin mints an invite code naming the guardian,
      2. the ward self-registers with it (``register_handled_actor`` →
         ``fauna.account.register`` → ``create_user_with_handle(..., guardian)``),
      3. we layer the mail recipient recipe onto the just-registered ward actor
         (``provision_mail_recipient(actor=…)``), and — for the send cases — a
         submission token so the ward can send (auto-seeding its sent-msgid set).

    The container's registration mode is set to ``invite_required`` in the fixture so
    the ward's self-service register is accepted (default is ``Closed``).
    """
    from common.auth import register_handled_actor

    with admin_ws(nest) as admin:
        code = admin.call(
            "fauna.admin.invite_codes.create",
            {"tier": "free", "uses": 1, "guardian_actor": guardian["actor_id_bytes"]},
        )["code"]

    # The register signature is over actor_id‖handle‖domain‖ts; the nest tries its
    # handle_domain + every active local domain, so DOMAIN (an active local domain
    # after register_primary_domain) verifies.
    ward = register_handled_actor(
        nest["port"], local_part, nest["admin"].get("domain") or DOMAIN,
        invite_code=code, base_url=nest["url"])

    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=local_part, password=password,
        actor=(ward["actor_id_bytes"], bytes(ward["signing_key"])))
    if with_send:
        _provision_submission_token(
            nest, run_seal_helper, actor_id=ward["actor_id_bytes"],
            signing_seed=bytes(ward["signing_key"]), credential=password)
    return {**recipient, "signing_key": ward["signing_key"], "handle": local_part}


def set_family_policy(nest, *, guardian, ward_actor_id: bytes, unknown_sender_mail: str):
    """Guardian (never admin/ward) sets the ward's ``unknown_sender_mail`` — the whole
    ReachPolicy is replaced; omitted fields default to unsupervised-equivalent."""
    with _guardian_ws(nest, guardian) as gws:
        gws.call("fauna.family.policy.update", {
            "supervised_actor_id": ward_actor_id,
            "policy": {"unknown_sender_mail": unknown_sender_mail},
        })


def list_mail_holds(nest, *, guardian, ward_actor_id: bytes) -> list:
    """The guardian's approval queue, filtered to this ward's mail holds (envelope
    metadata only: peer_address + message_id)."""
    with _guardian_ws(nest, guardian) as gws:
        approvals = gws.call("fauna.family.approvals.list", {}).get("approvals", [])
    return [a for a in approvals
            if a.get("kind") == "mail_hold"
            and bytes(a.get("supervised_actor_id", b"")) in (ward_actor_id, b"")]


# ── IMAP read-back (generalized to any mailbox, not just INBOX) ───────────────


def _imap_mailbox(host, port, server_name, username, password, mailbox, *,
                  want_min: int = 1, timeout: float = 90.0):
    """AUTHENTICATE PLAIN → poll ``SELECT "<mailbox>"`` until EXISTS ≥ ``want_min``,
    then return ``(count, first_body_or_None)``. Generalizes
    ``imap_fetch_only_inbox_message`` (helpers.py:1373) to an arbitrary mailbox — the
    held mailbox name carries a space, so it must be IMAP-quoted. Raises TimeoutError
    with the last count on miss; ``want_min=0`` returns as soon as a SELECT succeeds
    (used to assert a mailbox stays *empty*)."""
    from helpers.mail_wire import (
        _imap_auth_plain, _imap_fetch_body_cmd, _imap_read_tagged, _imaps_connect_addr,
    )

    quoted = f'"{mailbox}"' if " " in mailbox else mailbox
    deadline = time.monotonic() + timeout
    last_count = -1
    last_err = None
    sock = buf = None
    seq = 0
    try:
        while time.monotonic() < deadline:
            try:
                if sock is None:
                    sock, buf = _imaps_connect_addr(host, port, server_name, deadline)
                    status = _imap_auth_plain(sock, buf, "a0", username, password, deadline)
                    assert status == "OK", f"AUTH PLAIN must succeed for {username}; got {status}"
                seq += 1
                sock.sendall(f"s{seq} SELECT {quoted}\r\n".encode())
                status, untagged = _imap_read_tagged(sock, buf, f"s{seq}", deadline)
                assert status == "OK", f"SELECT {quoted} failed: {status}: {untagged!r}"
                exists = [ln for ln in untagged if ln.upper().endswith("EXISTS")]
                last_count = int(exists[0].split()[1]) if exists else 0
                if last_count >= want_min:
                    body = None
                    if last_count >= 1:
                        seq += 1
                        _s, body = _imap_fetch_body_cmd(
                            sock, buf, f"f{seq}", "UID FETCH 1:* BODY[]", deadline)
                    try:
                        sock.sendall(b"bye LOGOUT\r\n")
                    except OSError:
                        pass
                    return last_count, body
            except (OSError, AssertionError) as e:
                last_err = e
                if sock is not None:
                    try:
                        sock.close()
                    except OSError:
                        pass
                    sock = buf = None
            time.sleep(1.0)
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
    if want_min == 0:
        return last_count, None
    raise TimeoutError(
        f"{quoted} did not reach EXISTS≥{want_min} for {username} within {timeout}s "
        f"(last count={last_count}, last error: {last_err})")


def inbox_count(nest, recipient) -> int:
    mp = nest["mail_ports"]
    count, _ = _imap_mailbox("127.0.0.1", mp[993], DOMAIN, recipient["username"],
                             recipient["password"], "INBOX", want_min=0, timeout=15)
    return count


def held_count(nest, recipient) -> int:
    mp = nest["mail_ports"]
    count, _ = _imap_mailbox("127.0.0.1", mp[993], DOMAIN, recipient["username"],
                             recipient["password"], HELD_MAILBOX, want_min=0, timeout=15)
    return count


def wait_inbox(nest, recipient, want_min=1) -> bytes:
    mp = nest["mail_ports"]
    _c, body = _imap_mailbox("127.0.0.1", mp[993], DOMAIN, recipient["username"],
                             recipient["password"], "INBOX", want_min=want_min)
    return body


def wait_held(nest, recipient, want_min=1) -> bytes:
    mp = nest["mail_ports"]
    _c, body = _imap_mailbox("127.0.0.1", mp[993], DOMAIN, recipient["username"],
                             recipient["password"], HELD_MAILBOX, want_min=want_min)
    return body


# ── DSN / message construction ────────────────────────────────────────────────


def build_dsn(*, report_from: str, to_addr: str, final_recipient: str,
              returned_msgid: str | None, extra_headers: tuple = (),
              include_delivery_status: bool = True) -> bytes:
    """A hand-built RFC 3464 ``multipart/report; report-type=delivery-status`` — the
    shape the Go extractor (``dsn.go``) parses. ``report_from``/``extra_headers`` (e.g.
    a ``Cc:``) are the reply-address set bound 4 records. ``returned_msgid=None`` omits
    the returned-headers part (a report returning no message → held). Set
    ``include_delivery_status=False`` to make it a report *costume* (no
    ``message/delivery-status`` part → held)."""
    boundary = f"==dsn-{uuid.uuid4().hex}=="
    headers = [
        f"From: {report_from}",
        f"To: {to_addr}",
        "Subject: Delivery Status Notification (Failure)",
        f"Message-ID: <{uuid.uuid4().hex}@mailer.remote.test>",
        f"Date: {email.utils.formatdate(usegmt=True)}",
        "MIME-Version: 1.0",
        f'Content-Type: multipart/report; report-type="delivery-status"; '
        f'boundary="{boundary}"',
        *extra_headers,
    ]
    parts = [
        f"--{boundary}\r\n"
        "Content-Type: text/plain; charset=us-ascii\r\n\r\n"
        "Your message could not be delivered to one or more recipients.\r\n"
    ]
    if include_delivery_status:
        parts.append(
            f"--{boundary}\r\n"
            "Content-Type: message/delivery-status\r\n\r\n"
            "Reporting-MTA: dns; mailer.remote.test\r\n\r\n"
            f"Final-Recipient: rfc822; {final_recipient}\r\n"
            "Action: failed\r\n"
            "Status: 5.1.1\r\n")
    if returned_msgid is not None:
        parts.append(
            f"--{boundary}\r\n"
            "Content-Type: text/rfc822-headers\r\n\r\n"
            f"Message-ID: {returned_msgid}\r\n"
            f"To: {final_recipient}\r\n"
            "Subject: (returned message headers)\r\n")
    body = "".join(parts) + f"--{boundary}--\r\n"
    return ("\r\n".join(headers) + "\r\n\r\n" + body).encode()


def compose(*, sender: str, rcpt: str, subject: str, body: str, message_id: str) -> bytes:
    return (
        f"From: {sender}\r\nTo: {rcpt}\r\nSubject: {subject}\r\n"
        f"Message-ID: {message_id}\r\n"
        f"Date: {email.utils.formatdate(usegmt=True)}\r\n\r\n{body}\r\n"
    ).encode()


def accept_delivery(deliver_fn, *, tries: int = 8, delay: float = 3.0) -> str:
    """Call a ``(rc, combined_output)``-returning loopback delivery, retrying on a
    transient ``451``. The nest's ingest catch-all (``mta/server.go:1838-1842``)
    classifies any non-permanent ``ingest_inbound_mail`` error as ``451 4.7.0 … try
    again later`` — a real sending MTA retries a 4xx, so we do too (the fail-closed
    scan/ingest path can 451 in the brief window right after the listeners serve but
    before it is warm). Returns the accepting attempt's output; raises on a permanent
    reject (``5xx``) or a 451 that never clears."""
    last = ""
    for _ in range(tries):
        rc, out = deliver_fn()
        if rc == 0:
            return out
        last = out
        if "451" in out:
            time.sleep(delay)
            continue
        raise AssertionError(f"delivery permanently rejected (not a retryable 451): {out!r}")
    raise AssertionError(f"delivery still 451 after {tries} tries: {last!r}")


def ward_send(nest, ward, *, rcpt: str, message_id: str, subject="hello") -> None:
    """The ward submits one authenticated message over implicit-TLS 465 — seeds
    ``guardian_mail_sent_msgids`` (if the id is minted + ≥1 remote recipient) and
    allowlists ``rcpt`` at ``enqueue_outbound_mail``."""
    mp = nest["mail_ports"]
    raw = compose(sender=ward["username"], rcpt=rcpt, subject=subject,
                  body="body", message_id=message_id)
    submit_message_tls("127.0.0.1", mp[465], DOMAIN, sender=ward["username"],
                       password=ward["password"], rcpt=rcpt, raw_message=raw)


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield


@pytest.fixture(scope="module")
def family_nest(docker_image):
    """A claimed, serving container with the fail-closed scan sidecars, registration
    set to ``invite_required`` so wards can self-register. Module-scoped; each test
    provisions fresh guardian/ward/adult identities (distinct local parts) so per-ward
    allowlist / sent-msgid / hold state is isolated without a container rebuild."""
    _assert_mint_golden()
    suffix = find_free_port()
    network = f"fauna-fam-net-{suffix}"
    clamd_name = f"fauna-fam-clamd-{suffix}"
    rspamd_name = f"fauna-fam-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-mail-family-{http_port}"
    create_network(network)
    try:
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        start_container_with_ports(
            name, {3000: http_port, **mail_ports},
            env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000",
                 "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
                 "FAUNA_RSPAMD_URL": scanners["rspamd_url"]},
            network=network)
        try:
            wait_for_health(http_port, name)
            admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
            nest = {"name": name, "port": http_port,
                    "url": f"https://127.0.0.1:{http_port}",
                    "admin": admin, "mail_ports": mail_ports}
            register_primary_domain(nest, DOMAIN)
            relax_spam_policy(nest)
            with admin_ws(nest) as adm:
                adm.call("fauna.admin.set_registration_mode", {"mode": "invite_required"})
            bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
            yield nest
        finally:
            remove_container(name)
    finally:
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


def _rcpt_reply(curl_verbose: str) -> str | None:
    """The server's reply to ``RCPT TO`` in a ``curl -v`` SMTP transcript — the
    ``< NNN …`` line that follows the ``> RCPT TO:`` line — or None if absent."""
    lines = curl_verbose.splitlines()
    for i, line in enumerate(lines):
        if line.startswith("> RCPT TO:"):
            for reply in lines[i + 1:]:
                if reply.startswith("< "):
                    return reply[2:].strip()
            return None
    return None


# ── Tests ──────────────────────────────────────────────────────────────────────


@pytest.mark.feature("family-safety")
def test_reject_ward_refuses_cold_sender_adult_still_receives(family_nest, run_seal_helper):
    """(a) A ``reject`` ward's cold external sender is refused at ``RCPT TO`` while an
    adult co-recipient still receives — the reject is per-recipient at RCPT, not a
    DATA-stage refusal that would deny the adult too. Proven as two loopback
    deliveries from the same cold sender: ward → 550-at-RCPT, adult → 250 + INBOX.
    (A single-transaction two-RCPT variant is a follow-up — curl aborts on a rejected
    RCPT and the image has no python.)"""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian,
                     local_part="kidreject", password="ward-pw-reject-1")
    adult = provision_mail_recipient(nest, run_seal_helper, domain=DOMAIN,
                                     local_part="adultreject", password="adult-pw-1")
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="reject")

    # Ward RCPT is refused at RCPT TO with the SAME reply a nonexistent address
    # gets — never a distinct policy text that would reveal a supervised ward
    # (family-safety.md § The mail gate; network-exposure.md § Rulings F5).
    rc, out = deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="stranger@cold.test", rcpt_to=ward["username"],
        subject="cold to a reject ward", body_text="hi\n")
    assert rc != 0, f"a reject ward must refuse a cold sender; curl exited 0. {out!r}"
    ward_reply = _rcpt_reply(out)
    assert ward_reply == "550 5.1.1 User unknown", (
        f"reject must be the generic 550 5.1.1 at RCPT; got {ward_reply!r} in {out!r}")
    nobody = ward["username"].replace("kidreject@", "nobody-here@", 1)
    rc_nobody, out_nobody = deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="stranger@cold.test", rcpt_to=nobody,
        subject="cold to nobody", body_text="hi\n")
    assert rc_nobody != 0, f"a nonexistent address must be refused; {out_nobody!r}"
    assert _rcpt_reply(out_nobody) == ward_reply, (
        "the ward's refusal must be indistinguishable from a nonexistent address: "
        f"ward {ward_reply!r} vs nobody {_rcpt_reply(out_nobody)!r}")

    # The SAME cold sender to the adult is accepted and delivered — the ward's
    # rejection does not deny an adult co-recipient.
    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="stranger@cold.test", rcpt_to=adult["username"],
        subject="cold to the adult", body_text="hi adult\n"))
    body = wait_inbox(nest, adult)
    assert b"cold to the adult" in body, "the adult co-recipient must still receive"


@pytest.mark.feature("family-safety")
def test_hold_ward_null_path_nondsn_held(family_nest, run_seal_helper):
    """(b) A ``MAIL FROM:<>`` NON-DSN message to a ``hold`` ward is held (never INBOX)
    and surfaces in the guardian's queue with an empty sender."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian,
                     local_part="kidhold", password="ward-pw-hold-1")
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")

    # A well-formed null-path message (envelope MAIL FROM:<>) that is NOT a
    # delivery-status report — a valid From: header, plain body, no multipart/report
    # part. `family-safety.md:172`: "no report … is held". (The envelope is empty via
    # `mail_from=""`; the From: header is a real address, as a genuine bounce carries.)
    raw = compose(sender="postmaster@mailer.test", rcpt=ward["username"],
                  subject="not a bounce",
                  body="This is a plain message, not a delivery status report.",
                  message_id=f"<{uuid.uuid4().hex}@mailer.test>")
    accept_delivery(lambda: deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from="", rcpt_to=ward["username"], raw=raw))

    wait_held(nest, ward)  # lands in Guardian Review
    assert inbox_count(nest, ward) == 0, "a held message must never reach INBOX"
    holds = list_mail_holds(nest, guardian=guardian, ward_actor_id=ward["actor_id"])
    assert holds, "the guardian's queue must show the hold"


@pytest.mark.feature("family-safety")
def test_genuine_bounce_delivers_forged_report_held(family_nest, run_seal_helper):
    """(c) A genuine bounce (a DSN naming a ward-sent minted Message-ID) reaches INBOX;
    a forged report naming an allowlisted Final-Recipient but an id the ward never sent
    is held. The id authorizes; the address is a hint."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian, local_part="kidbounce",
                     password="ward-pw-bounce-1", with_send=True)  # gitleaks:allow
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")

    # Ward sends to a remote recipient with a minted id → seeds the sent-msgid set
    # (and allowlists alice@remote.test).
    minted = fauna_minted_msgid()
    ward_send(nest, ward, rcpt="alice@remote.test", message_id=minted)
    time.sleep(3)  # let enqueue_outbound_mail seed the sent-msgid

    # Genuine bounce naming the minted id → INBOX (correlated).
    genuine = build_dsn(report_from="MAILER-DAEMON@remote.test",
                        to_addr=ward["username"], final_recipient="alice@remote.test",
                        returned_msgid=minted)
    accept_delivery(lambda: deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from="", rcpt_to=ward["username"], raw=genuine))
    wait_inbox(nest, ward)

    # Forged report: allowlisted Final-Recipient (alice), but an id never sent → held.
    forged = build_dsn(report_from="attacker@evil.test", to_addr=ward["username"],
                       final_recipient="alice@remote.test",
                       returned_msgid=non_minted_msgid())
    accept_delivery(lambda: deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from="", rcpt_to=ward["username"], raw=forged))
    wait_held(nest, ward)
    assert inbox_count(nest, ward) == 1, "the forged report must not reach INBOX"


@pytest.mark.feature("family-safety")
def test_correlation_budget_exhaustion_holds(family_nest, run_seal_helper):
    """(d) A correlated report replayed past the per-id budget (remote-recipients + 2,
    = 3 for a 1-remote send) is held: three deliveries reach INBOX, the fourth is held."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian, local_part="kidbudget",
                     password="ward-pw-budget-1", with_send=True)
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")

    minted = fauna_minted_msgid()
    ward_send(nest, ward, rcpt="bob@remote.test", message_id=minted)
    time.sleep(3)

    budget = 3  # remote_recipients(1) + 2
    for _i in range(budget):
        dsn = build_dsn(report_from="MAILER-DAEMON@remote.test", to_addr=ward["username"],
                        final_recipient="bob@remote.test", returned_msgid=minted)
        accept_delivery(lambda dsn=dsn: deliver_inbound_raw_loopback_curl(
            nest["name"], mail_from="", rcpt_to=ward["username"], raw=dsn))
    wait_inbox(nest, ward, want_min=budget)

    over = build_dsn(report_from="MAILER-DAEMON@remote.test", to_addr=ward["username"],
                     final_recipient="bob@remote.test", returned_msgid=minted)
    accept_delivery(lambda: deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from="", rcpt_to=ward["username"], raw=over))
    wait_held(nest, ward)
    assert inbox_count(nest, ward) == budget, "past-budget report must be held, not delivered"


@pytest.mark.feature("family-safety")
def test_reply_to_correlated_report_never_allowlists_and_mint_verifies(
        family_nest, run_seal_helper):
    """(e) A genuine bounce delivers (bound 3 — its minted id verifies), the ward
    replies to it, and the report's ``From:`` is NOT thereafter a known sender (bound 4
    — ``guardian_mail_correlated_origins`` declines the auto-seed). Contrast: a normal
    recipient the ward mailed IS allowlisted, so the decline is specific to the
    correlated origin."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian, local_part="kidesc",
                     password="ward-pw-esc-1", with_send=True)
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")

    # Ward mails alice@remote.test (minted id) — seeds the sent-msgid AND allowlists
    # alice (the normal auto-seed, our contrast control).
    minted = fauna_minted_msgid()
    ward_send(nest, ward, rcpt="alice@remote.test", message_id=minted)
    time.sleep(3)

    # Bound 3 positive: a minted id correlates → the genuine bounce reaches INBOX.
    daemon = "MAILER-DAEMON@remote.test"
    genuine = build_dsn(report_from=daemon, to_addr=ward["username"],
                        final_recipient="alice@remote.test", returned_msgid=minted)
    accept_delivery(lambda: deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from="", rcpt_to=ward["username"], raw=genuine))
    wait_inbox(nest, ward)

    # The ward replies to the daemon; the auto-seed DECLINES it (bound 4).
    ward_send(nest, ward, rcpt=daemon.lower(), message_id=fauna_minted_msgid(),
              subject="Re: your bounce")
    time.sleep(3)

    # Proof the daemon is NOT known: a cold inbound from it is held.
    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from=daemon, rcpt_to=ward["username"],
        subject="cold from the daemon", body_text="should be held\n"))
    wait_held(nest, ward)

    # Contrast: alice (a normal correspondent the ward mailed) IS known — a cold
    # inbound from alice is delivered, not held. Confirms the decline is specific to
    # the correlated origin, not a blanket failure to allowlist.
    before = inbox_count(nest, ward)
    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="alice@remote.test", rcpt_to=ward["username"],
        subject="cold from alice", body_text="alice is allowlisted\n"))
    wait_inbox(nest, ward, want_min=before + 1)


# ── The child's own view of held mail, the in-domain sender, graduation ──────


def _ward_ws(nest, ward):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest["url"], actor_id=ward["actor_id"], signing_key=bytes(ward["signing_key"]))


class _ImapSession:
    """One authenticated IMAPS session as ``recipient`` — the ward's own mail
    client. ``cmd`` returns the tagged completion line in full (status AND
    text), because a refusal's text is what the MUA shows the child."""

    def __init__(self, nest, recipient, timeout: float = 60.0):
        from helpers.mail_wire import _imap_auth_plain, _imaps_connect_addr

        self._deadline = time.monotonic() + timeout
        self._seq = 0
        self.sock, self.buf = _imaps_connect_addr(
            "127.0.0.1", nest["mail_ports"][993], DOMAIN, self._deadline)
        status = _imap_auth_plain(self.sock, self.buf, "a0", recipient["username"],
                                  recipient["password"], self._deadline)
        assert status == "OK", f"AUTH PLAIN must succeed for {recipient['username']}"

    def cmd(self, text: str) -> tuple[str, list[str]]:
        from helpers.mail_wire import _recv_line

        self._seq += 1
        tag = f"c{self._seq}"
        self.sock.sendall(f"{tag} {text}\r\n".encode())
        untagged = []
        while True:
            line = _recv_line(self.sock, self.buf, self._deadline)
            if line.startswith(tag + " "):
                return line[len(tag) + 1:], untagged
            untagged.append(line)

    def select(self, mailbox: str) -> int:
        quoted = f'"{mailbox}"' if " " in mailbox else mailbox
        done, untagged = self.cmd(f"SELECT {quoted}")
        assert done.startswith("OK"), f"SELECT {quoted} failed: {done!r} {untagged!r}"
        exists = [ln for ln in untagged if ln.upper().endswith("EXISTS")]
        return int(exists[0].split()[1]) if exists else 0

    def close(self):
        try:
            self.sock.sendall(b"z LOGOUT\r\n")
        except OSError:
            pass
        self.sock.close()


@pytest.mark.feature("family-safety")
def test_ward_reads_held_mail_but_cannot_move_or_delete_it(family_nest, run_seal_helper):
    """Outcome 18, from the child's own mail client over IMAP.

    A cold sender's mail to a ``hold`` ward lands in ``Guardian Review``. The ward
    can SELECT that mailbox and FETCH the message — reading is theirs — but a
    ``UID MOVE`` out of it is refused with a tagged ``NO`` naming the guardian
    review, and a ``\\Deleted`` + ``EXPUNGE`` leaves it exactly where it is: the
    expunge succeeds (the ward's routine mail handling keeps working) and simply
    skips the held message. Only the guardian's release or discard relocates it,
    so after all of that the guardian's queue still shows the hold and INBOX is
    still empty."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian,
                     local_part="kidimap", password="ward-pw-imap-1")
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")

    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="stranger@cold.test", rcpt_to=ward["username"],
        subject="held for review", body_text="a held message the child may read\n"))
    wait_held(nest, ward)

    imap = _ImapSession(nest, ward)
    try:
        assert imap.select(HELD_MAILBOX) == 1
        done, untagged = imap.cmd("UID FETCH 1:* BODY[]")
        assert done.startswith("OK"), f"the ward must be able to read held mail: {done!r}"
        assert any("a held message the child may read" in ln for ln in untagged) or (
            "held for review" in " ".join(untagged)), (
            f"the FETCH must return the held message itself: {untagged!r}")

        done, _ = imap.cmd("UID MOVE 1:* INBOX")
        assert done.startswith("NO") and "held for guardian review" in done, (
            f"moving held mail out must be refused with the guardian-review NO: {done!r}")

        done, _ = imap.cmd("UID STORE 1:* +FLAGS (\\Deleted)")
        assert done.startswith("OK"), f"flagging is harmless and allowed: {done!r}"
        done, _ = imap.cmd("EXPUNGE")
        assert done.startswith("OK"), f"the expunge itself must still succeed: {done!r}"
        assert imap.select(HELD_MAILBOX) == 1, (
            "the expunge must skip the held message — it is not the child's to delete")
    finally:
        imap.close()

    assert inbox_count(nest, ward) == 0, "nothing the child tried may move it to INBOX"
    assert list_mail_holds(nest, guardian=guardian, ward_actor_id=ward["actor_id"]), (
        "the hold must still be the guardian's to decide")


@pytest.mark.feature("family-safety")
def test_same_nest_sender_is_told_the_mail_needs_guardian_approval(
        family_nest, run_seal_helper):
    """Outcome 19: the in-domain twin of the mail gate refuses the SENDER.

    A Fauna user on the same nest mails a ward whose guardian accepts only known
    senders (``reject``). There is no SMTP stage to refuse per recipient, so the
    nest refuses the whole send, typed ``guardian_approval_required`` — the
    sender is told — and refuses it BEFORE delivering to anyone: neither the
    ward nor an adult co-recipient receives it. The same sender's identical send
    to the adult alone is delivered, so the refusal is the ward's gate."""
    from clients.ws_rpc_admin_client import RpcCallError

    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian,
                     local_part="kidindomain", password="ward-pw-indomain-1")
    adult = provision_mail_recipient(nest, run_seal_helper, domain=DOMAIN,
                                     local_part="adultindomain", password="adult-pw-indomain")
    sender = provision_mail_recipient(nest, run_seal_helper, domain=DOMAIN,
                                      local_part="neighbour", password="sender-pw-indomain")
    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="reject")

    def send(recipients: list[str], subject: str):
        raw = compose(sender=sender["username"], rcpt=", ".join(recipients),
                      subject=subject, body="hello neighbour",
                      message_id=f"<{uuid.uuid4().hex}@{DOMAIN}>")
        with _ward_ws(nest, sender) as ws:
            return ws.call("fauna.email.send",
                           {"recipients": recipients, "raw_rfc5322": raw})

    with pytest.raises(RpcCallError) as refusal:
        send([ward["username"], adult["username"]], "to the kid and the adult")
    assert refusal.value.code == "fauna.email.guardian_approval_required", (
        f"the sender must be told the mail needs the guardian's approval: {refusal.value!r}")

    assert inbox_count(nest, ward) == 0, "nothing may be delivered to the ward"
    # Not `held_count`: the held mailbox is created by the first hold, so on a
    # ward that never had one SELECT fails — the guardian's queue is the fact.
    assert list_mail_holds(nest, guardian=guardian, ward_actor_id=ward["actor_id"]) == [], (
        "a reject ward's mail is refused, never held for the guardian")
    assert inbox_count(nest, adult) == 0, (
        "no partial delivery: the adult co-recipient must not receive the refused send")

    send([adult["username"]], "to the adult alone")
    body = wait_inbox(nest, adult)
    assert b"to the adult alone" in body, "the same sender's send to the adult must deliver"


@pytest.mark.feature("family-safety")
def test_graduation_keeps_the_account_and_hands_held_mail_to_the_child(
        family_nest, run_seal_helper):
    """Outcome 10: graduation turns the supervised account into an ordinary one
    in place — same account, same handle, same mail — and what was waiting for
    the guardian is released to the child.

    Before graduating, the ward has one message it already received (delivered
    while the policy allowed it) and one held for the guardian (after the
    guardian switched to ``hold``). The guardian graduates; afterwards the SAME
    identity reads the same handle on its account, no guardian on its family
    status, and — over the same IMAP credential — both messages in INBOX with
    ``Guardian Review`` empty and nothing left in the guardian's queue."""
    nest = family_nest
    guardian = make_guardian(nest)
    ward = make_ward(nest, run_seal_helper, guardian=guardian,
                     local_part="kidgrad", password="ward-pw-grad-1")

    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="pal@friends.test", rcpt_to=ward["username"],
        subject="already received", body_text="mail the child already had\n"))
    wait_inbox(nest, ward)

    set_family_policy(nest, guardian=guardian, ward_actor_id=ward["actor_id"],
                      unknown_sender_mail="hold")
    accept_delivery(lambda: deliver_inbound_attempt_loopback_curl(
        nest["name"], mail_from="stranger@cold.test", rcpt_to=ward["username"],
        subject="waiting for the guardian", body_text="held until graduation\n"))
    wait_held(nest, ward)

    with _ward_ws(nest, ward) as ws:
        handle_before = ws.call("fauna.account.get", {}).get("handle")
    assert handle_before, "the ward must have a handle before graduating"

    with _guardian_ws(nest, guardian) as gws:
        gws.call("fauna.family.graduate", {"supervised_actor_id": ward["actor_id"]})

    with _ward_ws(nest, ward) as ws:
        account = ws.call("fauna.account.get", {})
        own_family = ws.call("fauna.family.status", {})
    assert account.get("handle") == handle_before, (
        f"the graduated account must keep its handle: {account.get('handle')!r} "
        f"vs {handle_before!r}")
    assert not own_family.get("supervised_by"), (
        f"the graduated account must be an ordinary one: {own_family!r}")

    wait_inbox(nest, ward, want_min=2)
    imap = _ImapSession(nest, ward)
    try:
        assert imap.select("INBOX") == 2, "both the old mail and the released one are in INBOX"
        done, untagged = imap.cmd("UID FETCH 1:* BODY[]")
        assert done.startswith("OK"), done
        fetched = " ".join(untagged)
        for subject in ("already received", "waiting for the guardian"):
            assert subject in fetched, f"INBOX must carry {subject!r}: {fetched[:600]!r}"
        assert imap.select(HELD_MAILBOX) == 0, "nothing may stay in Guardian Review"
    finally:
        imap.close()
    assert list_mail_holds(nest, guardian=guardian, ward_actor_id=ward["actor_id"]) == [], (
        "nothing may be left waiting in the former guardian's queue")
