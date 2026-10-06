r"""tier_1: a windows e2e launch is isolated from the box's real user profile.

E2E rule 10 (`docs/goal/architecture/testing.md` § Cross-app e2e conventions, point
10): "A client launch is isolated from the box it runs on." A launched client inherits
its launcher's whole world, so every channel must be closed **by default** — not only
when a test remembers to ask.

`drivers/windows.py` used to set `FAUNA_E2E_DATA_DIR` only when a test explicitly
supplied `data_dir` (just the version-skew at-rest grid and `alice_second_device`).
Every OTHER windows e2e launch therefore ran against the machine's real
``%LocalAppData%\Fauna`` — the same ``mls.db``, ``logs/fauna.log.<date>``,
``drafts.json``, ``config-replica`` and ``nest_identity_pins.json`` that the
**installed production app** uses. On a dev box that app is live (auto-start at
sign-in is default ON), so the suite and a real ``FaunaApp.exe --autostart`` shared
one SQLite store and one rolling log, and a test result could depend on what the
developer's desktop happened to be doing. This is the windows twin of the linux
D-Bus finding rule 10 was ratified from.

These tests pin the DEFAULT, which is the whole point — a test asserting only that
an explicitly-supplied dir is honored would have passed the entire time the default
leaked. They drive the real ``launch()`` (bridge process, HTTP and the app POST
stubbed) rather than an extracted helper, so the assertion covers the env the app is
actually started with.
"""

import os
import shutil
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]


class _FakeProc:
    """Stands in for the `dotnet run` bridge process: reports a port, stays alive."""

    def __init__(self):
        # pid=0 is never a real, openable process on Windows — reap_descendants_of
        # (called on every real launch()) degrades to a no-op False rather than
        # touching anything live.
        self.pid = 0
        self.stdout = self
        self.stderr = self
        self._lines = iter(["BRIDGE_PORT=54321\n"])

    def readline(self):
        return next(self._lines, "")

    def read(self):
        return ""

    def poll(self):
        return None

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


@pytest.fixture
def driver(monkeypatch):
    """A `WindowsBridgeDriver` whose `launch()` runs for real against stubs.

    Only the outward calls are replaced (bridge build, process spawn, HTTP, the
    2s dialog-dismiss sleep) — the env-building and dir-ownership logic under test
    runs verbatim.
    """
    monkeypatch.setattr(win, "_ensure_bridge_built", lambda: None)
    monkeypatch.setattr(win.subprocess, "Popen", lambda *a, **k: _FakeProc())
    monkeypatch.setattr(win.time, "sleep", lambda *_a, **_k: None)

    d = win.WindowsBridgeDriver()
    monkeypatch.setattr(d, "_get", lambda *a, **k: {"ready": True})
    monkeypatch.setattr(d, "_post", lambda *a, **k: {})
    monkeypatch.setattr(d, "_delete", lambda *a, **k: {})
    # recover() polls _await_health() (-> _probe_health(), a real HTTP call),
    # not _health_raw (unused by the driver itself — see http_bridge.py); stub
    # the one actually on the path or recover() hits a real socket refusal.
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)
    yield d
    d.teardown()


def _launch_env(d, config=None):
    d.launch({"app_path": "FaunaApp.exe", **(config or {})})
    return d._session_body["environment"]


def _real_profile() -> Path | None:
    local = os.environ.get("LOCALAPPDATA")
    return Path(local) / "Fauna" if local else None


def test_default_launch_does_not_use_the_real_user_profile(driver):
    """The regression: a plain launch must NOT resolve to %LocalAppData%\\Fauna."""
    env = _launch_env(driver)

    data_dir = env.get("FAUNA_E2E_DATA_DIR")
    assert data_dir, (
        "a default windows launch set no FAUNA_E2E_DATA_DIR, so the app falls back "
        "to BackupPaths.DataDir = the real %LocalAppData%\\Fauna — the installed "
        "app's own profile (e2e rule 10)"
    )
    assert os.path.isdir(data_dir), f"{data_dir} was not created before launch"

    real = _real_profile()
    if real is not None:
        assert Path(data_dir).resolve() != real.resolve(), (
            f"the launch data dir IS the real profile ({real})"
        )


