"""tier_3 e2e: the deployment-wide unlisted-recipient spam penalty
(``mail-spam.md`` § Unlisted-recipient penalty; Plan 2 of the
recipient-whitelist/alias-import design, tracked internally).

The recipient-whitelist model treats mail to any address **not** on a user's
exact-alias set as spam: an unlisted address (never-registered *or* dropped)
falls through the resolver to the domain's **catch-all actor** and is stamped
``X-Fauna-Address-Catchall`` (``libs/fauna-mail/src/aliases/mod.rs``). When an
admin sets a deployment-wide ``unlisted_recipient_penalty`` (points, default 0)
via ``put_spam_policy``, the Go MTA per-recipient delivery loop
(``bins/fauna-bridges/internal/mta/server.go``) adds ``penalty*1000`` to that
recipient's combined spam milli-score, re-derives the disposition, and files the
copy to **Junk**. A *listed* exact-alias recipient carries no catch-all stamp, so
the penalty never applies → **INBOX**.

This is Plan 2's **Task 6** — the end-to-end proof of the
header → penalty → disposition → Junk chain through the real MTA + nest that the
tier_1 (``apply_unlisted_recipient_penalty_milli``), wire round-trip,
effective-overlay, and Go unit tests cannot cover (only the full stack proves the
stamp → score → placement wiring lands in ``bridge_imap_messages``).

Design (mirrors ``test_mail_bridge_mta.py``'s deliver + SQL-assert model):

- The ``mail_bridge_mta`` fixture provisions a ``recipient`` actor with an MLS
  pubkey + the exact alias ``recipient@<domain>``. We additionally designate that
  **same** actor as the domain's catch-all (``fauna.bridges.set_catch_all_actor``),
  so unlisted mail routes to it too — one actor, two mailboxes, one
  ``bridge_imap_messages`` query. (The catch-all actor MUST have an MLS pubkey on
  file — inbound seals ``encrypted_body`` to it — which the recipient already has.)

- ``put_spam_policy`` is a **full-replace**, so we re-send the fixture's DNS-gate
  overrides alongside the penalty (else DNSBL/greylist/fcrdns re-enable and break
  the single-transaction delivery). The penalty (1000 pts → 1,000,000 milli) far
  exceeds the default ``spam_folder`` tier (5 pts → 5000 milli) with
  reject off, so a clean catch-all message can only reach the
  spam-folder tier → **Junk**.

- The catch-all *routing* is nest-side (the ``resolve_recipient`` handler reads the
  ``mail_domains`` row fresh per RCPT) and effective immediately; a broken catch-all
  would surface as a ``550`` at RCPT TO, failing loudly. But the *penalty* is applied
  MTA-side from the bridge's config **snapshot** (``s.unlistedRecipientPenalty``),
  which the bridge picks up asynchronously on the ``config_changed`` push — so we
  deliver fresh unlisted messages in a bounded retry loop until one actually lands
  in Junk (that transition IS the proof the penalty fired).

- **False-green guard:** with the penalty reset to 0, the SAME unlisted/catch-all
  path lands in INBOX — proving the penalty (not merely the catch-all routing) is
  what filed the mail to Junk.

``nest_instance`` is session-scoped, so we assert on **mailbox-count deltas** for
the recipient actor (robust to a shared inbox that accumulates sibling-test mail;
the MTA populates only ``from_norm`` on inbound, so per-message ``to_norm`` /
``subject_norm`` matching is unavailable) and **clear the catch-all + restore the
fixture's baseline policy in ``finally``** — a stray catch-all left on the primary
domain would turn the sibling ``ghost@domain → 550`` reject into a ``250``.
"""

import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

# External sender domain — no local-domain exemption, so mail arrives as genuine
# external inbound (mirrors test_mail_client_receive / test_mail_client_spam_receive).
SENDER_DOMAIN = "external.test"

# The mail_bridge_mta fixture's baseline put_spam_policy overrides (conftest.py).
# EVERY field must be re-sent on each put_spam_policy call because the RPC
# FULL-REPLACES the override blob — omitting a field reverts it to the catalog
# default, re-enabling the DNS perimeter gates the fixture disabled and breaking
# single-connection loopback delivery.
_BASELINE_POLICY = {
    "dnsbl_servers": [],
    "greylist_enabled": False,
    "greylist_delay_secs": 0,
    "fcrdns_mode": "off",
    "max_conn_per_min": 1000,
    "baseline_standing_publish": False,
}


