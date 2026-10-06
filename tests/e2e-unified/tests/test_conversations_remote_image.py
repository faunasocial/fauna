"""html-mail Slice 3 (web): inbound markdown bodies render remote images BLOCKED by
default — never auto-fetched — until a per-message ``load-remote-content-button``
reveals them.

The privacy perimeter (``docs/goal/behavior/html-mail.md`` § Security & privacy):
the blocked ``<img>`` carries ``data-remote-src`` and **no** ``src``, so the browser
issues no network request; only the user's click flips that one message to fetch
mode (web re-renders the bubble via ``markdownToHtml`` instead of
``markdownToHtmlBlocked``). The button shows iff ``countRemoteImages(body) > 0`` and
the message has not been revealed.

Driven through the conversations inject seam on the markdown-capable FaunaMls rail
(→ ``BodyFormat::Markdown``), exercising the same per-``body_format`` bubble renderer
and reveal path as production. The native apps prove their image-render leg via a
real ``text/html`` mail (``test_mail_html_roundtrip.py``); this is the web inject-seam
twin. Web and windows run this inject-seam twin (windows landed the blocked-image
reveal in html-mail Slice 3 — DmMessageBubble + ``load-remote-content-button``,
ui-actual-windows.yaml); the other natives prove the leg via their real
``text/html`` mail e2e.

``tui`` adopts the twin too (render-model.md § D3 — its ``conversation_detail``
bubble was the one app still deferring ``load-remote-content-button``). The
``<img>``-attribute probes above are ``is_web()``-gated, so what a terminal
asserts is the part that is actually cross-app: a body with one blocked remote
image surfaces exactly one per-message reveal button, and revealing clears it.
"""

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.tui,
]

# One remote ![]() image → exactly one blocked placeholder + the reveal button.
REMOTE_IMG_URL = "https://example.com/cat.png"
REMOTE_IMG_BODY = f"Before ![cat]({REMOTE_IMG_URL}) after"


@pytest.mark.feature("email-in-conversations")
def test_remote_image_blocked_until_reveal(logged_in_app):
    """An inbound markdown body with a remote image renders BLOCKED (button shown,
    ``<img>`` has no ``src``) and reveals only on the per-message button click."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="carol-remoteimg@self-nest.test",
        subject=None,
        body=REMOTE_IMG_BODY,
    )

    assert d.count("dm-message-text") >= 1, "no message bubble on select"
    # Body carries one remote image → the per-message reveal button is shown.
    assert (
        d.count("load-remote-content-button") == 1
    ), "blocked-image reveal button not shown for a body with one remote image"

    # Privacy perimeter: the blocked <img> carries data-remote-src and NO src, so
    # the browser issued no request. Web-specific DOM probe (other apps assert
    # the no-fetch via their own render layer in their real-mail e2e).
    if d.is_web():
        before = d.eval_js(
            "(() => { const i = document.querySelector("
            "'.bubble-body img.blocked-remote-image');"
            " return i ? { hasSrc: !!i.getAttribute('src'),"
            " remote: i.getAttribute('data-remote-src') } : null; })()"
        )
        assert before == {"hasSrc": False, "remote": REMOTE_IMG_URL}, before

    # Reveal this message → the button clears and the image re-renders in fetch
    # mode (<img src>), the web twin of native "re-render the image spans in fetch".
    conv.reveal_remote_content(0)
    assert (
        d.count("load-remote-content-button") == 0
    ), "reveal button did not clear after loading remote content"

    if d.is_web():
        after = d.eval_js(
            "(() => { const i = document.querySelector('.bubble-body img');"
            " return i ? { hasSrc: !!i.getAttribute('src'),"
            " src: i.getAttribute('src') } : null; })()"
        )
        assert after and after["hasSrc"] and after["src"] == REMOTE_IMG_URL, after
