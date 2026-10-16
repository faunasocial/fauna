"""tier_1: android's on-device reads from OUTSIDE the app — `app_data_exists`,
`AndroidScopeStore`, and the photo-permission pre-grant.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*Android's run venue* (no device path is ever a host path) and
`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy (what
the erase witnesses read: `<filesDir>/<actor>` and `<databases>/<actor>`,
`AccountStores.kt`).

WHY THIS FILE EXISTS. Every account-scoping witness on android reads a device
path through `adb run-as`; a mistake there is invisible on a device run,
because the failure mode is a SILENT False — and "the scope is gone" asserts
False. So the three properties a device run cannot tell apart from a pass are
pinned here with `subprocess.run` replaced by a recorder:

  1. only the marker the device shell prints on success reads as present (no
     exit status crosses `exec-out`);
  2. nothing but a plain relative path is ever spelled into the device command
     line;
  3. `AndroidScopeStore.exists` asks the device, never the host filesystem.
"""

import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))

from common.scope_store import AndroidScopeStore, attach_scope_store  # noqa: E402
from drivers import android as android_mod  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

ACTOR = "ab" * 32


@pytest.fixture
def recorded(monkeypatch):
    """Replace `subprocess.run` in the driver module; return (driver, calls,
    answers) — `answers` maps a substring of the joined argv to the stdout
    the fake device prints for it."""
    calls: list[list[str]] = []
    answers: dict[str, bytes | str] = {}

    def fake_run(argv, **kwargs):
        calls.append(list(argv))
        joined = " ".join(argv)
        out = next((v for k, v in answers.items() if k in joined), b"")
        if kwargs.get("text") and isinstance(out, bytes):
            out = out.decode()
        return SimpleNamespace(returncode=0, stdout=out, stderr=b"")

    monkeypatch.setattr(android_mod.subprocess, "run", fake_run)
    driver = AndroidBridgeDriver()
    monkeypatch.setattr(driver, "_adb_cmd", lambda: ["adb", "-s", "emulator-5554"])
    return driver, calls, answers


def test_present_only_when_the_device_prints_the_marker(recorded):
    driver, calls, answers = recorded
    answers["files/" + ACTOR] = b"FAUNA_PRESENT\n"
    assert driver.app_data_exists(f"files/{ACTOR}") is True
    assert driver.app_data_exists(f"databases/{ACTOR}") is False
    # A refusal printed on stdout (some images print run-as errors there) is
    # not presence.
    answers["databases/" + ACTOR] = b"run-as: package not debuggable"
    assert driver.app_data_exists(f"databases/{ACTOR}") is False


def test_the_device_command_is_run_as_from_outside_the_app(recorded):
    driver, calls, _ = recorded
    driver.app_data_exists(f"files/{ACTOR}")
    argv = calls[-1]
    assert argv[:4] == ["adb", "-s", "emulator-5554", "exec-out"]
    assert argv[4] == (
        f'run-as social.fauna.fauna sh -c "test -e files/{ACTOR} && echo FAUNA_PRESENT"'
    )


@pytest.mark.parametrize("bad", [
    "", "/data/user/0/social.fauna.fauna/files", "../files", "files/../x",
    "files//x", "files/x y", 'files/x";reboot;"', "files/$(id)", "files/./x",
])
def test_only_a_plain_relative_path_reaches_the_device(recorded, bad):
    driver, calls, _ = recorded
    with pytest.raises(ValueError):
        driver.app_data_exists(bad)
    assert calls == [], "a refused path must never reach adb"


def test_attach_scope_store_answers_android_with_the_two_device_roots(recorded):
    driver, _, _ = recorded
    store = attach_scope_store("android", driver)
    assert isinstance(store, AndroidScopeStore)
    assert store.scope_dirs(ACTOR.upper()) == [Path("files") / ACTOR, Path("databases") / ACTOR]


def test_scope_store_exists_asks_the_device_never_the_host(recorded, tmp_path, monkeypatch):
    driver, calls, answers = recorded
    store = attach_scope_store("android", driver)
    # A host directory of the same relative name must not answer for the device.
    monkeypatch.chdir(tmp_path)
    (tmp_path / "files" / ACTOR).mkdir(parents=True)
    assert store.exists(store.scope_dirs(ACTOR)[0]) is False
    answers["files/" + ACTOR] = b"FAUNA_PRESENT"
    assert store.exists(store.scope_dirs(ACTOR)[0]) is True
    assert all("run-as" in " ".join(c) for c in calls)


@pytest.mark.parametrize("sdk, granted", [
    ("34", ["android.permission.READ_MEDIA_IMAGES", "android.permission.READ_MEDIA_VIDEO"]),
    ("33", ["android.permission.READ_MEDIA_IMAGES", "android.permission.READ_MEDIA_VIDEO"]),
    ("32", ["android.permission.READ_EXTERNAL_STORAGE"]),
])
def test_photo_grant_follows_the_screens_own_sdk_split(recorded, sdk, granted):
    driver, calls, answers = recorded
    answers["getprop"] = sdk + "\n"
    driver._apply_permission_grants()
    assert calls == [], "no grant is applied unless the test asked for one"
    driver.grant_photos_access()
    driver._apply_permission_grants()
    grants = [c[-1] for c in calls if "grant" in c]
    assert grants == granted
    assert all(c[-3:-1] == ["grant", "social.fauna.fauna"] for c in calls if "grant" in c)
