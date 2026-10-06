"""tier_2 e2e: no control character from remote-authored content reaches the
terminal — the render-funnel gate.

``docs/goal/architecture/apps/tui.md`` § Rendering owns the claim; the primitive
is ``fauna_core::control_chars::strip_control_chars``
(``apps/observability.md`` § Ring-ingest sanitization).

**What the defect was.** tui painted remote-authored strings straight into its
cell grid, and a probe established ratatui preserves ESC/BEL verbatim into that
grid — so anyone whose content rendered in a tui user's feed could drive
ANSI/OSC sequences into that user's terminal (clear the screen, set the window
title, rewrite what the reader believes they are looking at). The best source
needs no Fauna account at all: a third-party ``og:title``, since the nest's
link-preview parser applies only ``str::trim`` plus a char-count cap.

**Why the assertion is on the pty stream and not on an element.** The automation
agent answers from the per-frame *element registry*, which sits **upstream** of
paint — ``get_text`` returns ``Element::text``, so it cannot see what reached the
terminal, and a fix could pass an element-level assertion while still emitting
escapes. The tui driver owns the pty master and therefore *is* the terminal
emulator (``drivers/tui.py`` § the driver is the terminal emulator), so
``frame_stream_path()`` is the actual byte stream a terminal would interpret.
That is the strongest available witness for this property, and the only one
downstream of the funnel.

**The two assertions are deliberately opposite, and that is what makes this test
non-vacuous.** ``get_text`` must still show the *raw* payload (the strip is at
paint, not at ingest), proving the malicious bytes genuinely arrived and were
genuinely rendered; the stream must NOT contain the OSC introducer, proving the
funnel neutralised them. A test that only asserted absence would pass
identically if the seed never landed, the card never painted, or the driver were
broken — the failure mode this file's own queue has recorded six times.

**Both source classes, one assertion.** The og-metadata sink (``link-preview-title``,
a flat text element) and the post body (``feed-post-text``, walked through
``document::render_document`` — a *different* code path to the same grid) are
checked by the same scan, so a per-source fix fails this test. That is required:
the source list is open-ended, and the og class was found one commit after the
first two.

tui-only by structure: no other app has a terminal to inject into.
"""

import time
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier_2, pytest.mark.tui]

# An OSC window-title set, wrapped in a screen-clear and cursor-home. `\x1b]0;`
# is the discriminating half: the app's ONLY legitimate OSC is iTerm2's
# `\x1b]1337;` inline-image introducer (`apps/fauna-tui/src/graphics/iterm2.rs`),
# so `\x1b]0;` appearing in the stream can only have come from content.
#
# Note the printable *remainder* ("]0;pwned-og…") survives the strip as inert
# text — that is correct and is why the negative assertion keys on the ESC byte
# rather than on the word "pwned".
_OSC_INTRODUCER = b"\x1b]0;"
# ⚠ The innocent tails are deliberately SPACE-FREE. ratatui's renderer emits runs
# of *changed* cells, and an unchanged space between two words is skipped — so a
# painted "Innocent Headline" reaches the pty as `Innocent`, a cursor-move escape,
# then `Headline`, and searching the stream for the phrase never matches. A
# single token is written as one contiguous run.
_OG_PAYLOAD = "\x1b[2J\x1b[H\x1b]0;pwned-og\x07InnocentHeadline"
_BODY_PAYLOAD = "\x1b]0;pwned-body\x07InnocentBodyText"

# The stripped remainder of the og title, as it must appear on screen (the strip
# leaves the escape's printable tail as inert text, so the run reads
# `]0;pwned-ogInnocentHeadline`). Waiting for this is the **causal barrier** for
# the negative assertions below (convention 14): it proves the card actually
# painted, so "no OSC in the stream" cannot pass by the frame never having been
# drawn.
_PAINTED_MARKER = b"InnocentHeadline"

