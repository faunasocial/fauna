"""tier_3: the HTML-email markdown-interchange round-trip, on the linux app.

Proves `docs/goal/behavior/html-mail.md` success-item 3 end to end through the
real stack — both directions of the "markdown is the single interchange format"
design (Slice 1, shared Rust):

  OUTBOUND  Compose **markdown** in the (now-ungated) mail compose toolbar and
            send to an external recipient. The relayed RFC 5322 bytes the stub
            external MX receives are `multipart/alternative` carrying BOTH a
            `text/plain` part (the markdown source verbatim) AND a `text/html`
            part with the markdown rendered to HTML — both `<strong>…</strong>`
            (`**bold**`) AND `<em>…</em>` (underscore `_italic_`, the form htmd and
            the macOS/windows toolbars emit). Proves `rfc5322::build_message` +
            `fauna_core::markdown::markdown_to_html`.

  INBOUND   An external peer delivers a genuine `text/html` email. It surfaces
            in the conversations list, and opening it renders the body as
            FORMATTED markdown in `dm-message-text` — no raw `<p>`/`<strong>`
            tags, no literal `**` markers. Proves
            `smtp.rs::inbound_record_to_message` →
            `html_markdown::inbound_mail_body` (HTML→markdown +
            `BodyFormat::Markdown`) and the linux `message_bubble` Markdown
            render arm.

Both halves run against ONE claimed + mail-enabled nest so the heavy claim/
enable UI dance (`helpers.mail_client_ui.claim_enable_and_ready`) is paid once —
mirroring `test_mail_client_full_roundtrip`'s send+receive-in-one-function shape.

⚠ This test deliberately uses a REAL send (outbound) and a REAL inbound SMTP
delivery (inbound), NOT the linux mock-backend inject seam: that seam
(`apps/fauna-linux/src/main.rs` `inject_inbound_..._for_test`) hard-codes the
mail rail to `BodyFormat::PlainText`, so it cannot exercise the HTML→markdown
path at all. Only a real inbound message flows through `inbound_mail_body`.

Test taxonomy: tier_3 (every binary real, real SMTP wire in BOTH directions,
real seal → client fetch → parse → render).
"""

import time

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.mail_client_ui import claim_enable_and_ready, deliver_inbound

# `real_conversations`: a native app (windows, macOS/iOS) starts the REAL shared-Rust
# `ConversationsSession` receive loop only under `FAUNA_E2E_REAL_CONVERSATIONS`
# (windows `App.xaml.cs` `BuildE2eConvSessionAsync`), which conftest
# `_apply_real_conversations_env` sets only when a collected test carries this
# marker. Unmarked, windows kept its mock `ConversationsManagerHost`, so the REAL
# inbound delivery below never surfaced (`threads: []`) — while linux, which runs
# the real session for every e2e login, passed. The flag is session-wide: run this
# module in its own invocation (the sibling `test_mail_client_receive.py` shape).
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# The (external) sender/recipient domain — routed to the stub MX by the bridge's
# mta_mx_override on outbound, and (on inbound) with no loopback/local-domain
# exemption so a delivery arrives as genuine external mail.
EXTERNAL_DOMAIN = "external.test"