def test_default_data_dir_survives_recover_as_a_same_device_restart(driver):
    """`recover()` re-posts the same session ⇒ the SAME store.

    Windows' documented property (unlike linux's fresh-mkdtemp `recover()`):
    `mls.db` survives a relaunch, which is what makes the conversation-restart
    tests a genuine same-device restart. Isolation must not change that.
    """
    first = _launch_env(driver)["FAUNA_E2E_DATA_DIR"]
    assert driver.recover() is True
    after = driver._session_body["environment"]["FAUNA_E2E_DATA_DIR"]

    assert after == first, "recover() moved the data dir — that is a NEW device, not a restart"
    assert os.path.isdir(after)


def test_a_caller_supplied_data_dir_still_wins(driver, tmp_path):
    """The at-rest grid / `alice_second_device` seam keeps precedence."""
    pinned = tmp_path / "pinned-root"
    env = _launch_env(driver, {"data_dir": str(pinned)})

    assert env["FAUNA_E2E_DATA_DIR"] == str(pinned)
    assert pinned.is_dir()


def test_teardown_reclaims_only_a_dir_the_driver_created(tmp_path, monkeypatch):
    """Ownership mirrors the credential-store contract.

    A caller-supplied root is handed to a SEQUENCE of driver instances (a later
    phase launches a fresh driver against it and expects to find what this phase
    wrote), so teardown must leave it alone; the per-instance default is ours.
    """
    monkeypatch.setattr(win, "_ensure_bridge_built", lambda: None)
    monkeypatch.setattr(win.subprocess, "Popen", lambda *a, **k: _FakeProc())
    monkeypatch.setattr(win.time, "sleep", lambda *_a, **_k: None)

    def _stub(d):
        monkeypatch.setattr(d, "_get", lambda *a, **k: {"ready": True})
        monkeypatch.setattr(d, "_post", lambda *a, **k: {})
        monkeypatch.setattr(d, "_delete", lambda *a, **k: {})
        monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)
        return d

    owned = _stub(win.WindowsBridgeDriver())
    default_dir = _launch_env(owned)["FAUNA_E2E_DATA_DIR"]
    owned.teardown()
    assert not os.path.isdir(default_dir), "teardown leaked the per-instance data dir"

    pinned = tmp_path / "outlives-me"
    supplied = _stub(win.WindowsBridgeDriver())
    _launch_env(supplied, {"data_dir": str(pinned)})
    supplied.teardown()
    assert pinned.is_dir(), (
        "teardown deleted a CALLER-supplied data dir — the at-rest grid's next phase "
        "would find an empty root and prove nothing"
    )


def test_download_dir_follows_the_isolated_data_dir(driver):
    """`download_dir()` mirrors the app's `SnapshotFileSaver` fallback.

    The app builds it from `BackupPaths.DataDir`, so the harness-side reader must
    too — a hand-built `%LocalAppData%` path here would read a directory the
    isolated app never writes to.
    """
    env = _launch_env(driver)
    expected = os.path.join(env["FAUNA_E2E_DATA_DIR"], "e2e-downloads")

    assert driver.download_dir() == expected


def test_an_explicit_download_dir_override_still_wins(driver, tmp_path):
    env = {"FAUNA_E2E_DOWNLOAD_DIR": str(tmp_path / "dl")}
    _launch_env(driver, {"environment": env})

    assert driver.download_dir() == str(tmp_path / "dl")


# ----------------------------------------------------------------------------
# %LOCALAPPDATA% — the unified store root's ancestor (the third windows axis)
#
# `FAUNA_E2E_DATA_DIR` above closes only the app's OWN flat base. Shared Rust
# never reads it: `fauna_account_store::root::production_base()` reads
# `LOCALAPPDATA` and returns `%LOCALAPPDATA%\Fauna\sync`, the unified account-store
# root that `AccountStateDir.Erase`/`EraseAll` sweep by passing
# `storeContainer: null` (→ `StoreRoot::platform()`). Until the driver relocated
# the ancestor, a UI sign-out under e2e ran `remove_dir_all` over EVERY 64-hex
# actor dir in the developer's real profile. These pin the DEFAULT, for the same
# reason the data-dir tests above do.
# ----------------------------------------------------------------------------


