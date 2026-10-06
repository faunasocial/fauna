"""tier_3 e2e: the tui external-media open (M6 slice 3b) — the four
``media-external-open-*`` IDs end to end against a real nest and the real
``fauna-tui`` binary.

What this proves, per ``apps/tui.md`` § External media handoff:

- the trigger paints only for an audio/video item under ask|always (under
  ``never`` it is absent entirely — metadata only, never painted-but-inert);
- ``ask`` (the default) arms the inline confirm; confirm **downloads +
  decrypts** the item's real bytes (the blob-store-primary arm of the shared
  ``MediaMachine::download_file`` query — this item was uploaded through the
  page, so its recorded hash names a sealed blob primary), materializes them
  per the ratified hardened-temp-file shape (0600, real extension, under the
  per-user runtime dir), and spawns the OS handler;
- cancel is a pure no-op (nothing downloads, nothing launches, the detail
  stays open).

The OS handler is the ``FAUNA_TUI_MEDIA_OPENER`` test seam (the same injection
point the spawn unit tests use): a recorder script that appends its argv —
the handed-off path — to a file this test polls. The crypto round trip is
asserted on the **bytes**: the file the player would open equals the plaintext
that was uploaded.

tui-only by structure (the IDs are ``platform_elements: [tui]`` — inline AV
playback is a declared tui platform absence, no other app has the trigger),
via the same parametrized-fixture shape as the headless-credential-store
module, so ``--client`` filtering deselects instead of failing.

A dedicated launch (not the cached ``app`` fixture): the opener seam and an
isolated runtime base (``XDG_RUNTIME_DIR`` on Linux / ``TMPDIR`` on macOS —
where the handoff dir lives) must be in the binary's environment at spawn.
"""

import os
import stat
import time

import pytest

import fauna_ffi

from conftest import _make_user, _seed_cross_set_media, _seeded_environment, get_available_apps
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

BUTTON = "media-external-open-button"
MODAL = "media-external-open-confirm-modal"
CONFIRM = "media-external-open-confirm-button"
CANCEL = "media-external-open-cancel-button"

# Distinctive plaintext for the round-trip assertion — the bytes the recorder's
# handed-off file must contain after seal → POST → fetch → decrypt.
CLIP_BYTES = b"RIFF-fake-video-payload " * 64

# The same device id the shared login patch uses (conftest _E2E_LOGIN_DEVICE_ID).
_DEVICE_ID = "0123456789abcdef" * 4


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, in the ``--client``-filterable fixture shape."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@pytest.fixture()
def opened_app(launch_client, tui_app_path, nest_instance, tmp_path, request):
    """A logged-in tui launch with the opener seam + an isolated handoff dir.

    Logged in as a DEDICATED seeded actor (the ``seeded_media_app`` rationale):
    the page upload needs a readable owned folder to target
    (``media.error_no_set`` otherwise), and a dedicated actor can't pollute the
    shared ``test_user``'s empty-state assertions. The recording device
    self-heals its write registration (``ui/media.md`` § Implementation status,
    self-healing registration), so no explicit device step is needed.

    Yields ``(driver, record_file, runtime_dir)``: ``record_file`` accumulates
    one line per opener spawn (the handed-off path); ``runtime_dir`` is the
    launch's private runtime base, pinned as BOTH ``XDG_RUNTIME_DIR`` (the
    Linux handoff base) and ``TMPDIR`` (the macOS one), so the ratified
    handoff location (``<base>/fauna-tui/media``) is assertable on either OS
    without touching the developer's real runtime/temp dir.
    """
    user = _make_user(nest_instance)
    _seed_cross_set_media(nest_instance, user, {"media-e2e-open": ["seed/readme.txt"]})
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
                # The ratified handoff base is per-OS (apps/tui.md § External
                # media handoff → Location): Linux resolves $XDG_RUNTIME_DIR,
                # macOS the per-user temp dir ($TMPDIR). Pin BOTH to the same
                # private dir so the handoff lands under `runtime_dir` on either
                # OS and the assertions below stay OS-branch-free.
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
                "nav": {"stack": [{"view": "media"}]},
            }
        )
        driver.wait_for("media-view-toggle", timeout=15)
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


def _wait_for_recorded(record_file, count: int, timeout: float = 15.0) -> list[str]:
    deadline = time.monotonic() + timeout
    paths = _recorded_paths(record_file)
    while len(paths) < count and time.monotonic() < deadline:
        time.sleep(0.2)
        paths = _recorded_paths(record_file)
    return paths


