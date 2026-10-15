"""tier_3: one received mail record the app can no longer open does NOT stop the
mailbox — later mail keeps arriving, and the app says how many it skipped.

The defect this pins (`mail-app-surface.md` § Inbound client receive →
*Unopenable records*): the shared receive loop pages `inbox.fetch` from a
per-mailbox UID cursor, and the native page opener used to treat ONE record it
could not open as fatal to the whole page — the cursor never moved, the next
tick met the same record, and nothing after it was ever ingested, on every
app, with no client action that could clear it. Four linux e2e reds in the
2026-09-14 whole-suite sweep were exactly this: an earlier module had torn the
shared actor's mailbox down and re-enabled it, so UID 1 of the session actor's
INBOX was sealed to a key the account no longer held, and every later receive
test waited forever for mail the drain never reached.

The journey reproduces that shape with every binary real:

  enable mail → a real inbound email A is delivered through the real MTA and
  surfaces decrypted in Conversations → **A's sealed body is damaged where it
  rests on nest disk** (`helpers.segment_at_rest.unopenable_mail_record`, one
  ciphertext byte flipped in its finalized `__mail` segment) → **the app
  process is restarted** (a fresh receive loop re-drains the mailbox from
  UID 0, so it MEETS record A again — exactly the sweep's per-module cold
  relaunch) → a second inbound email B is delivered → B surfaces decrypted in
  Conversations (the drain moved past A) → the page reports the skipped
  record on `error-message` (`conversations.errors.mail_unopenable`, the
  floor of the page-error stack — `ui/conversations.md` § Errors & edge
  cases).

Why a tampered envelope: the goal names two causes of an unopenable record —
a seal addressed to no generation the account holds, or a malformed or
tampered envelope — and states that neither a rotation nor a disable →
re-enable produces the first (every MSEK generation is carried, and a
disable keeps the MSEK dormant; `mail-app-surface.md` § *Unopenable
records*). This test's earlier shapes relied on exactly those two and went
red once the product stopped losing keys; a foreign recipient seal key does
not reach the seal path either, because the MTA seals to the account's
published per-epoch key schedule, which a standing-key write leaves intact.
The tamper is a fault at rest the user cannot cause through the UI, so it
stands in for no user mutation (convention 8's fixture carve-out). The nest
checks the record's CID header but never re-hashes the block, so it still
serves the record, and only the client's open fails — the real
fetch → open → skip path, never a test seam inside it.

Why the restart is load-bearing: within one process the cursor is already
past A when it is damaged, so A is never re-read and nothing is skipped.
The unopenable case is a RE-DRAIN — a relaunch, the sweep's own shape — so
the test restarts the app rather than pretending. The skip ledger is per app
run, so the restart also proves the fresh process re-meets A and counts it
again.

⚠ A DEDICATED actor (``dedicated_actor_app``), never the shared session
``test_user``. Record A stays unopenable in its owner's INBOX for good, by
design; on ``test_user`` the floor would stand on the conversations page for
the rest of the run. On its own actor the count is also EXACT: A is the one record this
mailbox cannot open.

Not `inject_inbound_for_test` (that injects plaintext past the crypto). Every
binary is real, the seal / fetch / open / skip / ingest all run for real.
Where the harness cannot write the nest's data dir (a nest mode whose data
dir it does not own), the test declares `skip_environment` — no record can be
damaged at rest there.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP wire, a real client
  process restart (`hard_reload()`, replaying the login).
- Marked for the native-relaunch apps (`hard_reload` is a real teardown +
  relaunch there) plus web, whose reload-based `hard_reload` re-drains the
  same way. The skipped-mail notice assertion runs on every app: the
  page-error floor arm reading `ConversationsManager::unopenable_mail_count`
  has landed on all seven (tui led; `ui/conversations.md` § Implementation
  status today, the skipped-unopenable-mail row).
"""

import time

import pytest

from helpers.app_surface import skip_environment
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until
from i18n.strings import S
from helpers.mail_aliases import add_exact_alias
from helpers.segment_at_rest import unopenable_mail_record

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    pytest.mark.real_conversations,
]

SENDER_DOMAIN = "external.test"


