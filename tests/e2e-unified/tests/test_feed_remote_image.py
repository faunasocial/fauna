"""Cross-app Feed — the D3 body remote image (``doc-remote-image``).

``docs/goal/architecture/render-model.md`` § D3; ``docs/goal/architecture/apps/tui.md``
§ Rendering; ``tests/e2e-unified/ui.yaml`` ``doc-remote-image`` (indexed, inside
``post-card``).

A markdown ``![alt](https://…)`` in a post body is a shared
``RenderBlock::RemoteImage``, **blocked by default**: loading it would tell a third
party that this reader opened the post, so nothing is requested until the reader taps
the post's own ``load-remote-content-button``. The manager owns the reveal set and
projects ``revealed`` onto the block (no per-app dictionary); the app paints the
element from that one signal.

The element registers in **every** state under the same id — blocked, revealed but
still loading, painted — which is what makes the assertion below about the element's
*content* rather than its existence. An absence assertion against an element that
never registered would pass for the wrong reason (and did, in an earlier shape of
this suite's cousin), so the blocked case here first proves the card and the element
are there.

tier_3, because the point is the **positive** fetch→paint path and that needs bytes
a real server actually serves. The remote is a plain-``http`` loopback server the
test owns (``helpers.remote_media_server.RemoteMediaServer(tls=False)``, a thread,
no extra process) serving the fixture PNG: a genuinely third-party host, whose
request log witnesses that nothing is fetched before the reveal. It is deliberately
NOT the session's own nest: in docker nest mode the nest serves only its
self-signed floor cert, which the bare remote-image client
(``fauna_client::remote_image``) rightly refuses, so a nest blob url painted in
standalone mode (plain http) and stayed a placeholder under docker.

tui is the app that can assert the paint headlessly: it renders the picture as
half-block ``▀`` characters, so the picture IS the element's text. The GUI apps paint
into a native image view (or, on web, an ``<img src>`` the DOM can't be probed for
pixel content headlessly here) with no observable paint, so
``FeedActions.painted_doc_remote_image_count`` returns ``None`` there and the paint
assertion is skipped — the reveal-gate assertions still run on every app.

web/android adopt the shared ``RenderDocument::remote_images()`` UniFFI/wasm face
(render-model.md § Implementation status today) — web's ``documentToHtml`` walker
paints ``doc-remote-image`` inline (no separate extraction needed, its HTML-string
walk was already unified), android's ``documentRemoteImages`` now delegates to
``com.fauna.ffi.renderDocumentRemoteImages`` instead of its former hand-rolled
recursive twin.

apple (macos+ios, 2026-08-02) had no local walker to replace: its shared FaunaKit
``DocumentBodyView`` already pattern-matches the FFI-provided ``RenderBlock``
tree directly (no re-parse, no re-derivation), so the ``.remoteImage`` case
already came straight off the shared document — only the ``doc-remote-image``
id registration was missing, added once in ``remoteImageView`` and shared by
the feed post-card, post detail, and the conversations bubble (one shared
recursive walker, not three per-surface ports).

windows (2026-08-04) DID have a local walker to replace: ``RemoteImage`` used
to paint inline as a placeholder run inside the body ``TextBlock``
(``DocumentRenderer.Flatten``), with no id of its own. The walker arm is now
inert — the same "own element ID ⇒ painted by the page" rule ``post-image`` /
``quoted-post`` / ``link-preview-card`` already follow — and
``DocumentRenderer.RemoteImages`` (the ``render_document_remote_images`` UniFFI
face) feeds one shared ``DocumentPainter.ApplyRemoteImages`` panel builder the
feed post-card, the post_detail dialog, and the conversations DM bubble all
call, painting one ``doc-remote-image`` Border per entry just under the body.
"""

import uuid
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier_3]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.tui  # lead app (2026-08-01)
@pytest.mark.web  # 2026-08-01
@pytest.mark.android  # 2026-08-01; the other four follow in the batched trickle-down
@pytest.mark.macos  # 2026-08-02
@pytest.mark.ios  # 2026-08-02
@pytest.mark.windows  # 2026-08-04
@pytest.mark.linux  # was already fully built (document.rs's load-remote-content-button,
# wired to FeedManager::reveal_remote_images) but never got its marker in the
# 2026-08-01/04 trickle-down
@pytest.mark.feature("feed-images-and-video")
def test_body_remote_image_is_blocked_until_revealed_then_paints(logged_in_app):
    """Blocked → tap ``load-remote-content-button`` → the image loads and paints."""
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")
    app = logged_in_app

    from helpers.remote_media_server import RemoteMediaServer

    # A real, servable third-party image url: a plain-http loopback server this
    # test owns. Plain http, never the nest: in docker mode the nest serves only
    # its self-signed floor cert, which the bare remote-image client rightly
    # refuses — a nest blob url painted in standalone (plain http) and stayed a
    # placeholder under docker. The server's request log is also the witness
    # that nothing is fetched before the reveal.
    remote = RemoteMediaServer(
        "remote-image.test",
        None,
        {"/a-cat.png": (TEST_IMAGE.read_bytes(), "image/png")},
        tls=False,
    )
    try:
        _run_journey(app, remote)
    finally:
        remote.close()


def _run_journey(app, remote) -> None:
    url = remote.url("/a-cat.png")

    text = _unique("remote-image")
    # The card renders the image as its alt text, so wait for the plain token.
    app.feed.create_post(text=f"{text} ![a cat]({url})", expect_top=text)
    i = app.feed.post_index_by_text(text)
    assert i >= 0, (
        f"the composed post {text!r} must render a card; "
        f"post_count={app.feed.post_count()} error={app.error_text()!r}"
    )

    # Presence first, so the blocked assertion below cannot pass vacuously: the
    # element exists in the blocked state too, and the reveal affordance is up.
    assert app.driver.count("doc-remote-image", scope=f"post-card[{i}]") == 1, (
        f"a body remote image must register its element even while blocked; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count("load-remote-content-button", scope=f"post-card[{i}]") == 1, (
        "a blocked remote image must surface the post's reveal button"
    )

    # Blocked: nothing has been fetched, so nothing is painted.
    blocked = app.feed.painted_doc_remote_image_count(i)
    if blocked is not None:
        assert blocked == 0, (
            "a blocked remote image must not paint — its url has not been requested; "
            f"doc-remote-image text="
            f"{app.driver.get_text('doc-remote-image', scope=f'post-card[{i}]')!r}"
        )
    assert remote.requests == [], (
        f"a blocked remote image must not be requested; the remote saw {remote.requests!r}"
    )

    # Reveal → the manager flips the block, the app fetches the url, the picture
    # lands. The reveal button clears because nothing is blocked any more.
    app.feed.reveal_remote_content(i)
    assert app.driver.count("load-remote-content-button", scope=f"post-card[{i}]") == 0, (
        "the reveal button must clear once the post has no blocked remote content"
    )
    assert app.driver.count("doc-remote-image", scope=f"post-card[{i}]") == 1, (
        "revealing must not drop the element"
    )

    painted = app.feed.doc_remote_image_painted(i)
    if painted is not None:
        assert painted, (
            "a revealed remote image must PAINT its fetched bytes, not stay a "
            f"placeholder; doc-remote-image text="
            f"{app.driver.get_text('doc-remote-image', scope=f'post-card[{i}]')!r} "
            f"error={app.error_text()!r} remote requests={remote.requests!r}"
        )