def _upload_and_open_detail(driver, tmp_path, name: str, data: bytes) -> None:
    """Upload ``data`` as ``name`` through the page (the real seal + POST +
    record path) and open its ``media-item-detail`` with version rows loaded."""
    from actions.media import MediaActions

    media = MediaActions(driver)
    src = tmp_path / name
    src.write_bytes(data)
    before = media.item_count()
    media.upload_file(str(src))
    media.wait_for_item_count(before + 1)
    index = media.item_names().index(name)
    media.open_item_detail(index)
    got = media.wait_for_version_count(1)
    assert got == 1, f"the uploaded item's version rows never loaded (got {got})"


def test_ask_confirm_downloads_decrypts_and_hands_off(opened_app, tmp_path):
    """The full journey under the default ``ask`` mode: trigger → confirm →
    the OS handler receives a 0600 temp file, with the real extension, under
    the ratified handoff dir, whose bytes are the uploaded plaintext."""
    driver, record_file, runtime_dir = opened_app
    _upload_and_open_detail(driver, tmp_path, "clip.mp4", CLIP_BYTES)

    assert driver.is_visible(BUTTON), (
        "an AV item's detail paints the external-open trigger under ask: "
        f"{driver.diagnose(BUTTON)}"
    )
    driver.click(BUTTON)
    driver.wait_for(MODAL, timeout=10)
    assert "clip.mp4" in driver.get_text(MODAL), (
        "the confirm names the item (ui.yaml contract): "
        f"got {driver.get_text(MODAL)!r}"
    )

    driver.click(CONFIRM)
    paths = _wait_for_recorded(record_file, 1)
    error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
    assert len(paths) == 1, (
        "the OS opener should have been spawned exactly once; the download or "
        f"materialize failed — error element: {error or '(none)'}"
    )
    handed = paths[0]

    # The ratified materialization shape (apps/tui.md § External media handoff).
    expected_dir = str(runtime_dir / "fauna-tui" / "media")
    assert os.path.dirname(handed) == expected_dir, (
        f"the clip must rest under the runtime handoff dir: {handed}"
    )
    assert handed.endswith(".mp4"), (
        f"the real extension is what the OS resolves the handler by: {handed}"
    )
    mode = stat.S_IMODE(os.stat(handed).st_mode)
    assert mode == 0o600, f"owner-only file, got {oct(mode)}"

    # The crypto round trip: seal → POST → fetch → verify → decrypt returned
    # exactly the plaintext the user uploaded.
    with open(handed, "rb") as f:
        assert f.read() == CLIP_BYTES, "the handed-off bytes are the uploaded plaintext"

    # No error surfaced: the handoff is a success, not a silently-failing spawn.
    assert not driver.is_visible("error-message") or not driver.get_text("error-message")


def test_cancel_is_a_pure_no_op_and_never_hides_the_trigger(opened_app, tmp_path):
    """Cancel: nothing downloads, nothing launches, the detail stays open.
    Then, flipping the setting to ``never`` through the settings UI removes the
    trigger entirely (absent, not inert) — and no stale confirm survives."""
    driver, record_file, _runtime_dir = opened_app
    _upload_and_open_detail(driver, tmp_path, "song.mp3", b"ID3 fake audio")

    driver.click(BUTTON)
    driver.wait_for(MODAL, timeout=10)
    driver.click(CANCEL)
    assert driver.is_absent(MODAL), "cancel disarms the confirm"
    assert driver.is_visible("media-item-detail"), "the detail surface stays open"
    time.sleep(1.0)
    assert _recorded_paths(record_file) == [], (
        "cancel is a pure no-op: the opener must never have been spawned"
    )

    # Close the detail first — it owns the pane while open, so the explorer
    # chrome the media-page wait below keys on would never paint behind it.
    driver.click("media-item-detail-close-button")

    # Flip to `never` through the UI (the only config surface), then re-open
    # the same item's detail: the trigger is ABSENT, not painted-but-inert.
    driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "tui-settings"}]}}
    )
    driver.wait_for("tui-settings-external-media", timeout=10)
    driver.select("tui-settings-external-media", "never")
    driver.set_state({"nav": {"stack": [{"view": "media"}]}})
    driver.wait_for("media-view-toggle", timeout=10)

    from actions.media import MediaActions

    media = MediaActions(driver)
    media.open_item_detail(media.item_names().index("song.mp3"))
    media.wait_for_version_count(1)
    assert driver.is_visible("media-item-detail-close-button"), "the detail is open"
    assert driver.is_absent(BUTTON), (
        "under `never` the trigger is absent entirely — metadata only "
        "(ui.yaml media-external-open-button contract)"
    )