@pytest.mark.feature("email-in-conversations")
def test_html_mail_markdown_roundtrip(app, unclaimed_mail_nest_ui):
    """Compose markdown → sent as multipart/alternative with a rendered
    text/html part; receive a real text/html email → rendered as formatted
    markdown (not raw tags). The two halves prove the bidirectional
    markdown-interchange feature on the lead app."""
    if not (app.driver.is_linux() or app.driver.is_macos() or app.driver.is_ios()):
        skip_unbuilt(
            app.driver,
            surface="the HTML-mail markdown round-trip (render + compose toolbar)",
            detail="wired on linux first (the lead client whose render + "
            "compose toolbar are verified working); the other apps are the "
            "cross-app follow-on",
            tracked="mail-settings.md",
        )

    nest = unclaimed_mail_nest_ui
    ctx = claim_enable_and_ready(app, nest, credential_kind="oauthbearer")
    domain = ctx["domain"]
    handle = ctx["handle"]

    # ── OUTBOUND: compose markdown → relayed multipart/alternative + text/html ──
    out_nonce = f"htmlout{int(time.time() * 1000)}qx"
    recipient = f"bob@{EXTERNAL_DOMAIN}"
    # The body carries the nonce (for stub-MX matching) and a markdown **bold**
    # AND an _italic_ span that must appear rendered to HTML in the text/html
    # alternative AND as the markdown source in the text/plain alternative. The
    # italic is deliberately the UNDERSCORE form — that is what the inbound
    # HTML→markdown converter (htmd) and the macOS/windows toolbars emit, and the
    # bug this guards against was the shared renderer ignoring `_…_` entirely.
    bold_token = f"bold-{out_nonce}"
    italic_token = f"italic-{out_nonce}"
    app.conversations.start_new_conversation(
        recipient,
        subject=f"HTML out {out_nonce}",
        body=f"Outbound {out_nonce} with **{bold_token}** and _{italic_token}_ body.\n",
    )

    deadline = time.monotonic() + 90.0
    relayed = None
    while time.monotonic() < deadline and relayed is None:
        for raw in nest.stub_mx.messages():
            if out_nonce.encode() in raw:
                relayed = raw
                break
        if relayed is None:
            time.sleep(0.5)

    _threads = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert relayed is not None, (
        f"the stub external MX never received the client-sent markdown message "
        f"{out_nonce!r}. threads={_threads}; conversations error: "
        f"{app.error_text()!r}; {nest.bridge_log_hint()}"
    )

    sent = relayed.decode("utf-8", "replace")
    # The outbound serializer builds multipart/alternative (rfc5322::build_message).
    assert "multipart/alternative" in sent, (
        f"outbound mail is not multipart/alternative — build_message did not run; "
        f"raw:\n{sent}"
    )
    assert "Content-Type: text/plain" in sent and "Content-Type: text/html" in sent, (
        f"multipart/alternative must carry both a text/plain and a text/html part; "
        f"raw:\n{sent}"
    )
    # markdown → HTML happened in the text/html part (markdown_to_html).
    assert f"<strong>{bold_token}</strong>" in sent, (
        f"the markdown **{bold_token}** was not rendered to <strong> in the "
        f"text/html alternative — markdown→HTML (markdown_to_html) did not run; "
        f"raw:\n{sent}"
    )
    # underscore italic must render too (the regression: the shared parser only
    # knew `*…*`, so `_italic_` went out as literal underscores on the wire).
    assert f"<em>{italic_token}</em>" in sent, (
        f"the markdown _{italic_token}_ was not rendered to <em> in the text/html "
        f"alternative — underscore italic emphasis was dropped by the shared "
        f"renderer (fauna_core::markdown); raw:\n{sent}"
    )
    # the markdown source survives verbatim in the text/plain part.
    assert f"**{bold_token}**" in sent, (
        f"the markdown source **{bold_token}** is missing from the text/plain "
        f"alternative; raw:\n{sent}"
    )

    # ── INBOUND: deliver a genuine text/html email → rendered FORMATTED ─────────
    in_nonce = f"htmlin{int(time.time() * 1000)}qx"
    inbound = (
        "\r\n".join([
            f"From: Carol <carol@{EXTERNAL_DOMAIN}>",
            f"To: {handle}",
            f"Subject: HTML in {in_nonce}",
            f"Message-ID: <{in_nonce}@{EXTERNAL_DOMAIN}>",
            "Date: Mon, 08 Jun 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/html; charset=utf-8",
            "",
            f"<p>Hello <strong>{in_nonce}</strong> and <em>italic-{in_nonce}</em> world</p>",
        ]) + "\r\n"
    ).encode()
    deliver_inbound(
        nest.mx_port, domain, f"carol@{EXTERNAL_DOMAIN}", handle, inbound,
        time.monotonic() + 40.0,
    )

    # The received HTML mail surfaces decrypted in the conversations list.
    deadline = time.monotonic() + 60.0
    found = None
    while time.monotonic() < deadline and found is None:
        for t in app.conversations.list_threads():
            if in_nonce in (t.label or "") or in_nonce in (t.snippet or ""):
                found = t
                break
        if found is None:
            time.sleep(1.0)
    assert found is not None, (
        f"the inbound text/html email {in_nonce!r} never surfaced in the "
        f"conversations list — the client receive path did not complete.\n"
        f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
        f"  conversations error: {app.error_text()!r}; {nest.bridge_log_hint()}"
    )
    assert found.rail == "Smtp", (
        f"received mail must land on the Smtp rail; got {found.rail!r}"
    )

    # Open the thread and read the rendered message bubble. The body must render
    # FORMATTED through the markdown path (inbound_mail_body stamped Markdown):
    # the `<strong>`/`<p>` tags are gone (HTML→markdown) and the `**` markdown
    # markers are stripped by the renderer (message_bubble Markdown arm). A
    # PlainText/Html stamp would instead show raw tags or literal markers.
    app.conversations.open_thread_for_sender(in_nonce)
    n = app.driver.count("dm-message-text")
    assert n >= 1, "no message bubble rendered in the opened inbound thread"
    rendered = None
    for i in range(n):
        t = app.driver.get_text("dm-message-text", index=i) or ""
        if in_nonce in t:
            rendered = t
            break
    assert rendered is not None, (
        f"the inbound message body carrying {in_nonce!r} did not render in any "
        f"dm-message-text bubble; bubbles: "
        f"{[app.driver.get_text('dm-message-text', index=i) for i in range(n)]!r}"
    )
    assert "<strong>" not in rendered and "<p>" not in rendered, (
        f"inbound HTML rendered as RAW TAGS — inbound_mail_body did not convert "
        f"HTML→markdown (or the bubble stamped Html/PlainText): {rendered!r}"
    )
    assert "**" not in rendered, (
        f"inbound body shows literal `**` markdown markers — the bubble did not "
        f"render through the Markdown arm (body_format != Markdown): {rendered!r}"
    )
    # The <em> arrived as htmd `_italic-…_`; a correct render strips the markers.
    # Literal `_italic-` means the shared renderer ignored underscore emphasis.
    assert f"italic-{in_nonce}" in rendered and f"_italic-{in_nonce}" not in rendered, (
        f"inbound <em> rendered as literal `_…_` — the shared markdown renderer "
        f"did not understand htmd's underscore italic: {rendered!r}"
    )