def test_default_launch_relocates_localappdata_away_from_the_real_profile(driver):
    """The regression: a plain launch must not leave the app on the real `%LOCALAPPDATA%`."""
    env = _launch_env(driver)

    local = env.get("LOCALAPPDATA")
    assert local, (
        "a default windows launch set no LOCALAPPDATA, so shared Rust's "
        "production_base() resolves the real %LOCALAPPDATA%\\Fauna\\sync — the unified "
        "account-store root a UI sign-out sweeps with remove_dir_all (e2e rule 10)"
    )
    assert os.path.isdir(local), f"{local} was not created before launch"

    real = os.environ.get("LOCALAPPDATA")
    if real:
        assert Path(local).resolve() != Path(real).resolve(), (
            f"the launch %LOCALAPPDATA% IS the real one ({real})"
        )


def test_the_published_store_root_is_the_shared_rust_derivation(driver):
    """`_resolved_store_root` must be `production_base()`'s own path, not a guess.

    `tests/common/scope_store.py` asserts a sign-out erased this directory, so a
    harness-side path that did not match what `StoreRoot::platform()` resolved
    would make the erase test pass against a directory nothing ever wrote to.
    """
    env = _launch_env(driver)
    expected = os.path.join(env["LOCALAPPDATA"], "Fauna", "sync")

    assert driver._resolved_store_root == expected
    assert driver._resolved_local_appdata == env["LOCALAPPDATA"]


def test_the_default_data_dir_hangs_off_the_relocated_localappdata(driver):
    """Production's relationship between the two roots, reproduced.

    `BackupPaths.DataDir` is `%LocalAppData%\\Fauna` and the store root is its
    `sync` child, so isolating the shared ancestor keeps the isolated tree
    shaped like the real one — the same one-ancestor relocation linux/tui
    (`XDG_CONFIG_HOME`) and macOS (`HOME`) rely on.
    """
    env = _launch_env(driver)

    assert env["FAUNA_E2E_DATA_DIR"] == os.path.join(env["LOCALAPPDATA"], "Fauna")
    assert driver._resolved_store_root == os.path.join(env["FAUNA_E2E_DATA_DIR"], "sync")


def test_localappdata_survives_recover_as_a_same_device_restart(driver):
    """Same contract as the data dir: `recover()` is a restart, not a new device."""
    first = _launch_env(driver)["LOCALAPPDATA"]
    assert driver.recover() is True
    after = driver._session_body["environment"]["LOCALAPPDATA"]

    assert after == first, "recover() moved %LOCALAPPDATA% — that is a NEW device"
    assert os.path.isdir(after)


def test_a_caller_supplied_data_dir_is_not_reparented(driver, tmp_path):
    """The at-rest grid / `alice_second_device` seam keeps its EXACT path.

    Those seams hand one root to a sequence of driver instances, so re-parenting
    it under this instance's throwaway `%LOCALAPPDATA%` would hand the next phase
    a different (empty) root. `%LOCALAPPDATA%` is still relocated — the two roots
    just stop being nested, which is why the store root is published separately
    rather than derived from the data dir.
    """
    pinned = tmp_path / "pinned-root"
    env = _launch_env(driver, {"data_dir": str(pinned)})

    assert env["FAUNA_E2E_DATA_DIR"] == str(pinned)
    assert env["LOCALAPPDATA"] != str(pinned)
    assert driver._resolved_store_root == os.path.join(env["LOCALAPPDATA"], "Fauna", "sync")


def test_teardown_reclaims_the_relocated_localappdata_it_created(tmp_path, monkeypatch):
    """Ownership mirrors the credential/data-dir contract."""
    monkeypatch.setattr(win, "_ensure_bridge_built", lambda: None)
    monkeypatch.setattr(win.subprocess, "Popen", lambda *a, **k: _FakeProc())
    monkeypatch.setattr(win.time, "sleep", lambda *_a, **_k: None)

    def _stub(d):
        monkeypatch.setattr(d, "_get", lambda *a, **k: {"ready": True})
        monkeypatch.setattr(d, "_post", lambda *a, **k: {})
        monkeypatch.setattr(d, "_delete", lambda *a, **k: {})
        monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)
        return d

    owned = _stub(win.WindowsBridgeDriver())
    default_local = _launch_env(owned)["LOCALAPPDATA"]
    owned.teardown()
    assert not os.path.isdir(default_local), "teardown leaked the relocated %LOCALAPPDATA%"

    pinned = tmp_path / "outlives-me"
    supplied = _stub(win.WindowsBridgeDriver())
    _launch_env(supplied, {"local_appdata": str(pinned)})
    supplied.teardown()
    assert pinned.is_dir(), "teardown deleted a CALLER-supplied %LOCALAPPDATA% root"