def test_a_followed_folders_file_opens_from_the_browse_with_no_key(
    opened_app, nest_instance
):
    """A file in a folder the user FOLLOWS opens through the same trigger, over
    the keyless followed read (``download_followed``; ``ui/media.md`` § Followed
    public folders, *Downloads*) — the followed detail's one affordance, and the
    first test to drive that read through an app.

    Deliberately NOT tagged as the ``follow-a-public-folder`` outcome-10 witness:
    that sentence promises the download *with no account on the owner's nest*,
    and this is the same-nest follow (the only one any app can make today — the
    follow gesture never reaches another nest). The cross-nest row that builds
    that gesture extends this test into the witness.
    """
    import secrets

    from actions.media import MediaActions
    from common.auth import create_actor_and_register

    from tests.api.test_public_folder_fetch import DEVICE_ID, _record, _set_audience
    from tests.api.test_web_paywall_folder import _actor_client

    driver, record_file, _runtime_dir = opened_app
    url, port = nest_instance["url"], nest_instance["port"]

    # ── Fixture: someone else publishes a folder holding one clip. ──
    owner = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    folder = f"pub-{secrets.token_hex(3)}"
    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(url, bytes(owner["signing_key"]), {"name": folder})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    _set_audience(url, owner, folder, "public")
    _record(url, port, owner, folder, "clip.mp4", CLIP_BYTES)
    with _actor_client(url, owner) as ws:
        folder_id = ws.call(
            "fauna.folders.public.fetch",
            {"owner_actor_id": owner["actor_id_hex"], "folder_name": folder, "since": 0},
        )["folder_id"]

    # ── The follow, through the UI. ──
    driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}}
    )
    driver.click("folder-follow-button")
    driver.wait_for("folder-follow-name-input", timeout=10)
    driver.clear_and_type("recipient-picker-input", owner["actor_id_hex"])
    driver.clear_and_type("folder-follow-name-input", folder)
    driver.click("folder-follow-confirm")
    driver.wait_for("folder-followed-item", timeout=15)

    # ── The browse, then the open. ──
    driver.set_state({"nav": {"stack": [{"view": "media"}]}})
    driver.wait_for("media-view-toggle", timeout=15)
    media = MediaActions(driver)
    media.set_filter(f"followed:{folder_id}@")
    media.wait_for_item_count(1)
    media.open_item_detail(0)
    driver.wait_for(BUTTON, timeout=15)
    driver.click(BUTTON)
    driver.wait_for(MODAL, timeout=10)
    driver.click(CONFIRM)

    paths = _wait_for_recorded(record_file, 1)
    error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
    assert len(paths) == 1, (
        "the followed file never reached the opener — the keyless read or the "
        f"handoff failed; error element: {error or '(none)'}"
    )
    with open(paths[0], "rb") as f:
        assert f.read() == CLIP_BYTES, "the handed-off bytes are the published file"


