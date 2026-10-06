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

The journey reproduces that shape through the app UI alone (convention 8):

  enable mail → a real inbound email A is delivered through the real MTA and
  surfaces decrypted in Conversations → the user **rotates the mail keys three
  times** (each a fresh MSEK; the grace window keeps the current key and two
  priors — `SNAPSHOT_GRACE_KEYPAIRS` — so the third rotation drops A's
  generation and A's seal is addressed to no key this account holds any more)
  → **the app process is restarted**
  (a fresh receive loop re-drains the mailbox from UID 0, so it MEETS record A
  again under the new keys — exactly the sweep's per-module cold relaunch) →
  a second inbound email B is delivered → B surfaces decrypted in
  Conversations (the drain moved past A) → the page reports the skipped
  record on `error-message` (`conversations.errors.mail_unopenable`, the
  floor of the page-error stack — `ui/conversations.md` § Errors & edge
  cases).

Why rotation and not disable → re-enable (the journey's shape until
2026-09-30): since `fauna.state.mail`'s cut, an email-only disable leaves the
MSEK dormant on the account's mail custody (its state row is present-wins, the
MSEK being irrecoverable) and a re-enable mints a fresh credential under that
SAME key, so A stays openable — the no-data-loss direction
(`mail-credentials.md` § MSEK lifecycle, *Disable mail*). Rotating past the
grace window is how a record honestly becomes unopenable now.

Why the restart is load-bearing: within one process the cursor is already
past A when the keys rotate, so A is never re-read and nothing is skipped.
The unopenable case is a RE-DRAIN under changed keys — a relaunch, the sweep's
own shape — so the test restarts the app rather than pretending.

⚠ A DEDICATED actor (``dedicated_actor_app``), never the shared session
``test_user``. Record A stays unopenable in its owner's INBOX for good, by
design, and the rotations also strand every record delivered before it; on
``test_user`` the floor would stand on the conversations page for the rest of
the run. On its own actor the count is also EXACT: A is the one record this
mailbox cannot open.

Not `inject_inbound_for_test` (that injects plaintext past the crypto). Every
binary is real, the seal / fetch / open / skip / ingest all run for real.

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

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until
from i18n.strings import S
from helpers.mail_aliases import add_exact_alias

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

    # ── 2. Email A arrives under the FIRST key generation and renders — the
    # sanity half: the receive path works before anything is torn down.
    nonce_a = f"reenablea{int(time.time() * 1000)}qx"
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, _message(recipient_addr, nonce_a),
        time.monotonic() + 40.0,
    )
    app.conversations.navigate()
    _wait_for_nonce(app, nonce_a, what="before the re-enable", mail_bridge_mta=mail_bridge_mta)

    # ── 3. Rotate the mail keys three times, through the app. The grace
    # window holds the current MSEK and two priors (`SNAPSHOT_GRACE_KEYPAIRS`
    # = 3), so the third rotation retires A's generation: A's seal is now
    # addressed to no key this account holds.
    app.mail_settings.navigate()
    for n in range(1, 4):
        app.mail_settings.rotate_keys()
        assert app.mail_settings.wait_for_rotation_to_finish(timeout=60.0), (
            f"rotation {n} of 3 never finished (the confirm form stayed open); "
            f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
        )
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "a rotation re-wraps every live credential, so one must remain; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # ── 4. RESTART the app: the fresh receive loop re-drains the mailbox from
    # UID 0 under the NEW keys and meets record A — the sweep's per-module
    # cold-relaunch shape. `hard_reload` relaunches and replays the login, so
    # the same actor reconnects. The mailbox needs no re-confirm: its keys are
    # the account's mail custody, which the fresh process reads back.
    app.driver.hard_reload()
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # ── 5. Email B arrives under the NEW keys. Its surfacing proves the drain
    # moved PAST the unopenable record A: before the fix the cursor stuck at
    # A forever and B was never ingested.
    nonce_b = f"reenableb{int(time.time() * 1000)}qx"
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, _message(recipient_addr, nonce_b),
        time.monotonic() + 40.0,
    )
    app.conversations.navigate()
    found_b = _wait_for_nonce(
        app, nonce_b, what="after the re-enable + restart", mail_bridge_mta=mail_bridge_mta
    )
    assert found_b.rail == "Smtp", f"received mail must land on the Smtp rail; got {found_b.rail!r}"

    # A itself is gone for good — sealed to a key rotated out of the window —
    # and the fresh process could not have ingested it. (Its pre-restart
    # rendering lived only in that process's memory.)
    assert _thread_with_nonce(app, nonce_a) is None, (
        f"record A ({nonce_a!r}) is sealed to a rotated-out key and cannot have "
        "opened after the restart; its presence means the drain never re-met it "
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
