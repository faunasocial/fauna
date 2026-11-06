"""tier_3 e2e: the tui inline-image arms (M6 slice 3a) — auto-detection and the
post-paint emit, end to end against a real nest and the real ``fauna-tui``
binary.

What this proves, per ``apps/tui.md`` § Rendering — *"inline via terminal
graphics protocols (kitty, iTerm2, sixel), with a half-block cell fallback on
terminals without one. Protocol choice is auto-detected, never configured"*:

- against a terminal that **reports sixel**, a real thumbnail is emitted as a
  real sixel sequence, positioned at the cell rect the art occupies and sized
  from the terminal's own reported cell metrics;
- against a terminal that **does not**, nothing is emitted and the half-block
  art is the picture — the fallback, not a hole;
- **either way the art paints and stays readable**, which is what keeps
  ``painted_thumbnail_count`` honest (see below).

**Nothing here forces a protocol on the client.** The driver owns the pty
master, so it *is* the terminal: it answers the client's ``ESC [ c`` probe as a
terminal with or without sixel (``drivers/tui.py``), and the client's own probe
→ parse → decide → emit runs for real. An app-side "use sixel" switch would be
the configuration knob the goal doc forbids, and it would also test nothing:
the branch most likely to be wrong *is* the detection.

**The no-sixel case is a control, not a courtesy.** Both cases upload the same
image and reach the same painted state; the only difference is what the terminal
said. Without it, `emits_a_sixel...` passing would not distinguish "the client
detected sixel and emitted" from "something in this stack always emits sixel".

The pair also pins the trap the design is shaped around (``graphics`` module
docs): `painted_thumbnail_count` counts ``▀`` in ``media-thumbnail``'s registry
text, which comes from the *element*, not the frame. An arm that replaced the
art with pixels instead of overlaying it would empty that text, and every media
suite would keep passing while asserting nothing. So the sixel case asserts the
art is *still there*, underneath.

tui-only by structure: no other app has a terminal to emit into (inline
images are the tui paint layer's own concern — ``tui.md`` § Cross-platform).
Uses the ``--client``-filterable fixture shape, so filtering deselects rather
than fails.

A dedicated launch (not the cached ``app`` fixture): the terminal's DA1 answer
is fixed at pty setup, before the client's probe, which happens once at startup.
"""

import time
from pathlib import Path

import pytest

from conftest import _make_user, _seed_cross_set_media, _seeded_environment, get_available_apps
from drivers import create_driver
from drivers.tui import _PTY_CELL_H, _PTY_CELL_W
from helpers.e2e_session import e2e_login_device_id

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

# The same device id the shared login patch gives tui.
_DEVICE_ID = e2e_login_device_id("tui")

# A sixel sequence's Device Control String introducer, as
# ``apps/fauna-tui/src/graphics/sixel.rs`` emits it.
SIXEL_INTRODUCER = b"\x1bP0;1;0q"

# `apps/fauna-tui/src/thumbnail.rs::THUMBNAIL_COLS` — the art's width in cells,
# and so the emitted image's width in cells.
THUMBNAIL_COLS = 16

# `apps/fauna-tui/src/graphics/mod.rs::FALLBACK_CELL.width` — the cell width the
# client assumes when the terminal will not report its pixel size, which is every
# ConPTY launch (`drivers/pty_backend.py`). Deliberately at the low end of real
# cell sizes: guessing large spills the picture past the cells the layout
# reserved for it, guessing small only shrinks it.
FALLBACK_CELL_W = 6

# What each launch's dedicated actor owns before the upload: one folder holding
# one text file, which the producer makes no thumbnail for.
_SEED_PATHS = ("seed/readme.txt",)


def _launch(client, app_path, nest_instance, request, *, da1_sixel: bool):
    """A logged-in tui launch on the Media page, against a terminal that reports
    sixel support (or doesn't).

    A DEDICATED seeded actor (the ``seeded_media_app`` rationale): the page
    upload needs a readable owned folder to target, and a dedicated actor
    cannot pollute the shared ``test_user``'s empty-state assertions.
    """
    user = _make_user(nest_instance)
    _seed_cross_set_media(nest_instance, user, {"media-e2e-gfx": list(_SEED_PATHS)})

    driver = create_driver(client)
    driver.launch(
        {
            "app_path": app_path,
            "url": nest_instance["url"],
            # Read by the DRIVER (the terminal), never by the client.
            "tui_da1_sixel": da1_sixel,
            "environment": _seeded_environment(request, nest_instance),
        }
    )
    driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": user["signing_key"].encode().hex(),
                "handle": "e2e-user",
                "actor_id": user["actor_id_hex"],
                "device_id": _DEVICE_ID,
            },
            "nav": {"stack": [{"view": "media"}]},
        }
    )
    driver.wait_for("media-view-toggle", timeout=15)
    return driver