@pytest.mark.feature("email-in-conversations")
def test_html_mail_remote_image_blocked_until_reveal(app, unclaimed_mail_nest_ui):
    """html-mail Slice 3: an inbound HTML email with a remote ``<img>`` renders
    its image BLOCKED by default — no auto-fetch — surfacing a per-message
    ``load-remote-content-button``; clicking it reveals the image for that
    message only.

    Proves `docs/goal/behavior/html-mail.md` § Rendering + § Security & privacy
    end to end on the lead app (linux, which owns this e2e):

      - inbound ``<img src="https://…" alt="…">`` → HTML→markdown keeps it as a
        remote ``![alt](url)`` ref (`html_markdown::inbound_mail_body`), the body
        is stamped ``BodyFormat::Markdown``, and the linux ``message_bubble``
        Markdown arm renders a blocked placeholder + the reveal button (never an
        auto-fetched image — the privacy perimeter);
      - the button is visible BECAUSE the message is in the blocked state (it is
        shown only when ``count_remote_images > 0`` and not yet revealed), so its
        presence is the "blocked, not fetched" signal and its disappearance after
        the click is the "revealed" signal — both assertable without screenshots.

    Test taxonomy: tier_3 (real nest + bridge, real inbound SMTP wire, real seal
    → client fetch → HTML→markdown → render). Uses a REAL inbound delivery, not
    the linux mock inject seam (which hard-codes PlainText — see the module
    docstring), so the HTML→markdown image path actually runs.
    """
    if not (
        app.driver.is_linux() or app.driver.is_windows()
        or app.driver.is_macos() or app.driver.is_ios()
    ):
        # macos/ios shipped the shared-FaunaKit render (MarkdownBodyView /
        # DmMessageBubble). The apple mail-enable gaps this gate used to cite
        # (macOS enable-toggle crash; iOS settings-subpage nav) are dated
        # 2026-06-13 and `mail-settings.md` § Implementation status today now
        # records the apple mail-enable path fixed 2026-07-13 with the
        # dedicated-mail-nest tier_3 family green on `--client macos` —
        # re-verify with a real run before trusting the old comment.
        skip_unbuilt(
            app.driver,
            surface="html-mail inline-image render",
            detail="landed on linux (lead, sets the "
            "placeholder + reveal-button shape) + windows; macos/ios shipped the "
            "render but are blocked upstream by apple mail-enable gaps (see comment "
            "above); web + android are the cross-area follow-on work",
            tracked="mail-settings.md",
        )

    nest = unclaimed_mail_nest_ui
    ctx = claim_enable_and_ready(app, nest, credential_kind="oauthbearer")
    domain = ctx["domain"]
    handle = ctx["handle"]

    # A genuine text/html inbound email carrying a remote image. The non-empty
    # `alt` is required: the shared parser (and `count_remote_images`, the gate)
    # treats `![](url)` with an empty alt as NOT an image, matching this client.
    in_nonce = f"htmlimg{int(time.time() * 1000)}qx"
    img_url = f"https://img.test/pixel-{in_nonce}.png"
    inbound = (
        "\r\n".join([
            f"From: Dave <dave@{EXTERNAL_DOMAIN}>",
            f"To: {handle}",
            f"Subject: HTML image {in_nonce}",
            f"Message-ID: <{in_nonce}@{EXTERNAL_DOMAIN}>",
            "Date: Mon, 08 Jun 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/html; charset=utf-8",
            "",
            f"<p>Pixel {in_nonce} below</p>"
            f'<img src="{img_url}" alt="pixel {in_nonce}">',
        ]) + "\r\n"
    ).encode()
    deliver_inbound(
        nest.mx_port, domain, f"dave@{EXTERNAL_DOMAIN}", handle, inbound,
        time.monotonic() + 40.0,
    )

    # The received mail surfaces in the conversations list.
    deadline = time.monotonic() + 60.0
    found = None
    while time.monotonic() < deadline and found is None:
        for t in app.conversations.list_threads():
            if in_nonce in (t.label or "") or in_nonce in (t.snippet or ""):
                found = t
                break
        if found is None:
            time.sleep(1.0)
    assert found is not None, (
        f"the inbound text/html email {in_nonce!r} never surfaced in the "
        f"conversations list — the receive path did not complete.\n"
        f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
        f"  conversations error: {app.error_text()!r}; {nest.bridge_log_hint()}"
    )

    # Open the thread. The image MUST render blocked — i.e. the per-message
    # `load-remote-content-button` is present (it shows only for not-yet-revealed
    # remote images). If the client had auto-fetched, the button would be absent.
    app.conversations.open_thread_for_sender(in_nonce)
    assert app.driver.count("dm-message-text") >= 1, (
        "no message bubble rendered in the opened inbound image thread"
    )
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and app.driver.count("load-remote-content-button") == 0:
        time.sleep(0.5)
    blocked_count = app.driver.count("load-remote-content-button")
    assert blocked_count >= 1, (
        f"the inbound remote image did not render BLOCKED — no "
        f"load-remote-content-button surfaced, so either the image was "
        f"auto-fetched (privacy regression) or HTML→markdown dropped the "
        f"![]() ref. button count={blocked_count}; conversations error: "
        f"{app.error_text()!r}"
    )

    # Reveal: clicking the button loads the remote content for THIS message; the
    # button then disappears (nothing left to reveal) — the blocked state clears.
    app.conversations.reveal_remote_content()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and app.driver.count("load-remote-content-button") > 0:
        time.sleep(0.5)
    after_count = app.driver.count("load-remote-content-button")
    assert after_count == 0, (
        f"after clicking load-remote-content-button the blocked state did not "
        f"clear — the button is still present (count={after_count}); the reveal "
        f"did not flip the message to fetch mode"
    )
