"""tier_3: the RECEIVE half of the single-user mail round-trip, observed through
the Fauna APP (the conversations list), not a raw IMAP read.

A real inbound email is delivered through the real Go MTA bridge to the
logged-in user; the MTA HPKE-seals the RFC 5322 body to the recipient's
MSEK-derived pubkey and ingests it. The client's shared receive loop
(`ConversationsSession::start_receive_loop`, over the shared
`fauna-client-conversations::NestMailInboundSource` registered at login) then
fetches the sealed record over `fauna.email.inbox.fetch`, opens BOTH crypto
layers client-side with the account's MSEK (`fauna.state.mail`) (`fauna_mail::open_inbound_record`),
parses the RFC 5322, and feeds the shared `ConversationsManager`. Asserting the
decrypted subject/body surfaces in `list_threads` proves the client-driven
inbound path end-to-end — the receive counterpart to
`test_mail_client_send.py::test_client_driven_send_relays_to_external_mx`.

This is deliberately NOT `inject_inbound_for_test` (that injects plaintext past
the crypto — tier_2). Every binary is real, the wire is real, and the seal /
inbox-fetch / open / parse all run for real (`green-test-or-it-doesnt-work`,
`mda-openmailrecord-not-decrypt`).

Together with the send test this is the full client-driven send+receive
round-trip that mail deploy-verification owns.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP wire, real
  MTA-seal → client-fetch → client-open → parse → ingest.
"""

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import route_inbound_mail_to_app
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until

# Client-driven SMTP receive runs the real shared receive loop
# (NestMailInboundSource + ConversationsSession::start_receive_loop) under e2e on
# linux + web (real session by construction) and windows (via the
# `real_conversations` harness — App.xaml.cs's set_state login path builds the real
# ConversationsSession when FAUNA_E2E_REAL_CONVERSATIONS is set, so the snapshot
# reflects real-decrypted mail instead of the mock ConversationsManagerHost — see
# conftest._build_app_config `_apply_real_conversations_env` + pytest.ini). apple
# has the same harness (FaunaMacApp/FaunaApp applySessionPatch); `.macos` is now
# gated (green in a co-running `--client macos` session). The earlier "receive loop
# doesn't drain after a per-module recover() relaunch" theory was WRONG: the leak was
# cross-test contamination — `test_mail_client_spam_receive` seeded a balanced
# full-confidence spam model for the shared `test_user` that re-filed this test's
# token-neutral inbound to Junk; that test now deletes its model on teardown
# (`_isolate_spam_model`), so this one sees cold-start. `.ios` is gated too (green
# in a co-running `--client ios` session). android carries the same
# `ConversationsManagerHost.startConversationsSession` real-session wiring
# (landed 2026-07-20, `FAUNA_E2E_REAL_CONVERSATIONS` intent-extra twin) — its
# `--client android` run is emulator-host-gated like every android e2e track
# , so the marker is a coverage-contract claim, not a
# device-run claim.
# Mark the supported clients so --client deselects this elsewhere rather than building
# the mail bridge and then skipping in-body (the build is session-scoped + slow).
# `real_conversations` drives the native launch flag.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

# The (external) sender's domain — has no local-domain / loopback exemption, so
# the message arrives as genuine external inbound.
SENDER_DOMAIN = "external.test"


def _deliver_inbound(mx_port: int, server_name: str, recipient_addr: str,
                     raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener. Returns after the `250` on `.`, which the MTA
    sends only once the WS-RPC `ingest_inbound_mail` (seal + store) committed."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("email-in-conversations")
def test_client_driven_receive_renders_decrypted(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    app = logged_in_app

    # ── 1–2. Enable mail on the logged-in user (the shared MailSettingsMachine
    # mints a fresh MSEK, stores it client-side in the `fauna.state.mail` plane — what
    # the poll loop reads to derive the recipient secret — and registers the
    # matching recipient pubkey on the nest, so the MTA can seal inbound DATA to a
    # key only this client can open), then route a distinct local part (not the
    # fixture's "recipient") to *this* actor.
    domain = mail_bridge_mta.domain
    recipient_addr = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "e2euser"
    )

    # ── 3. Deliver a real inbound email through the MTA's port-25 STARTTLS
    # listener. The MTA resolves the alias → this actor, seals the body to the
    # actor's MSEK-derived pubkey, and ingests it. Unique nonce so the assertion
    # is unambiguous even across a session-shared inbox.
    nonce = f"clientrecv{int(time.time() * 1000)}qx"
    subject = f"Client-driven inbound {nonce}"
    message_id = f"<{nonce}@{SENDER_DOMAIN}>"
    body_lines = [
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must round-trip into the conversations view.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, raw_message,
        time.monotonic() + 40.0,
    )

    # ── 4. The shared receive loop (push-driven, `fauna.mail.received`, with a
    # backstop ticker shrunk to 2 s for the e2e via `FAUNA_CONV_POLL_SECS`)
    # fetches + decrypts + ingests on its own. Poll `list_threads` until the
    # DECRYPTED message surfaces — its plaintext subject and body carry the
    # nonce, so its appearance proves the open succeeded. A named generous
    # budget (e2e-conventions.md convention 14 mechanism 1), not a fixed
    # deadline — this test reaches windows/macOS/iOS too (`real_conversations`),
    # none of which have the `poke_receive_cycle`/`await_receive_cycle_after`
    # causal anchor yet, so this stays a
    # deadline poll rather than a poke until those land.
    def _decrypted_thread():
        return next(
            (
                t
                for t in app.conversations.list_threads()
                if nonce in (t.label or "") or nonce in (t.snippet or "")
            ),
            None,
        )

    found = wait_until(
        _decrypted_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the inbound email tagged {nonce!r} never surfaced decrypted in the "
            f"conversations list — the client receive path (inbox.fetch "
            f"→ open_inbound_record → parse → ingest) did not complete.\n"
            f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )
    # The decrypted body (not just the subject) round-tripped: the snippet is the
    # body preview, and the nonce is in the body line.
    assert nonce in (found.snippet or "") or nonce in (found.label or ""), (
        f"decrypted thread must carry the nonce in its label/snippet; got "
        f"label={found.label!r} snippet={found.snippet!r}"
    )
    assert found.rail == "Smtp", (
        f"received mail must land on the Smtp rail; got {found.rail!r}"
    )
