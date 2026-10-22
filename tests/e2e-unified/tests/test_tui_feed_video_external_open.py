"""tier_3 e2e: tui's feed-video leg of inline playback — the OS external handoff
on ``video-thumbnail``, end to end against a real nest and the real ``fauna-tui``
binary.

Inline AV playback is tui's declared platform absence, so its leg of
``render-model.md`` § D6c → *Inline playback* is the external handoff
``architecture/apps/tui.md`` § External media handoff → *Feed video* owns: the
Media page's mode gate and hardened temp file, fed from the feed. What this
proves:

- the thumbnail is ``state=idle`` until activated — never autoplay;
- under ``ask`` (the default) activation arms the reused
  ``media-external-open-*`` confirm on the feed; confirm fetches the public
  post's clip off the blob route, materializes it per the ratified shape (0600,
  the real extension sniffed from the bytes, under the per-user runtime dir),
  hands that FILE to the OS handler, and the element reads ``state=playing``;
- cancel is a pure no-op, and under ``never`` the thumbnail is an inert label:
  still painted (it is the block's only element), refused as a click, nothing
  launched.

The sealed-``Video`` and ``ProxiedVideo`` byte paths are pinned by the feed
module's unit tests (``apps/fauna-tui/src/feed/mod.rs``) over the same
``fetch_video_bytes``; this journey drives the public path through the whole
stack.

The OS handler is the ``FAUNA_TUI_MEDIA_OPENER`` seam, as in
``test_tui_media_external_open.py``: a recorder script appending its argv (the
handed-off path) to a file this test polls. The bytes are compared with what
the nest SERVES for the blob (convention 5: the upload pipeline may neutralise
container metadata in place, so the served blob — not the fixture on disk — is
the clip a player would get).

tui-only by structure (the confirm IDs are ``platform_elements: [tui]``), via
the same parametrized-fixture shape as the Media-page pin, so ``--app``
filtering deselects instead of failing. A dedicated launch: the opener seam and
an isolated runtime base must be in the binary's environment at spawn.
"""

import os
import re
import stat
import time
import uuid
import urllib.request
from pathlib import Path

import pytest

from conftest import _make_user, _seeded_environment, get_available_apps
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

THUMB = "video-thumbnail"
MODAL = "media-external-open-confirm-modal"
CONFIRM = "media-external-open-confirm-button"
CANCEL = "media-external-open-cancel-button"

FIXTURE = Path(__file__).parent.parent.parent / "fixtures" / "tiny-video.mp4"

# The same device id the shared login patch uses (conftest _E2E_LOGIN_DEVICE_ID).
_DEVICE_ID = "0123456789abcdef" * 4


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, in the ``--app``-filterable fixture shape."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@pytest.fixture()
def opened_app(launch_client, tui_app_path, nest_instance, tmp_path, request):
    """A logged-in tui launch on the feed with the opener seam + an isolated
    handoff dir. Yields ``(driver, record_file, runtime_dir)`` — the
    ``test_tui_media_external_open.py`` shape, for the same reasons (a dedicated
    actor; ``runtime_dir`` pinned as both ``XDG_RUNTIME_DIR`` and ``TMPDIR`` so
    the handoff location is assertable on either OS)."""
    user = _make_user(nest_instance)
    record_file = tmp_path / "opener-record.txt"
    runtime_dir = tmp_path / "runtime"
    runtime_dir.mkdir(mode=0o700)
    opener = tmp_path / "fake-opener.sh"
    opener.write_text(f'#!/bin/sh\nprintf \'%s\\n\' "$1" >> "{record_file}"\n')
    opener.chmod(0o755)

    driver = create_driver("tui")
    driver.launch(
        {
            "app_path": tui_app_path,
            "url": nest_instance["url"],
            "environment": {
                **_seeded_environment(request, nest_instance),
                "FAUNA_TUI_MEDIA_OPENER": str(opener),
                "XDG_RUNTIME_DIR": str(runtime_dir),
                "TMPDIR": str(runtime_dir),
            },
        }
    )
    try:
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
                "nav": {"stack": [{"view": "feed"}]},
            }
        )
        driver.wait_for("feed-view", timeout=15)
        yield driver, record_file, runtime_dir
    finally:
        try:
            driver.teardown()
        except Exception:
            pass


def _recorded_paths(record_file) -> list[str]:
    if not record_file.exists():
        return []
    return [line for line in record_file.read_text().splitlines() if line]


def _wait_for_recorded(record_file, count: int, timeout: float = 20.0) -> list[str]:
    deadline = time.monotonic() + timeout
    paths = _recorded_paths(record_file)
    while len(paths) < count and time.monotonic() < deadline:
        time.sleep(0.2)
        paths = _recorded_paths(record_file)
    return paths