def _upload_image_and_wait_for_paint(driver):
    """Upload the image fixture and wait for its thumbnail to paint.

    The producer makes a thumbnail for an image upload; the client fetches,
    decrypts and rasterizes it off the machine's observer tick, so the paint is
    polled for rather than awaited (``tui.md`` § Rendering).
    """
    from actions.media import MediaActions

    media = MediaActions(driver)
    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    # The seeded folder must be LISTED before the upload, or the page has no
    # folder to upload into. Right after sign-in the folder's rows cannot be
    # verified until the account runtime has assembled and its custody reads,
    # so the page's first listing can come back without the folder, and an
    # upload pressed then is refused with `media.error_no_set` ("You don't have
    # a folder to upload into yet"). That refusal was this file's red on Windows: the
    # upload lost the race by 55 ms on one launch and won it on the next,
    # whichever terminal the driver answered as. A `before` read at that moment
    # also counted 0, so the seeded item turning up a moment later satisfied
    # `before + 1` and hid the refused upload. So wait for the seed itself,
    # the same state `test_media.py` waits for before uploading. `ensure_loaded`
    # alone is not enough: an empty first listing reads as loaded.
    media.ensure_loaded()
    seeded = media.wait_for_item_count(len(_SEED_PATHS))
    assert seeded == len(_SEED_PATHS), (
        f"the seeded folder should list its {len(_SEED_PATHS)} item before the upload, "
        f"got {seeded}; error={_error_text(driver)!r}; media log: {_media_log(driver)}"
    )
    media.upload_file(str(FIXTURE_IMAGE))
    uploaded = media.wait_for_item_count(seeded + 1)
    assert uploaded == seeded + 1, (
        f"the upload should add one item ({seeded} -> {seeded + 1}), got {uploaded}; "
        f"error={_error_text(driver)!r}; media log: {_media_log(driver)}"
    )
    # Exactly one painted item: the image just uploaded. The set's other seeded
    # member is a text file, so the producer makes it no thumbnail and its row
    # paints the placeholder — which is why this counts rather than reading a
    # fixed row.
    painted = media.wait_for_painted_thumbnails(1)
    assert painted == 1, (
        f"the uploaded image's thumbnail should paint as half-block art whatever "
        f"the terminal supports, got {painted} of {media.item_count()} item(s); "
        f"error={_error_text(driver)!r}; terminal stream "
        f"{Path(driver.frame_stream_path()).stat().st_size} bytes, sixel introducer "
        f"{'present' if SIXEL_INTRODUCER in Path(driver.frame_stream_path()).read_bytes() else 'absent'}; "
        f"media log: {_media_log(driver)}"
    )
    return media


def _error_text(driver) -> str | None:
    """The page's ``error-message`` text, or None when the page registers none
    yet — a diagnostic must report, never raise over the failure it explains
    (convention 6)."""
    try:
        return driver.get_text("error-message")
    except LookupError:
        return None


def _media_log(driver) -> list[str]:
    """The client's own log lines about the media page and its notices, last
    few first-to-last — a diagnostic, never raising (convention 6).

    The page error alone is not enough to explain a failed upload: a refused
    upload sets it, and the next successful listing clears it again, so by the
    time an assertion reads `error-message` it can be empty. The log keeps
    the refusal (`fauna_media` warns its key, `fauna_tui::notice` logs the
    banner)."""
    try:
        text = driver.app_stderr_text() or ""
    except Exception as exc:  # noqa: BLE001 — a diagnostic reports, never raises
        return [f"<log unreadable: {exc}>"]
    lines = [
        line.strip()
        for line in text.splitlines()
        if "fauna_media" in line or "fauna_tui::notice" in line or "fauna_tui::media" in line
    ]
    return lines[-6:]


def _wait_for_bytes(path, needle: bytes, timeout: float = 10.0) -> bytes:
    """Poll the client's terminal stream until ``needle`` appears; return it all."""
    deadline = time.monotonic() + timeout
    stream = b""
    while time.monotonic() < deadline:
        stream = Path(path).read_bytes()
        if needle in stream:
            return stream
        time.sleep(0.2)
    return stream


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, in the ``--client``-filterable fixture shape."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