def test_keeping_the_data_dir_keeps_the_root_that_contains_it(monkeypatch):
    """`FAUNA_E2E_KEEP_DATA_DIR` must not be defeated by the parent sweep.

    The default data dir lives INSIDE the relocated `%LOCALAPPDATA%`, so
    reclaiming the parent would delete the very dir the flag just said it was
    keeping — and that dir is the only windows-side record of what the shared
    crates decided (`<data_dir>/logs/fauna.log.<date>`).
    """
    monkeypatch.setattr(win, "_ensure_bridge_built", lambda: None)
    monkeypatch.setattr(win.subprocess, "Popen", lambda *a, **k: _FakeProc())
    monkeypatch.setattr(win.time, "sleep", lambda *_a, **_k: None)
    monkeypatch.setenv("FAUNA_E2E_KEEP_DATA_DIR", "1")

    d = win.WindowsBridgeDriver()
    monkeypatch.setattr(d, "_get", lambda *a, **k: {"ready": True})
    monkeypatch.setattr(d, "_post", lambda *a, **k: {})
    monkeypatch.setattr(d, "_delete", lambda *a, **k: {})
    monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)

    env = _launch_env(d)
    data_dir, local = env["FAUNA_E2E_DATA_DIR"], env["LOCALAPPDATA"]
    try:
        d.teardown()
        assert os.path.isdir(data_dir), "FAUNA_E2E_KEEP_DATA_DIR did not keep the data dir"
        assert os.path.isdir(local)
    finally:
        shutil.rmtree(local, ignore_errors=True)


# ── Convention 10's windows focus axis: a harness launch never ACTIVATES ─────
#
# The person working on Windows shares the Windows session every harness-launched
# FaunaApp lives in, so an activating launch takes their keyboard focus — dozens
# of times per run. The app shows its window without activation only when the
# driver passes `NO_ACTIVATE_ARG` (a test-agent build's `App.xaml.cs`
# `ShowLaunchWindow`); these pin that the driver does so BY DEFAULT and keeps
# doing so across `recover()`, which re-posts the args minus `--nest-url`.


def _launch_args(d, config=None):
    d.launch({"app_path": "FaunaApp.exe", **(config or {})})
    return d._session_body["args"].split()


def test_default_launch_asks_the_app_not_to_activate_its_window(driver):
    assert win.NO_ACTIVATE_ARG in _launch_args(driver), (
        "a default windows launch did not pass the no-activate argument, so the app "
        "activates its window and takes the keyboard focus from the person at this "
        "desktop (e2e convention 10, the windows focus axis)"
    )


def test_no_activate_survives_recover_even_when_nest_url_is_stripped(driver, monkeypatch):
    """`recover()` re-tokenises the frozen args to strip `--nest-url`; the
    no-activate argument must come through that intact, or every per-module cold
    relaunch steals the focus again. Read off the body `recover()` actually POSTs
    — it strips a copy and leaves `_session_body` frozen."""
    args = _launch_args(driver, {"url": "http://127.0.0.1:1"})
    assert "--nest-url" in args and win.NO_ACTIVATE_ARG in args
    posted = []
    monkeypatch.setattr(
        driver, "_post",
        lambda path, data=None, *a, **k: posted.append((path, data)) or {})
    assert driver.recover() is True
    sessions = [data for path, data in posted if path == "/session"]
    assert sessions, "recover() posted no /session body"
    after = sessions[-1]["args"].split()
    assert "--nest-url" not in after, "precondition: recover() strips --nest-url"
    assert after.count(win.NO_ACTIVATE_ARG) == 1, (
        f"recover() lost or duplicated the no-activate argument: {after!r}"
    )


def test_activate_window_opts_out_explicitly(driver):
    """The opt-out for a test whose subject is the activation itself."""
    assert win.NO_ACTIVATE_ARG not in _launch_args(driver, {"activate_window": True})