@pytest.mark.feature("follow-a-public-folder")
def test_a_followed_folders_file_on_another_nest_opens_with_no_account_there(
    opened_app, nest_instance, cross_nest_foreign
):
    """The ``follow-a-public-folder`` outcome-10 witness: *a file in a folder
    you follow can be downloaded from the browse, with no account on the
    owner's nest* — the cross-nest twin of the test above, and the first
    follow any app makes of a folder homed on ANOTHER nest.

    Two real nests (convention 16: two seats, one machine, no operator):

    * **H** (``cross_nest_foreign``) — the owner's home. Its handle domain is
      its own loopback authority and it serves TLS on the self-signed floor,
      exactly the shape every LAN, loopback or domainless nest has.
    * **F** (``nest_instance``) — the follower's nest, the one the tui app is
      signed into. The follower has no account, roster row, grant or key on H.

    The production chain each step drives, end to end
    (``behavior/folders.md`` § Publicly-synced follow, the address bullet;
    ``ui/media.md`` § Followed public folders, *Downloads*;
    ``security.md`` § Transport trust, the federation-granted row):

      the follower types ``owner@<H's domain>`` into the reused
      ``recipient-picker-input`` → ``follow_ops::resolve_owner_address`` probes
      F's ``fauna.actor.by_handle`` (unknown there), the shared
      ``is_foreign_handle_domain`` verdict says foreign, and the anonymous hop
      resolves the handle AGAINST H → the record's ``home_nest_url`` is H's base
      URL from ``peer_nest_url`` → ``fauna.folders.public.fetch{nest_url}`` on F
      is RELAYED to H (``originate_folder_public_fetch``), pinning ``folder_id``
      and stamping H's ``home_nest_actor_id`` → the followed row renders → Media
      offers the scope, whose listing rides the same relay → the external-open
      confirm runs ``download_followed``: the foreign fetcher graduates H's SPKI
      pin against the stamped identity (``graduate_home_nest_pin`` — H's cert is
      self-signed, so WebPKI alone would refuse it) and fetches the manifest and
      chunks off H's open by-hash plane with no bearer → the handed-off bytes
      equal the published file.

    RED-prove (any one of these turns the follow, or the download, red):
    ``follow_ops::resolve_owner_address`` sending an empty ``home_nest_url``
    (the pre-2026-09-22 shape — H answers not-found through F's local arm);
    ``NativeForeignFetchers::fetcher_for`` dropping the identity (WebPKI refuses
    H's self-signed cert at the first GET); the devices-machine scope losing
    ``home_nest_actor_id`` on its way to Media (same).

    Convention 8: the owner's folder, audience flip and file row are fixture
    setup over the API on a nest no GUI drives; every mutation the follower
    makes — the follow, the browse, the open — goes through the tui UI.
    """
    import secrets

    from actions.media import MediaActions
    from common.auth import register_handled_actor

    from tests.api.test_public_folder_fetch import DEVICE_ID, _record, _set_audience
    from tests.api.test_web_paywall_folder import _actor_client

    driver, record_file, _runtime_dir = opened_app
    home = cross_nest_foreign
    home_url, home_port = home["url"], home["port"]

    # ── Fixture: an owner on H publishes a folder holding one clip. ──
    owner = register_handled_actor(
        home_port,
        handle="xnpub" + secrets.token_hex(3),
        domain=home["authority"],
        base_url=home_url,
    )
    folder = f"xnest-pub-{secrets.token_hex(3)}"
    with _actor_client(home_url, owner) as ws:
        fauna_ffi.harness_create_set(home_url, bytes(owner["signing_key"]), {"name": folder})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    _set_audience(home_url, owner, folder, "public")
    _record(home_url, home_port, owner, folder, "clip.mp4", CLIP_BYTES)
    with _actor_client(home_url, owner) as ws:
        folder_id = ws.call(
            "fauna.folders.public.fetch",
            {"owner_actor_id": owner["actor_id_hex"], "folder_name": folder, "since": 0},
        )["folder_id"]

    # ── The follow, through the UI, by `handle@domain` — never a nest url. ──
    address = f"{owner['handle']}@{home['authority']}"
    driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}}
    )
    driver.click("folder-follow-button")
    driver.wait_for("folder-follow-name-input", timeout=10)
    driver.clear_and_type("recipient-picker-input", address)
    driver.clear_and_type("folder-follow-name-input", folder)
    driver.click("folder-follow-confirm")
    try:
        driver.wait_for("folder-followed-item", timeout=30)
    except Exception:
        error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
        raise AssertionError(
            f"the cross-nest follow of {address!r} never produced a followed row; "
            f"error element: {error or '(none)'}"
        )

    # ── The browse on the relayed scope, then the open off H's byte plane. ──
    # The scope value is the machine-minted `(folder_id, home_nest_url)`
    # identity; the home url is what the handle's domain derived (uniform
    # https, the explicit loopback port honoured — `resolve_handle_domain`).
    driver.set_state({"nav": {"stack": [{"view": "media"}]}})
    driver.wait_for("media-view-toggle", timeout=15)
    media = MediaActions(driver)
    media.set_filter(f"followed:{folder_id}@https://{home['authority']}")
    media.wait_for_item_count(1)
    media.open_item_detail(0)
    driver.wait_for(BUTTON, timeout=15)
    driver.click(BUTTON)
    driver.wait_for(MODAL, timeout=10)
    driver.click(CONFIRM)

    paths = _wait_for_recorded(record_file, 1, timeout=30.0)
    error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
    assert len(paths) == 1, (
        "the followed file on the other nest never reached the opener — the "
        "keyless cross-nest read (pin graduation, relay, or byte fetch) failed; "
        f"error element: {error or '(none)'}"
    )
    with open(paths[0], "rb") as f:
        assert f.read() == CLIP_BYTES, (
            "the handed-off bytes are the file published on the other nest"
        )


def test_a_non_av_item_paints_no_trigger(opened_app, tmp_path):
    """A document's detail never paints the trigger, whatever the mode — the
    predicate is the shared ``fauna_core::share::is_audio_video_filename``."""
    driver, _record_file, _runtime_dir = opened_app
    _upload_and_open_detail(driver, tmp_path, "notes.txt", b"just text")
    assert driver.is_visible("media-item-detail-close-button")
    assert driver.is_absent(BUTTON), (
        "a non-audio/video item must not offer the external open"
    )