# Two posts, one per sink — they cannot be combined. The shared producer emits a
# `LinkPreview` block only for a **standalone** bare url (`test_feed_link_preview.py`),
# so prefixing post 0's body with the second payload silently suppresses the card
# and leaves the og assertion testing nothing. Both posts paint on the same feed
# page and are covered by the same stream scan below, which is what keeps "one
# assertion over both sinks" true.
_POSTS = [
    {
        # The og-metadata sink: a hostile `og:title` on a resolved preview card.
        # Body is exactly the bare url, nothing else.
        "post_id": "c" * 64,
        "author": "3" * 64,
        "body": "[https://example.com/article](https://example.com/article)",
        "link_previews": [
            {
                "url": "https://example.com/article",
                "title": _OG_PAYLOAD,
                "description": "A short description.",
                "image_hash": "ab" * 32,
            }
        ],
    },
    {
        # The non-og sink: a hostile post body, which reaches the grid through
        # `document::render_document` — a different code path to the same cells.
        "post_id": "d" * 64,
        "author": "4" * 64,
        "body": _BODY_PAYLOAD,
    },
]


def _wait_for_bytes(path, needle: bytes, timeout: float = 30.0) -> bytes:
    """Poll the client's terminal stream until ``needle`` appears; return it all.

    Generous budget, deadline-polled: a green run returns on the first frame that
    carries the marker and pays nothing for the ceiling (convention 14).
    """
    deadline = time.monotonic() + timeout
    stream = b""
    while time.monotonic() < deadline:
        stream = Path(path).read_bytes()
        if needle in stream:
            return stream
        time.sleep(0.2)
    return stream


def test_remote_authored_content_reaches_the_terminal_with_no_control_characters(
    logged_in_app,
):
    """A hostile ``og:title`` and a hostile post body both render, and neither
    puts a control character on the terminal."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # (1) Both payloads really arrived and really rendered. The strip is at PAINT,
    # so the elements still carry the raw bytes — if either assertion fails, that
    # sink never made it onto the page and the stream checks below would be vacuous
    # for it. This guard has already earned its keep: it caught a first cut of this
    # test whose post 0 body suppressed the preview card outright.
    for element_id, sink in (
        ("link-preview-title", "og:title"),
        ("feed-post-text", "post body"),
    ):
        scope = "post-card[0]" if element_id == "link-preview-title" else "post-card[1]"
        text = app.driver.get_text(element_id, scope=scope)
        assert "\x1b" in text, (
            f"the {sink} element text should still carry the RAW payload — the gate "
            "is at the paint funnel, not at ingest. Without an escape here this "
            f"test proves nothing about that sink. got {text!r}; "
            f"error={app.error_text()!r}"
        )

    # (2) The causal barrier: wait until the stripped title has actually painted.
    stream = _wait_for_bytes(app.driver.frame_stream_path(), _PAINTED_MARKER)
    assert _PAINTED_MARKER in stream, (
        "the link-preview title never painted, so the absence check below would "
        f"be vacuous. stream tail: {stream[-400:]!r}"
    )

    # (3) The security property: no OSC introducer from content reached the
    # terminal. Covers both sinks — the flat og-title element and the walked
    # document body — in one scan, so a per-source fix fails here.
    assert _OSC_INTRODUCER not in stream, (
        "a control sequence from remote-authored content reached the terminal: an "
        "OSC window-title set the app never emits itself. Remote content can "
        "rewrite the reader's screen. "
        f"first occurrence at byte {stream.find(_OSC_INTRODUCER)} of {len(stream)}"
    )
    # The exact payload bytes of each sink, checked individually so a failure says
    # WHICH source leaked. These strings are unique to this test's seed, so
    # nothing in the app or in ratatui's own output could produce them — no
    # false-positive risk, unlike a generic CSI sequence (`ESC [ 2J` is what
    # crossterm's own screen-clear emits, so it is not usable as a signal).
    for sink, payload in (
        ("og:title (flat element text)", _OG_PAYLOAD),
        ("post body (walked document)", _BODY_PAYLOAD),
    ):
        raw = payload.encode()
        assert raw not in stream, (
            f"the {sink} sink put its control sequence on the terminal verbatim — "
            "the render funnel did not strip it. A per-source fix that covered "
            "only the other sink would fail exactly here. "
            f"byte {stream.find(raw)} of {len(stream)}"
        )