def _wait_state(driver, want: str, timeout: float = 20.0) -> str | None:
    deadline = time.monotonic() + timeout
    state = driver.get_attr(THUMB, "state")
    while state != want and time.monotonic() < deadline:
        time.sleep(0.2)
        state = driver.get_attr(THUMB, "state")
    return state


def _post_a_video(driver) -> str:
    """Post ``tiny-video.mp4`` through the composer (the real upload path) and
    return the blob hash its ``video-thumbnail`` paints."""
    from actions.feed import FeedActions

    assert FIXTURE.exists(), f"{FIXTURE} missing"
    FeedActions(driver).create_post_with_video(
        text=f"video-handoff-{uuid.uuid4().hex[:8]}", video_path=str(FIXTURE)
    )
    driver.wait_for(THUMB, timeout=20)
    match = re.search(r"[0-9a-f]{64}", driver.get_text(THUMB))
    assert match, f"the thumbnail paints its blob hash: {driver.diagnose(THUMB)}"
    return match.group(0)


def _served_blob(nest_instance, blob_hash: str) -> bytes:
    """What the nest serves for the blob — the public, unauthenticated route."""
    with urllib.request.urlopen(f"{nest_instance['url']}/api/v1/blob/{blob_hash}") as reply:
        return reply.read()


def _error_text(driver) -> str:
    return driver.get_text("error-message") if driver.is_visible("error-message") else ""


def test_ask_confirm_hands_the_feed_clip_to_the_os_player(opened_app, nest_instance):
    """The full journey under the default ``ask``: idle → activate → the
    reused confirm → confirm → the OS handler receives a 0600 ``.mp4`` under the
    runtime handoff dir whose bytes are the clip the nest serves, and the
    thumbnail reads ``state=playing``."""
    driver, record_file, runtime_dir = opened_app
    blob_hash = _post_a_video(driver)

    assert driver.get_attr(THUMB, "state") == "idle", (
        "a video must never autoplay: before activation the thumbnail is idle"
    )
    driver.click(THUMB)
    driver.wait_for(MODAL, timeout=10)
    assert _recorded_paths(record_file) == [], "ask launches nothing before the confirm"

    driver.click(CONFIRM)
    paths = _wait_for_recorded(record_file, 1)
    assert len(paths) == 1, (
        "the OS opener should have been spawned exactly once; the fetch or the "
        f"materialize failed — error element: {_error_text(driver) or '(none)'}; "
        f"{driver.diagnose(THUMB)}"
    )
    state = _wait_state(driver, "playing")
    assert state == "playing", (
        f"the spawn should read state=playing, got {state!r}: {driver.diagnose(THUMB)}"
    )
    assert driver.is_absent(MODAL), "the confirm closes on launch"

    handed = paths[0]
    expected_dir = str(runtime_dir / "fauna-tui" / "media")
    assert os.path.dirname(handed) == expected_dir, (
        f"the clip must rest under the runtime handoff dir: {handed}"
    )
    assert handed.endswith(".mp4"), (
        f"the extension sniffed from the clip is what the OS resolves the player by: {handed}"
    )
    mode = stat.S_IMODE(os.stat(handed).st_mode)
    assert mode == 0o600, f"owner-only file, got {oct(mode)}"
    with open(handed, "rb") as f:
        handed_bytes = f.read()
    assert handed_bytes == _served_blob(nest_instance, blob_hash), (
        "the handed-off bytes are the clip the nest serves for the post's video"
    )
    assert not _error_text(driver), f"no error after a handoff: {_error_text(driver)!r}"


def test_cancel_runs_nothing_and_never_leaves_the_thumbnail_inert(opened_app):
    """Cancel is a pure no-op; then, with the mode flipped to ``never`` through
    the settings UI, the thumbnail still paints but is inert — the click is
    refused, nothing arms, nothing launches."""
    driver, record_file, _runtime_dir = opened_app
    _post_a_video(driver)

    driver.click(THUMB)
    driver.wait_for(MODAL, timeout=10)
    driver.click(CANCEL)
    assert driver.is_absent(MODAL), "cancel disarms the confirm"
    assert driver.get_attr(THUMB, "state") == "idle", "nothing was fetched"

    # Flip to `never` through the UI (the only config surface).
    driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "tui-settings"}]}}
    )
    driver.wait_for("tui-settings-external-media", timeout=10)
    driver.select("tui-settings-external-media", "never")
    driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    driver.wait_for(THUMB, timeout=20)

    assert driver.is_visible(THUMB), "under never the thumbnail still paints"
    with pytest.raises(Exception):
        # The agent answers `not actuable` for a label — the inert observable.
        driver.click(THUMB)
    driver.barrier()
    assert driver.is_absent(MODAL), "under never nothing arms"
    assert driver.get_attr(THUMB, "state") == "idle"
    assert _recorded_paths(record_file) == [], (
        "cancel and never are both no-ops: the opener must never have been spawned"
    )
