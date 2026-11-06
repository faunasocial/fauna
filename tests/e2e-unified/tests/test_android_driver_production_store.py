"""tier_1: android's opt-in production-store launch.

Authority: `docs/goal/architecture/e2e-conventions.md` convention 12 (the Android
arm) and `docs/goal/architecture/apps/common.md` § Credential storage (the Android
row). The witness is
`tests/real_session/test_identity_rests_in_the_android_secure_prefs.py`.

WHY THIS FILE EXISTS. The app takes its production `SharedPrefsSecretBackend`
whenever the launch intent names no credential file, and the bridge names one
whenever `e2e_credentials.json` exists. So what selects the production store is
the file's ABSENCE, and the failure mode of getting that wrong is silent: a seed
of `{}` — which every `AndroidCredStore` launch sends — creates the file and puts
the whole "production" launch back on the e2e file arm. The properties a device
run could not tell apart from a pass are pinned here with `subprocess.run`
replaced by a recorder.
"""

import io
import os
import sys
import tarfile
from types import SimpleNamespace

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))

from drivers import android as android_mod  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

PACKAGE = "social.fauna.fauna"
STORE_FILES = (
    "shared_prefs/fauna_secure_prefs.xml",
    "shared_prefs/fauna_install_prefs.xml",
    "files/e2e_credentials.json",
    "files/install-secrets.json",
)


@pytest.fixture
def recorded(monkeypatch):
    """Replace `subprocess.run` in the driver module; return (driver, calls)."""
    calls: list[list[str]] = []

    def fake_run(argv, **kwargs):
        calls.append(list(argv))
        return SimpleNamespace(returncode=0, stdout=b"", stderr=b"")

    monkeypatch.setattr(android_mod.subprocess, "run", fake_run)
    driver = AndroidBridgeDriver()
    monkeypatch.setattr(driver, "_adb_cmd", lambda: ["adb", "-s", "emulator-5554"])
    return driver, calls


def _wipes(calls):
    return [c for c in calls if "rm" in c]


def test_an_empty_seed_never_reaches_the_bridge_on_a_production_store_launch(recorded):
    # The seed `AndroidCredStore` sends on every launch: writing it would create
    # the very file whose absence selects the production backend.
    driver, _ = recorded
    body = driver._session_body({"production_store": True, "seed_credentials": {}})
    assert "seed_credentials" not in body


def test_a_real_seed_on_a_production_store_launch_is_refused(recorded):
    # The witness's subject is what a real sign-in writes; a pre-seeded identity
    # would rest in the file arm and prove nothing, so it is an error, never ignored.
    driver, _ = recorded
    with pytest.raises(ValueError, match="production_store"):
        driver._session_body(
            {"production_store": True, "seed_credentials": {"fauna/index": "{}"}}
        )


def test_an_ordinary_launch_still_sends_its_seed(recorded):
    driver, _ = recorded
    body = driver._session_body({"seed_credentials": {}})
    assert body["seed_credentials"] == {}


def test_the_wipe_stops_the_app_then_removes_exactly_the_stores(recorded):
    driver, calls = recorded
    driver._wipe_production_store(check=False)
    assert calls[0][-3:] == ["am", "force-stop", PACKAGE], (
        "the app must be down first: a live process keeps its SharedPreferences in "
        "memory and writes them back"
    )
    wipe = _wipes(calls)
    assert len(wipe) == 1
    assert wipe[0][3:8] == ["shell", "run-as", PACKAGE, "rm", "-f"]
    assert tuple(wipe[0][8:]) == STORE_FILES


def test_teardown_gives_the_store_back_on_a_production_store_launch(recorded):
    driver, calls = recorded
    driver._launch_config = {"production_store": True}
    driver._stop_app()
    assert len(_wipes(calls)) == 1


def test_a_pinned_teardown_keeps_the_store_for_its_relaunch(recorded, monkeypatch):
    driver, calls = recorded
    driver._launch_config = {"production_store": True}
    monkeypatch.setattr(driver, "_relaunch_pinned", lambda: True)
    driver._stop_app()
    assert _wipes(calls) == []


def test_an_ordinary_teardown_never_touches_the_real_store(recorded):
    driver, calls = recorded
    driver._launch_config = {}
    driver._stop_app()
    assert _wipes(calls) == []


def _tar_of(members: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        link = tarfile.TarInfo("./escape")
        link.type = tarfile.SYMTYPE
        link.linkname = "/etc/passwd"
        tar.addfile(link)
    return buf.getvalue()


def test_pull_app_data_lays_out_regular_files_and_nothing_else(monkeypatch, tmp_path):
    tar = _tar_of({"./shared_prefs/fauna_secure_prefs.xml": b"<map/>", "./files/a/b": b"x"})
    calls = []

    def fake_run(argv, **kwargs):
        calls.append(list(argv))
        return SimpleNamespace(returncode=0, stdout=tar, stderr=b"")

    monkeypatch.setattr(android_mod.subprocess, "run", fake_run)
    driver = AndroidBridgeDriver()
    monkeypatch.setattr(driver, "_adb_cmd", lambda: ["adb"])

    root = driver.pull_app_data(tmp_path / "dump")

    assert calls[-1] == ["adb", "exec-out", "run-as", PACKAGE, "tar", "-cf", "-", "."]
    assert (root / "shared_prefs" / "fauna_secure_prefs.xml").read_bytes() == b"<map/>"
    assert (root / "files" / "a" / "b").read_bytes() == b"x"
    assert not (root / "escape").is_symlink()


def test_pull_app_data_refuses_an_answer_that_is_no_archive(monkeypatch, tmp_path):
    # `exec-out` carries no exit status, and run-as prints its refusal on stdout on
    # some images: an unreadable answer must not read as an empty, clean device.
    monkeypatch.setattr(
        android_mod.subprocess, "run",
        lambda argv, **kw: SimpleNamespace(
            returncode=0, stdout=b"run-as: package not debuggable", stderr=b""
        ),
    )
    driver = AndroidBridgeDriver()
    monkeypatch.setattr(driver, "_adb_cmd", lambda: ["adb"])
    with pytest.raises(RuntimeError, match="not debuggable"):
        driver.pull_app_data(tmp_path / "dump")