def _admin_ws(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _put_spam_policy(nest_instance, *, unlisted_recipient_penalty=None):
    """Full-replace the deployment spam policy: the fixture's DNS-gate baseline
    plus (optionally) the unlisted-recipient penalty."""
    payload = dict(_BASELINE_POLICY)
    if unlisted_recipient_penalty is not None:
        payload["unlisted_recipient_penalty"] = unlisted_recipient_penalty
    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_spam_policy", payload)


def _set_catch_all(nest_instance, domain, actor_id):
    with _admin_ws(nest_instance) as ws:
        ws.call(
            "fauna.bridges.set_catch_all_actor",
            {"domain": domain, "actor_id": actor_id},
        )


def _clear_catch_all(nest_instance, domain):
    # Omitting actor_id clears the designation (SetCatchAllActorRequest.actor_id
    # is Option<ByteBuf>, None => clear).
    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.set_catch_all_actor", {"domain": domain})


def _mailbox_counts(db_path, actor_id):
    """``(inbox, junk)`` row counts for ``actor_id`` in ``bridge_imap_messages``.
    Deltas isolate this test from the session-shared inbox's accumulated mail."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        (inbox,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages "
            "WHERE actor_id = ?1 AND mailbox = 'INBOX'",
            (actor_id,),
        ).fetchone()
        (junk,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages "
            "WHERE actor_id = ?1 AND mailbox = 'Junk'",
            (actor_id,),
        ).fetchone()
        return int(inbox), int(junk)
    finally:
        conn.close()


def _message(recipient_addr, subject, message_id, body):
    lines = [
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        body,
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


def _deliver_inbound(mx_port, server_name, recipient_addr, raw_message, deadline):
    """One real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's port-25
    STARTTLS listener. Returns after the ``250`` on ``.``, which the MTA sends only
    once the WS-RPC ingest (seal + store) committed — so the row is queryable
    immediately. A broken catch-all would 550 at RCPT TO and raise here."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _deliver_unlisted_and_wait_mailbox(handle, db_path, actor_id):
    """Deliver one benign message to a fresh **unlisted** address (no alias →
    routes to the domain catch-all → stamped ``X-Fauna-Address-Catchall``) and
    return the mailbox (``'INBOX'`` or ``'Junk'``) its copy landed in for
    ``actor_id`` — via the count that incremented."""
    inbox_b, junk_b = _mailbox_counts(db_path, actor_id)
    nonce = secrets.token_hex(6)
    to = f"nobody-{nonce}@{handle.domain}"
    msg = _message(
        to,
        f"Unlisted probe {nonce}",
        f"<unlisted-{nonce}@{SENDER_DOMAIN}>",
        "A benign message to an address not on the whitelist.",
    )
    deadline = time.monotonic() + 20.0
    _deliver_inbound(handle.mx_port, handle.domain, to, msg, deadline)
    while time.monotonic() < deadline:
        inbox_a, junk_a = _mailbox_counts(db_path, actor_id)
        if junk_a > junk_b:
            return "Junk"
        if inbox_a > inbox_b:
            return "INBOX"
        time.sleep(0.3)
    return None


@pytest.mark.feature("admin-mail-policy")
def test_unlisted_recipient_penalty_files_catchall_mail_to_junk(
    mail_bridge_mta, nest_instance
):
    handle = mail_bridge_mta
    domain = handle.domain
    actor_id = handle.recipient_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]

    # Designate the recipient actor as the domain catch-all: unlisted mail now
    # routes to it (+ carries the X-Fauna-Address-Catchall stamp) instead of a 550.
    _set_catch_all(nest_instance, domain, actor_id)
    try:
        # ── Penalty ON: 1000 pts dominates every tier; with reject off
        # a catch-all recipient's copy can only reach spam-folder → Junk.
        _put_spam_policy(nest_instance, unlisted_recipient_penalty=1000)

        # Catch-all routing is nest-side (immediate), but the penalty is applied
        # from the bridge's config snapshot (async config_changed push). Deliver
        # fresh unlisted messages until one lands in Junk — that transition proves
        # the stamp → penalty → disposition → placement chain fired end-to-end.
        phase_deadline = time.monotonic() + 60.0
        landed = None
        while time.monotonic() < phase_deadline and landed != "Junk":
            landed = _deliver_unlisted_and_wait_mailbox(handle, db_path, actor_id)
        assert landed == "Junk", (
            "an unlisted (catch-all) message must land in Junk once the "
            f"unlisted_recipient_penalty is active; last landed in {landed!r}. "
            f"bridge log: {handle.log_file}"
        )

        # ── A LISTED exact-alias recipient is unaffected → INBOX. The fixture
        # provisions `recipient@<domain>` as an exact alias for this actor; an
        # exact hit short-circuits before the catch-all step, so it carries no
        # stamp and the penalty never applies.
        inbox_b, junk_b = _mailbox_counts(db_path, actor_id)
        nonce = secrets.token_hex(6)
        listed = f"{handle.recipient_local_part}@{domain}"
        msg = _message(
            listed,
            f"Listed probe {nonce}",
            f"<listed-{nonce}@{SENDER_DOMAIN}>",
            "A benign message to a whitelisted exact alias.",
        )
        deadline = time.monotonic() + 20.0
        _deliver_inbound(handle.mx_port, domain, listed, msg, deadline)
        while time.monotonic() < deadline:
            inbox_a, junk_a = _mailbox_counts(db_path, actor_id)
            if inbox_a > inbox_b or junk_a > junk_b:
                break
            time.sleep(0.3)
        inbox_a, junk_a = _mailbox_counts(db_path, actor_id)
        assert inbox_a == inbox_b + 1 and junk_a == junk_b, (
            "listed (exact-alias) mail must land in INBOX, not Junk — the penalty "
            f"applies only to catch-all recipients; INBOX {inbox_b}->{inbox_a}, "
            f"Junk {junk_b}->{junk_a}"
        )

        # ── False-green guard: with the penalty back to 0, the SAME unlisted /
        # catch-all path lands in INBOX — the penalty (not the catch-all routing)
        # is what moved the mail to Junk above.
        _put_spam_policy(nest_instance, unlisted_recipient_penalty=0)
        phase_deadline = time.monotonic() + 60.0
        landed = None
        while time.monotonic() < phase_deadline and landed != "INBOX":
            landed = _deliver_unlisted_and_wait_mailbox(handle, db_path, actor_id)
        assert landed == "INBOX", (
            "with unlisted_recipient_penalty back to 0, an unlisted (catch-all) "
            "message must land in INBOX (the penalty is what filed it to Junk); "
            f"last landed in {landed!r}"
        )
    finally:
        # Restore the shared session state: clear the catch-all (else a sibling's
        # `ghost@domain -> 550` becomes a 250) and restore the fixture's baseline
        # spam policy (penalty back to 0).
        _clear_catch_all(nest_instance, domain)
        _put_spam_policy(nest_instance)