def test_sixel_terminal_gets_a_real_sixel_at_the_arts_cell_rect(
    launch_client, tui_app_path, nest_instance, request
):
    """A terminal reporting sixel gets the thumbnail as a sixel.

    The width assertion is the interesting one. ``"1;1;<w>;<h>`` are the sixel's
    raster attributes, and a correct ``w`` means the whole chain agreed: the art
    is ``THUMBNAIL_COLS`` cells wide, the client learned this terminal's cell
    width, and it multiplied the two to size the picture into exactly the box the
    art occupies. A wrong ``w`` is a picture that spills over its neighbours or
    underfills its own cells — the failure a "does it paint at all" assertion
    sails straight past.

    Which cell width that is comes from the *terminal*: a POSIX pty reports its
    pixel size on the winsize ioctl, ConPTY has no such concept and the client
    falls back to its own assumed ``FALLBACK_CELL_W``. So the expectation asks
    the driver which kind of terminal this is rather than which OS it is on — the
    client's arithmetic is the thing under test and it is identical either way.
    """
    driver = _launch(launch_client, tui_app_path, nest_instance, request, da1_sixel=True)
    try:
        media = _upload_image_and_wait_for_paint(driver)

        stream = _wait_for_bytes(driver.frame_stream_path(), SIXEL_INTRODUCER)
        assert SIXEL_INTRODUCER in stream, (
            "a terminal that answered the DA1 probe with sixel support (attribute 4) "
            "should have been sent a sixel sequence; the client emitted none, so "
            "detection or the post-paint emit is broken. "
            f"stream tail: {stream[-400:]!r}"
        )

        reports_px = driver.reports_pixel_size()
        cell_w = _PTY_CELL_W if reports_px else FALLBACK_CELL_W
        source = "reports" if reports_px else "does not report"
        expected_width = THUMBNAIL_COLS * cell_w
        raster = b'"1;1;%d;' % expected_width
        assert raster in stream, (
            f"the sixel should be {expected_width}px wide — {THUMBNAIL_COLS} cells of art "
            f"at {cell_w}px cells (this pty {source} its pixel size) — so its raster "
            f"attributes should read {raster!r}. Absent means the cell metrics or the "
            f"cell box did not survive the trip. stream tail: {stream[-400:]!r}"
        )

        # Positioned before it is painted: a sixel lands at the cursor.
        assert b"\x1b[" in stream and b"H" + SIXEL_INTRODUCER in stream, (
            "the sixel should be preceded by a cursor-position escape ending in 'H', "
            f"or it paints wherever the cursor happened to be. stream tail: {stream[-400:]!r}"
        )

        # The trap (`graphics` module docs): the art must still be there, with a
        # live protocol arm emitting. If an arm ever replaces the art instead of
        # overlaying it, this count drops to zero — and `test_media.py`'s paint
        # assertion, which sits behind an `if painted is not None` guard, goes
        # vacuous and stays green.
        assert media.painted_thumbnail_count() == 1, (
            "the half-block art must stay painted underneath a live protocol arm — "
            "it is the fallback, the eraser for the compositing protocols, AND the "
            "only thing `painted_thumbnail_count` can read"
        )
    finally:
        try:
            driver.teardown()
        except Exception:
            pass


def test_terminal_without_sixel_gets_no_escapes_and_paints_half_blocks(
    launch_client, tui_app_path, nest_instance, request
):
    """The fallback arm, and the control for the test above.

    Same upload, same painted state — the only difference is what the terminal
    said about itself. A client that emitted a sixel here would be ignoring the
    probe it just made, and the picture would be garbage on screen: a terminal
    without sixel prints the escape's payload as text.
    """
    driver = _launch(launch_client, tui_app_path, nest_instance, request, da1_sixel=False)
    try:
        media = _upload_image_and_wait_for_paint(driver)

        # The art is on screen (asserted above), so anything the protocol arms
        # were going to emit has been emitted by now.
        stream = Path(driver.frame_stream_path()).read_bytes()
        assert SIXEL_INTRODUCER not in stream, (
            "a terminal that reported no sixel support must not be sent a sixel — "
            "it would print the payload as thousands of characters of garbage"
        )
        assert b"\x1b_G" not in stream, "and no kitty graphics escape"
        assert b"\x1b]1337;" not in stream, "and no iTerm2 inline-image escape"

        assert media.painted_thumbnail_count() == 1, (
            "the half-block arm is the picture on a terminal without a protocol"
        )
    finally:
        try:
            driver.teardown()
        except Exception:
            pass
