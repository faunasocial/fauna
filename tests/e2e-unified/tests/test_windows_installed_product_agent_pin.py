"""The installed-product journey must launch its app UNPINNED, so the installed
app spawns the installer's own agent.

`test_installer.py::TestFullJourneyInstalledApp` exists to prove the MSI-installed
`FaunaApp.exe` brings up the MSI-installed `fauna-sync-agent.exe`. The installed
app is built with the test-flavored FFI (the journey drives it through the e2e
agent), so it honours the harness agent pin `FAUNA_E2E_SYNC_AGENT_BIN`
(`libs/fauna-client-sync` `AGENT_BIN_ENV`). The windows driver defaults that
pin to the dev tree's `target\\debug\\fauna-sync-agent.exe` on every launch.
Measured 2026-09-27, on the first elevated run with a buildable MSI: the
installed app spawned the dev-tree agent and the journey went red. The subject
under test had been swapped by the harness itself.

An EMPTY pin is the contract's own "not pinned": Rust `pinned_agent_binary` maps
empty to `None`, and C# `E2eEnv.SyncAgentBin` is read with `IsNullOrEmpty`. So
conftest writes `""` for an installed-product session, and the driver's default
must leave a caller-supplied value, empty included, alone.
"""

import types

import pytest

pytestmark = pytest.mark.tier_1

AGENT_BIN = "FAUNA_E2E_SYNC_AGENT_BIN"


def _e2e_conftest():
    import conftest as mod

    assert hasattr(mod, "_prebuild_binaries"), (
        f"'import conftest' resolved to {mod.__file__!r} — expected "
        "tests/e2e-unified/conftest.py"
    )
    return mod


class _Item:
    def __init__(self, markers=()):
        self._markers = set(markers)

    def get_closest_marker(self, name):
        return object() if name in self._markers else None


def _request(*items):
    return types.SimpleNamespace(session=types.SimpleNamespace(items=list(items)))


def test_an_installed_product_session_launches_its_app_unpinned():
    mod = _e2e_conftest()
    env = {}
    mod._apply_installed_product_agent_env(
        env, _request(_Item({"installed_product", "real_sync_agent"}))
    )
    assert env == {AGENT_BIN: ""}


def test_a_dev_build_session_is_left_for_the_driver_to_pin():
    mod = _e2e_conftest()
    env = {}
    mod._apply_installed_product_agent_env(env, _request(_Item({"real_sync_agent"})))
    assert AGENT_BIN not in env


def test_the_driver_default_never_overrides_an_explicit_unpin():
    from drivers import windows as win_driver

    env = {AGENT_BIN: ""}
    win_driver._default_sync_agent_pin(env)
    assert env[AGENT_BIN] == ""


def test_the_driver_still_pins_the_dev_agent_by_default():
    from drivers import windows as win_driver

    env = {}
    win_driver._default_sync_agent_pin(env)
    assert env[AGENT_BIN] == str(win_driver._SYNC_AGENT_EXE)
