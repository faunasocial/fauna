"""tier_3: the client-UI proof of the mail reference-reader half — the
counterpart, at the client-feed leg, of
`test_mail_inbound_to_imap.py::test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte`
(the IMAP/MDA leg).

A real inbound email too large to ride inline on `fauna.email.inbox.fetch`
(the sealed stored envelope exceeds `INLINE_MAIL_REQUEST_BUDGET_BYTES`, ~2
MiB) is delivered through the real Go MTA bridge. The nest's client-feed
reference leg (`bins/fauna-nest/src/email_handlers.rs::mailbox_fetch_handler`,
`mail-message-size.md` § The client-feed leg) stages the envelope on the byte plane
and ships an `InboxMessage.body_ref` instead of the inline envelope. The
client's shared receive loop resolves it
(`fauna_mail::body_ref::resolve_referenced_mail_body`, native
`NestPublicChunkFetcher` / wasm `resolveMailBodyRef` — the reader half) — GETting the referenced chunks over the open download route,
rejoining them, and opening the result exactly as an inline `sealed_envelope`
— then feeds the shared `ConversationsManager` for display, exactly as an
inline-sized message would.

This is deliberately real end to end (no `inject_inbound_for_test` shortcut,
`green-test-or-it-doesnt-work`): an over-frame envelope can reach
`inbox.fetch` ONLY by reference (see the handler's `over_frame` branch), so the
message surfacing at all in the client's thread list is already proof the
reference leg ran. HPKE-AEAD authenticates the sealed envelope as a whole, so
any dropped/reordered/corrupted chunk in the rejoin would fail the open
outright rather than render garbled content — the decrypted thread appearing
with an intact head AND tail marker several megabytes apart is the
byte-for-byte proof (mirrors the IMAP sibling's own "getting the last marker
back means every chunk before it landed, in order" reasoning).

Together with `test_mail_client_receive.py` (the inline-sized case) this
completes the client-UI receive proof across both wire shapes the client-feed
leg can produce.

Goal docs: `mail-message-size.md` § The client-feed leg — the OWNER of the
`mail-message-size` concept since 2026-08-03, when `smtp-server.md` § Message
size limits became a pointer stub to it (`smtp-server.md:368-374`); that doc's
`## Implementation status today` is what records this app-UI proof as owed.
`mail-app-surface.md` § Inbound client receive owns the `inbox.fetch` contract,
and `imap-server.md` § Body size on the MDA↔nest wire is the sibling MDA leg
this test mirrors at the client-feed leg.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP wire, real
  MTA-seal → stage-on-byte-plane → client chunk-fetch → rejoin → open →
  parse → ingest → render.
"""

import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until
from helpers.mail_aliases import add_exact_alias

# Same client set as the inline-sized sibling `test_mail_client_receive.py` —
# the reader half reached every app unconditionally, and
# this exercises the same shared receive loop, just with an over-frame body.
# `real_conversations` drives the native launch flag for windows/macos/ios;
# android carries the same real-session wiring (see the sibling file's
# comment) — its `--client android` run is emulator-host-gated like every android
# e2e track .
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

# Generous budget for the larger round trip this test drives, beyond the
# inline-sized sibling's `MLS_HANDSHAKE_S` (90s): real SMTP delivery of a ~3
# MiB body, nest-side byte-plane staging, the client's chunk-fetch + rejoin +
# HPKE-open + RFC 5322 parse, then ingest into the shared ConversationsManager
# (e2e-conventions.md convention 14 mechanism 1 — a named generous budget, not
# a fixed sleep).
OVER_FRAME_RECEIVE_BUDGET_S = 180.0

# The bubble's own budget, separate from the receive budget above because it
# measures a different thing: not the wire, but one app opening and laying out
# a ~3 MiB single message body.
#
# ⚠ This is NOT a formality. **Measured on windows 2026-09-11: the bubble
# appears 40.7 s after the thread is opened** — the thread row, its subject and
# its snippet are all on screen long before the body is. Before this budget
# existed the test read `count("dm-message-text")` immediately after
# `open_thread_with_subject`, whose internal wait polls `thread-header` — page
# chrome that renders in well under a second — and RETURNS SILENTLY when its
# 5 s elapses (`actions/conversations.py` `_wait_thread_open`). So the count was
# taken ~5 s into a ~41 s render and was deterministically 0: a 100 %
# reproducible red that looked exactly like a broken rejoin and was in fact the
# "wait for the state you ASSERT, never for a proxy that renders earlier" trap
# (`e2e-latency-independent-assertions.md`). web and tui never hit it because their render is effectively
# instant; the three native apps all did.
#
# 90 s is ~2.2× that measured worst case — generous, and a deadline poll rather
# than a sleep (convention 14 mechanism 1). It is a TEST budget, not a
# statement that a 41 s open is acceptable product behaviour. That gap closed
# on 2026-09-15: windows paints a large body as virtualized 16-line runs and no
# longer rebuilds bubbles per observer tick, and the bubble is there when the
# thread-open step returns (`mail-message-size.md` § Implementation status
# today); the budget stays as the bound the reported time is measured against.
BUBBLE_RENDER_BUDGET_S = 90.0


