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

The handoff is tui's door to the playback outcomes the feature catalog grades
on every column (``docs/features/feed-images-and-video.md`` outcomes 12, 13, 14,
15 and 19), so each journey here carries the feature marker and the contract
names it beside the siblings' in-place witnesses in ``test_feed_video_playback.py``:
the public path (12), a bridged post's ``ProxiedVideo`` through the nest's media
proxy (13), a feed refresh while a handoff runs (14), leaving the feed and coming
back (15), and a gated post's sealed ``Video`` once it unlocks (19). The feed
module's unit tests (``apps/fauna-tui/src/feed/mod.rs``) pin the same byte paths
over ``fetch_video_bytes`` underneath.

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
from contextlib import contextmanager
from pathlib import Path

import pytest

from conftest import _make_user, _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.e2e_session import e2e_login_device_id
# The bridged and sealed seeds are the in-place witnesses' own fixtures: one
# AP nest + remote media server, one dedicated gated nest (priority #2).
from tests.test_feed_video_playback import (  # noqa: F401 — pytest fixtures
    bridged_video_nest,
    remote_media,
    sealed_video_nest,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

THUMB = "video-thumbnail"
MODAL = "media-external-open-confirm-modal"
CONFIRM = "media-external-open-confirm-button"
CANCEL = "media-external-open-cancel-button"

FIXTURE = Path(__file__).parent.parent.parent / "fixtures" / "tiny-video.mp4"

# The same device id the shared login patch gives tui.
_DEVICE_ID = e2e_login_device_id("tui")


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, in the ``--app``-filterable fixture shape."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@contextmanager
def _tui_on_feed(request, tui_app_path, nest, user, tmp_path, *, handle="e2e-user",
                 device_id=_DEVICE_ID):
    """A logged-in tui launch against ``nest`` as ``user``, on the feed, with the
    opener seam + an isolated handoff dir. Yields ``(driver, record_file,
    runtime_dir)`` — the ``test_tui_media_external_open.py`` shape, for the same
    reasons (a dedicated actor; ``runtime_dir`` pinned as both
    ``XDG_RUNTIME_DIR`` and ``TMPDIR`` so the handoff location is assertable on
    either OS). The launch is seeded for ``nest`` itself: an environment fixed at
    launch names the nest the app will use (``_seeded_environment``)."""
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
            "url": nest["url"],
            "environment": {
                **_seeded_environment(request, nest),
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
                    "node_url": nest["url"],
                    "secret_hex": bytes(user["signing_key"]).hex(),
                    "handle": handle,
                    "actor_id": user["actor_id_hex"],
                    "device_id": device_id,
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


@pytest.fixture()
def opened_app(launch_client, tui_app_path, nest_instance, tmp_path, request):
    """The shared session nest, a fresh actor: ``_tui_on_feed``'s triple."""
    user = _make_user(nest_instance)
    with _tui_on_feed(request, tui_app_path, nest_instance, user, tmp_path) as opened:
        yield opened


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


@pytest.mark.feature("feed-images-and-video")
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


def _hand_off(driver, record_file, count: int, *, scope: str | None = None) -> list[str]:
    """Tap the thumbnail under ``ask``, confirm, and wait for the opener's
    ``count``-th spawn and ``state=playing``. Returns every recorded path."""
    driver.click(THUMB, scope=scope)
    driver.wait_for(MODAL, timeout=10)
    assert len(_recorded_paths(record_file)) == count - 1, (
        "ask launches nothing before the confirm"
    )
    driver.click(CONFIRM)
    paths = _wait_for_recorded(record_file, count)
    assert len(paths) == count, (
        f"the OS opener should have been spawned {count} time(s), got {paths!r} — "
        f"error element: {_error_text(driver) or '(none)'}; {driver.diagnose(THUMB)}"
    )
    deadline = time.monotonic() + 20
    state = driver.get_attr(THUMB, "state", scope=scope)
    while state != "playing" and time.monotonic() < deadline:
        time.sleep(0.2)
        state = driver.get_attr(THUMB, "state", scope=scope)
    assert state == "playing", (
        f"the spawn should read state=playing, got {state!r}: {driver.diagnose(THUMB)}"
    )
    return paths


@pytest.mark.feature("feed-images-and-video")
def test_a_feed_refresh_leaves_a_running_handoff_alone(opened_app):
    """A new post landing above a video whose clip is with the OS player leaves
    that handoff alone (outcome 14): the re-queried feed keeps the thumbnail at
    ``state=playing``, spawns nothing again, and the handed-off file the player
    holds is still there — the refresh deletes no clip (only a new handoff
    does, tui.md § External media handoff → *Lifetime*)."""
    from actions.feed import FeedActions

    driver, record_file, _runtime_dir = opened_app
    _post_a_video(driver)
    paths = _hand_off(driver, record_file, 1)

    # `create_post` returns once the new note renders, i.e. once the re-queried
    # page has landed — the causal anchor for the reads below (convention 14).
    FeedActions(driver).create_post(f"after-the-video-{uuid.uuid4().hex[:8]}")
    driver.barrier()
    state = driver.get_attr(THUMB, "state")
    assert state == "playing", (
        f"the refresh reset the running handoff's thumbnail: state={state!r}: "
        f"{driver.diagnose(THUMB)}"
    )
    assert _recorded_paths(record_file) == paths, "a refresh spawns no player"
    assert os.path.exists(paths[0]), (
        f"the refresh deleted the clip the player was handed: {paths[0]}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_leaving_the_feed_mid_handoff_and_coming_back_opens_the_video_again(opened_app):
    """Leave the feed while the clip is with the OS player, come back: the app
    answers through the page change, the thumbnail is back on the card, and a
    second tap hands the clip off again (outcome 15)."""
    from actions import ActionLayer

    driver, record_file, _runtime_dir = opened_app
    app = ActionLayer(driver)
    _post_a_video(driver)
    _hand_off(driver, record_file, 1)

    app.contacts.navigate()
    # An app that died leaving the page never acknowledges the barrier.
    driver.barrier()
    app.feed.navigate()
    driver.wait_for(THUMB, timeout=20)
    state = driver.get_attr(THUMB, "state")
    assert state in ("idle", "playing"), (
        f"back on the feed the video should be there to open: state={state!r}: "
        f"{driver.diagnose(THUMB)}"
    )
    paths = _hand_off(driver, record_file, 2)
    assert os.path.exists(paths[1]) and paths[1].endswith(".mp4"), (
        f"the second handoff rests its clip like the first: {paths[1]}"
    )


@pytest.mark.standalone_only
@pytest.mark.feature("feed-images-and-video")
def test_a_bridged_video_hands_off_through_the_nest_proxy(
    launch_client, tui_app_path, bridged_video_nest, remote_media, tmp_path, request
):
    """A fediverse post's video attachment folds to a ``ProxiedVideo``: its
    thumbnail paints the proxied path and the remote server sees no request
    before the tap — nor before the confirm — and the confirmed handoff's bytes
    are the remote clip, fetched through this nest's media proxy (outcome 13;
    render-model.md § D6c → *Proxied video*; tui's leg is a bearer GET of the
    proxied path, tui.md § External media handoff → *Feed video*)."""
    from actions import ActionLayer
    from drivers.port_util import find_free_port
    from helpers.ap_nest import deliver_note, enable_ap
    from helpers.fake_follower import FakeFollower
    nest = bridged_video_nest
    # The remote outlives this test (module scope): judge only this test's asks.
    seen_before = len(remote_media.requests)

    def fetched() -> list[str]:
        return remote_media.requests[seen_before:]

    path, content_type = "/clip.mp4", "video/mp4"
    user = _make_user(nest)
    username, actor_url = enable_ap(nest, user)
    text = f"bridged-video-{uuid.uuid4().hex[:8]}"
    with FakeFollower(find_free_port()) as follower:
        deliver_note(
            nest, follower, username, actor_url, text,
            attachment=[{
                "type": "Document",
                "mediaType": content_type,
                "url": remote_media.url(path),
                "name": "a bridged clip",
            }],
        )

    with _tui_on_feed(request, tui_app_path, nest, user, tmp_path) as opened:
        driver, record_file, runtime_dir = opened
        assert ActionLayer(driver).feed.wait_for_post_text(text, timeout_s=30.0), (
            f"the bridged note {text!r} never rendered in the feed; "
            f"error={_error_text(driver)!r}"
        )
        driver.wait_for(THUMB, timeout=20)
        assert driver.get_attr(THUMB, "state") == "idle", (
            "a video must never autoplay: before the tap the thumbnail is idle"
        )
        label = driver.get_text(THUMB)
        assert "/api/v1/media/proxy?url=" in label, (
            f"a ProxiedVideo thumbnail paints its proxied path, got {label!r}"
        )
        assert path not in fetched(), (
            f"the thumbnail must load no remote byte before the tap; the remote saw {fetched()!r}"
        )

        driver.click(THUMB)
        driver.wait_for(MODAL, timeout=10)
        assert path not in fetched(), (
            f"ask fetches nothing before the confirm; the remote saw {fetched()!r}"
        )
        driver.click(CONFIRM)
        paths = _wait_for_recorded(record_file, 1)
        assert len(paths) == 1, (
            f"the OS opener should have been spawned once; the remote saw {fetched()!r}; "
            f"error element: {_error_text(driver) or '(none)'}; {driver.diagnose(THUMB)}"
        )
        state = _wait_state(driver, "playing")
        assert state == "playing", f"the spawn should read state=playing, got {state!r}"

        handed = paths[0]
        assert os.path.dirname(handed) == str(runtime_dir / "fauna-tui" / "media"), handed
        assert handed.endswith(".mp4"), f"the sniffed container names the extension: {handed}"
        assert stat.S_IMODE(os.stat(handed).st_mode) == 0o600
        with open(handed, "rb") as f:
            assert f.read() == FIXTURE.read_bytes(), (
                "the handed-off bytes are the remote clip, as the nest's proxy relayed it"
            )
        assert path in fetched(), (
            f"the remote file should have been fetched through the proxy; it saw {fetched()!r}"
        )


@pytest.mark.feature("feed-images-and-video")
def test_a_sealed_video_hands_off_once_its_post_unlocks(
    launch_client, tui_app_path, sealed_video_nest, tmp_path, request
):
    """A video on a tier-gated post opens for a reader entitled to it once the
    post unlocks (outcome 19) — here its author, who unlocks it on detail open,
    the in-place witness's journey: the detail's thumbnail is idle, the
    confirmed handoff spawns the opener with the OPENED item — a 0600 file whose
    container sniffs as MP4, which ciphertext never would."""
    from nacl.signing import SigningKey

    from actions import ActionLayer
    from common.auth import create_actor_and_register
    from tests.test_feed_video_playback import GATED_BODY, GATED_MARKER, GATED_PREVIEW

    nest = sealed_video_nest
    admin_sk = nest["admin"]["signing_key"]
    if not isinstance(admin_sk, SigningKey):
        admin_sk = SigningKey(bytes.fromhex(admin_sk))
    author = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=admin_sk,
    )

    with _tui_on_feed(
        request, tui_app_path, nest, author, tmp_path, handle="e2e-sealed-video"
    ) as opened:
        driver, record_file, runtime_dir = opened
        app = ActionLayer(driver)
        tier = f"gold-{uuid.uuid4().hex[:8]}"
        subs = app.subscriptions
        subs.navigate()
        subs.open_tiers_tab()
        subs.create_tier(tier, rank=1, price_hint="$5/mo")
        assert subs.wait_for_tier(tier), (
            f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
        )

        driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        driver.wait_for("feed-view")
        app.feed.create_gated_post(GATED_BODY, GATED_PREVIEW, tier, image_path=str(FIXTURE))

        app.feed.open_post_detail(0)
        driver.wait_for("feed-post-detail-body")
        deadline = time.monotonic() + 15
        body = ""
        while time.monotonic() < deadline:
            body = app.feed.post_detail_body()
            if GATED_MARKER in body:
                break
            time.sleep(0.5)  # sleep-ok: poll interval inside a deadline loop, not a settle wait
        assert GATED_MARKER in body, (
            f"the author's own gated post should unseal on detail open, got {body!r}; "
            f"error-message={_error_text(driver)!r}"
        )

        scope = "feed-post-detail-dialog"
        deadline = time.monotonic() + 20
        while driver.count(THUMB, scope=scope) < 1 and time.monotonic() < deadline:
            time.sleep(0.25)
        assert driver.count(THUMB, scope=scope) >= 1, (
            f"the unlocked post's video never painted in the detail: {driver.diagnose(THUMB)}"
        )
        assert driver.get_attr(THUMB, "state", scope=scope) == "idle", (
            "a video must never autoplay: before the tap the thumbnail is idle"
        )

        handed = _hand_off(driver, record_file, 1, scope=scope)[0]
        assert os.path.dirname(handed) == str(runtime_dir / "fauna-tui" / "media"), handed
        assert handed.endswith(".mp4"), (
            f"the extension is sniffed from the opened plaintext; sealed bytes sniff as "
            f"nothing: {handed}"
        )
        assert stat.S_IMODE(os.stat(handed).st_mode) == 0o600
        with open(handed, "rb") as f:
            head = f.read(8)
        assert head[4:8] == b"ftyp", (
            f"the player is handed the opened clip, never the sealed blob: head={head!r}"
        )