def _deliver_inbound(mx_port, server_name, recipient_addr, raw_message, deadline):
    """Deliver one RFC 5322 message to the MTA's port-25 STARTTLS listener;
    returns after the `250` on `.`, i.e. once the sealed record is stored."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _message(recipient_addr: str, nonce: str) -> bytes:
    lines = [
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: Receive after re-enable {nonce}",
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must round-trip into the conversations view.",
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


def _thread_with_nonce(app, nonce: str):
    return next(
        (
            t
            for t in app.conversations.list_threads()
            if nonce in (t.label or "") or nonce in (t.snippet or "")
        ),
        None,
    )


def _wait_for_nonce(app, nonce: str, *, what: str, mail_bridge_mta):
    return wait_until(
        lambda: _thread_with_nonce(app, nonce),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"{what}: the inbound email tagged {nonce!r} never surfaced decrypted in "
            f"the conversations list.\n"
            f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )


@pytest.mark.feature("email-in-conversations")
def test_mailbox_keeps_receiving_past_a_record_the_reenabled_keys_cannot_open(
    dedicated_actor_app, mail_bridge_mta, nest_instance
):
    app, actor = dedicated_actor_app

    # ── 1. Mail on, and the routing alias for this actor (the same admin-side
    # alias setup the plain receive test makes; `enable_mail` registers the
    # recipient pubkey but not the `<local>@<domain> → actor` alias).
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    domain = mail_bridge_mta.domain
    # Per actor: a sweep runs this once per app against one nest, each time
    # as a fresh actor, and an exact alias names exactly one of them.
    local_part = f"e2ereenable{actor['actor_id_hex'][:10]}"
    recipient_addr = f"{local_part}@{domain}"
    add_exact_alias(nest_instance["url"], actor["signing_key"], domain, local_part)

    # ── 2. Email A arrives and renders — the sanity half: the receive path
    # works before anything is damaged. Surfacing it is also the one read
    # that finalizes its segment on disk, which the tamper below needs.
    nonce_a = f"reenablea{int(time.time() * 1000)}qx"
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, _message(recipient_addr, nonce_a),
        time.monotonic() + 40.0,
    )
    app.conversations.navigate()
    _wait_for_nonce(app, nonce_a, what="before the tamper", mail_bridge_mta=mail_bridge_mta)

    # ── 3. Damage A's sealed body at rest — a tampered envelope, the goal's
    # cause the product can still reach. A is this mailbox's newest (and only)
    # record, which is the one the helper damages.
    try:
        _, uid_a = unopenable_mail_record(nest_instance, bytes.fromhex(actor["actor_id_hex"]))
    except PermissionError:
        skip_environment(
            "the nest's data dir is not writable by the harness in this nest mode, "
            "so no record can be damaged at rest"
        )
    assert uid_a == 1, f"record A must be the mailbox's first record; the tamper hit uid {uid_a}"

    # ── 4. RESTART the app: the fresh receive loop re-drains the mailbox from
    # UID 0 and meets the damaged record A — the sweep's per-module
    # cold-relaunch shape. `hard_reload` relaunches and replays the login, so
    # the same actor reconnects. The mailbox needs no re-confirm: its keys are
    # the account's mail custody, which the fresh process reads back.
    app.driver.hard_reload()
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # ── 5. Email B arrives. Its surfacing proves the drain
    # moved PAST the unopenable record A: before the fix the cursor stuck at
    # A forever and B was never ingested.
    nonce_b = f"reenableb{int(time.time() * 1000)}qx"
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, _message(recipient_addr, nonce_b),
        time.monotonic() + 40.0,
    )
    app.conversations.navigate()
    found_b = _wait_for_nonce(
        app, nonce_b, what="after the tamper + restart", mail_bridge_mta=mail_bridge_mta
    )
    assert found_b.rail == "Smtp", f"received mail must land on the Smtp rail; got {found_b.rail!r}"

    # A itself is gone for good — its sealed body no longer authenticates —
    # and the fresh process could not have ingested it. (Its pre-restart
    # rendering lived only in that process's memory.)
    assert _thread_with_nonce(app, nonce_a) is None, (
        f"record A ({nonce_a!r}) was damaged at rest and cannot have opened "
        "after the restart; its presence means the drain never re-met it "
        "(the restart did not re-drain from UID 0) — the containment property "
        "is then untested here"
    )

    # ── 6. The user is told: the skipped record surfaces on `error-message` as
    # the floor of the page-error stack — on every app.
    notice = wait_until(
        lambda: "could not be opened" in (app.error_text() or "") and app.error_text(),
        20.0,
        diagnose=lambda: (
            f"the skipped record never surfaced on error-message; error text: "
            f"{app.error_text()!r}"
        ),
    )
    # Exactly A: this actor's mailbox holds one record its keys cannot open.
    assert notice == S.conversations.errors.mail_unopenable(count="1"), (
        f"the notice must count exactly record A, as the resolved i18n string: {notice!r}"
    )