def _deliver_inbound(mx_port: int, server_name: str, recipient_addr: str,
                     raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener. Returns after the `250` on `.`, which the MTA
    sends only once the WS-RPC `ingest_inbound_mail` (seal + stage + store)
    committed."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("email-in-conversations")
def test_client_driven_receive_renders_over_frame_reference_body(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    app = logged_in_app

    # ── 1. Enable mail on the logged-in user (mints the MSEK, registers the
    # matching recipient pubkey on the nest — mirrors
    # `test_mail_client_receive.py`).
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # ── 2. Register the inbound routing alias for this actor via the
    # production Admin WS-RPC path (a distinct local part from the fixture's
    # own default recipient).
    domain = mail_bridge_mta.domain
    local_part = "e2eoverframe"
    recipient_addr = f"{local_part}@{domain}"
    add_exact_alias(nest_instance["url"], test_user["signing_key"], domain, local_part)

    # ── 3. Build a ~3 MiB inbound message — comfortably over
    # `INLINE_MAIL_REQUEST_BUDGET_BYTES` (2 MiB − 64 KiB) once sealed, so the
    # stored envelope can ONLY reach `fauna.email.inbox.fetch` by reference
    # (`test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`
    # pins this exact magnitude at the SMTP-acceptance leg). Nonce markers at
    # both ends of the filler: recovering the TAIL marker proves every byte
    # before it survived the reference round trip in order. No line begins
    # with "." so SMTP dot-stuffing is a no-op and the bytes on the wire are
    # the bytes the client must render.
    nonce = f"overframe{int(time.time() * 1000)}qx"
    subject = f"Client-driven over-frame inbound {nonce}"
    message_id = f"<{nonce}@{SENDER_DOMAIN}>"
    filler_line = "z" * 76 + "\r\n"
    filler = filler_line * ((3 * 1024 * 1024) // len(filler_line))
    body = f"{nonce}-HEAD\r\n{filler}{nonce}-TAIL\r\n"
    header_lines = [
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "",
    ]
    raw_message = ("\r\n".join(header_lines) + body).encode()
    assert len(raw_message) > 2 * 1024 * 1024, (
        "the probe must exceed the 2 MiB WS-RPC frame budget — otherwise it "
        "would ride inline and prove nothing about the client-feed reference leg"
    )

    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, raw_message,
        time.monotonic() + 120.0,
    )

    # ── 4. The shared receive loop fetches the referenced envelope, resolves
    # the chunks, opens both crypto layers, parses the RFC 5322, and feeds the
    # shared ConversationsManager on its own. Poll `list_threads` until the
    # DECRYPTED subject carrying the nonce surfaces — its appearance alone
    # proves the reference leg ran (an over-frame envelope with the gate off
    # is skipped by the server, never reaching `inbox.fetch` at all).
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
        OVER_FRAME_RECEIVE_BUDGET_S,
        diagnose=lambda: (
            f"the over-frame inbound email tagged {nonce!r} never surfaced "
            f"decrypted in the conversations list — the client-feed reference "
            f"leg (stage → body_ref → chunk-fetch → rejoin → open → ingest) "
            f"did not complete.\n"
            f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )
    assert found.rail == "Smtp", (
        f"received mail must land on the Smtp rail; got {found.rail!r}"
    )

    # ── 5. Open the thread and read the rendered message bubble. Both the
    # HEAD and TAIL markers surviving in the SAME bubble — several megabytes
    # apart in the original body — is the byte-for-byte proof: HPKE-AEAD
    # authenticates the sealed envelope as a whole, so a dropped, reordered,
    # or corrupted chunk in the rejoin would fail the open outright (no
    # partial/garbled render is possible), never merely truncate the tail.
    app.conversations.open_thread_with_subject(subject)
    opened = time.monotonic()
    n = wait_until(
        lambda: app.driver.count("dm-message-text") or None,
        BUBBLE_RENDER_BUDGET_S,
        diagnose=lambda: (
            f"no message bubble rendered in the opened over-frame thread "
            f"within {BUBBLE_RENDER_BUDGET_S}s.\n"
            f"  {app.driver.diagnose('dm-message-text')}\n"
            f"  {app.driver.diagnose('thread-header')}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  threads: {[(t.label, t.snippet, t.rail, t.message_count) for t in app.conversations.list_threads()]}"
        ),
    )
    # Reported, never asserted (the budget above is the only bound): the
    # per-app open cost is an open product question in `mail-message-size.md`
    # § Implementation status today, and this line is how it gets measured.
    print(f"[over-frame] bubble rendered {time.monotonic() - opened:.1f}s after the thread opened")
    rendered = None
    for i in range(n):
        t = app.driver.get_text("dm-message-text", index=i) or ""
        if f"{nonce}-HEAD" in t:
            rendered = t
            break
    assert rendered is not None, (
        f"the over-frame message body carrying {nonce!r}-HEAD did not render in "
        f"any dm-message-text bubble; bubble count: {n}"
    )
    assert f"{nonce}-TAIL" in rendered, (
        "the TAIL marker is missing from the rendered bubble — the chunk "
        "rejoin dropped or truncated the tail of the referenced body "
        f"(rendered length: {len(rendered)} chars, expected body: {len(body)} bytes)"
    )
    # The rendered text must be close to the full ~3 MiB body, not a short
    # truncated preview — a generous floor tolerant of CRLF/whitespace
    # normalization in the render path.
    assert len(rendered) > 3_000_000, (
        f"the rendered bubble is far shorter than the ~3 MiB sent body "
        f"({len(rendered)} chars) — the client displayed a truncated preview "
        "instead of the whole message"
    )
