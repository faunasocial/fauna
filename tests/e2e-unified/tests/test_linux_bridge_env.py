"""Unit tests for the Linux app's launch environment — the three ways the driver
isolates a harness-launched client from the box it runs on.

1. **Display** (`_headless_render_cmd_env`): regression guard for focus-stealing
   on Wayland / GNOME Remote Login (`--system` RDP) desktops. Launched headless
   under `xvfb-run`, fauna-desktop must be forced onto the Xvfb X display
   (`GDK_BACKEND=x11`, `WAYLAND_DISPLAY` dropped) so GTK4 doesn't prefer the real
   Wayland compositor and pop windows onto the developer's screen.
2. **Session bus** (`_wants_private_bus` / `_private_bus_env`): regression guard
   for the box's *desktop session* deciding a test's result. An app on the real
   session bus sees whatever `org.kde.StatusNotifierWatcher` gnome-shell happens
   to be publishing, which flips close-to-tray between quit and hide — so
   `wait_app_exit()` passed or failed by the hour (diagnosed 2026-07-17). Both
   carve-outs below are load-bearing; each has a test that fails if it is
   "simplified" away.
3. **XDG isolation** (`build_launch_env`): regression guard for the per-seat
   agent isolation the convention-16 `native` sync seat rests on
   (`e2e-conventions.md` § Implementation status today). Two launches that
   share `XDG_RUNTIME_DIR` share one `fauna-sync-agent` and one watch-folder
   engine — a pair test would go GREEN while proving nothing about
   device-to-device sync, the failure shape `e2e-conventions.md` calls "the
   worst failure shape available to this test". Pure, template `drivers/tui.py`'s
   `build_launch_env` (priorities #1/#3: two drivers doing one thing two ways).
"""

import os
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.linux import (
    _headless_render_cmd_env,
    _private_bus_env,
    _secret_service_bus_missing,
    _wants_private_bus,
    build_launch_env,
)

_APP = ["/opt/fauna/fauna-desktop"]

# A Wayland / RDP-style ambient environment (mirrors a Linux dev VM's `--system` session).
_WAYLAND_ENV = {
    "DISPLAY": ":0",  # XWayland, visible in the RDP view
    "WAYLAND_DISPLAY": "wayland-0",
    "XDG_SESSION_TYPE": "wayland",
}


class TestHeadlessWaylandFix:
    def test_xvfb_present_forces_x11_and_drops_wayland(self, monkeypatch):
        monkeypatch.setattr("drivers.linux.shutil.which", lambda _: "/usr/bin/xvfb-run")
        cmd, env = _headless_render_cmd_env(list(_APP), dict(_WAYLAND_ENV))

        assert cmd[:2] == ["xvfb-run", "-a"]
        assert cmd[2:] == _APP  # the app command is preserved after the wrapper
        assert env["GDK_BACKEND"] == "x11"
        assert "WAYLAND_DISPLAY" not in env
        # DISPLAY must be left intact — xvfb-run overrides it to the Xvfb at
        # exec; dropping it would let x11 fall back to the visible :0.
        assert env["DISPLAY"] == ":0"
        # Software-rendering safety under the virtual framebuffer.
        assert env["GSK_RENDERER"] == "cairo"

    def test_explicit_gsk_renderer_is_preserved(self, monkeypatch):
        monkeypatch.setattr("drivers.linux.shutil.which", lambda _: "/usr/bin/xvfb-run")
        _, env = _headless_render_cmd_env(
            list(_APP), {**_WAYLAND_ENV, "GSK_RENDERER": "ngl"}
        )
        assert env["GSK_RENDERER"] == "ngl"


class TestRealDisplayOptOut:
    def test_real_display_keeps_wayland_no_xvfb(self, monkeypatch):
        # Even if xvfb-run exists, FAUNA_E2E_REAL_DISPLAY=1 means "watch the UI".
        monkeypatch.setattr("drivers.linux.shutil.which", lambda _: "/usr/bin/xvfb-run")
        cmd, env = _headless_render_cmd_env(
            list(_APP), {**_WAYLAND_ENV, "FAUNA_E2E_REAL_DISPLAY": "1"}
        )

        assert "xvfb-run" not in cmd
        assert env["WAYLAND_DISPLAY"] == "wayland-0"
        assert "GDK_BACKEND" not in env


class TestXvfbMissingFallback:
    def test_xvfb_absent_leaves_env_untouched(self, monkeypatch):
        # No xvfb-run → render on the real display (no wrapper, env unchanged).
        monkeypatch.setattr("drivers.linux.shutil.which", lambda _: None)
        cmd, env = _headless_render_cmd_env(list(_APP), dict(_WAYLAND_ENV))

        assert cmd == _APP
        assert env["WAYLAND_DISPLAY"] == "wayland-0"
        assert "GDK_BACKEND" not in env


# A real GNOME session's ambient bus vars, as inherited via `os.environ`.
_AMBIENT_BUS_ENV = {
    "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
    "DBUS_STARTER_ADDRESS": "unix:path=/run/user/1000/bus",
    "DBUS_STARTER_BUS_TYPE": "session",
}
_PRIVATE = "unix:path=/tmp/fauna-e2e-session-bus-xyz/bus"


class TestPrivateBusDecision:
    def test_ordinary_launch_gets_a_private_bus(self):
        # The default, and the whole point: no ambient tray host can reach the app.
        assert _wants_private_bus({"app_path": "/opt/fauna/fauna-desktop"}) is True

    def test_empty_environment_block_still_gets_a_private_bus(self):
        assert _wants_private_bus({"environment": {}}) is True
        assert _wants_private_bus({"environment": {"FAUNA_DNS_PROVIDER_FAKE": "1"}}) is True

    def test_caller_supplied_bus_address_wins(self):
        # test_tray_close_to_tray.py drives host-present AND host-absent, so it
        # owns its bus; the driver must not overwrite it with a fixed default.
        assert _wants_private_bus(
            {"environment": {"DBUS_SESSION_BUS_ADDRESS": _PRIVATE}}
        ) is False

    def test_real_keyring_mode_rides_the_secret_service_bus_the_caller_owns(self):
        # That mode's subject is a real Secret Service IMPLEMENTATION, and the
        # test's `LibsecretCredStore` runs one of its own on a private bus
        # (`drivers/secret_service.py`), handed in through the caller-owned-bus
        # carve-out above. Until 2026-09-15 this mode kept the AMBIENT bus
        # instead, and its force-quit tests crashed the desktop keyring daemon
        # — which came back locked and took every other client's secrets with
        # it. The ambient bus is never the answer again.
        assert _wants_private_bus(
            {"use_real_keyring": True, "environment": {"DBUS_SESSION_BUS_ADDRESS": _PRIVATE}}
        ) is False
        assert _wants_private_bus({"use_real_keyring": True}) is True

    def test_real_keyring_mode_without_a_secret_service_bus_is_refused(self):
        # Not a private bus owning nothing (the app would fast-fail every
        # credential read and the test would fail obscurely), not the ambient
        # bus (the machine-wide hazard above): a loud refusal at launch.
        assert _secret_service_bus_missing({"use_real_keyring": True}) is True
        assert _secret_service_bus_missing({"use_real_keyring": True, "environment": {}}) is True
        assert _secret_service_bus_missing(
            {"use_real_keyring": True, "environment": {"DBUS_SESSION_BUS_ADDRESS": _PRIVATE}}
        ) is False
        assert _secret_service_bus_missing({"app_path": "/opt/fauna/fauna-desktop"}) is False


class TestPrivateBusEnv:
    def test_points_at_the_private_bus_and_drops_the_ambient_pointers(self):
        env = _private_bus_env(dict(_AMBIENT_BUS_ENV), _PRIVATE)

        assert env["DBUS_SESSION_BUS_ADDRESS"] == _PRIVATE
        # A stale second pointer at the real bus is a way back to the box's session.
        assert "DBUS_STARTER_ADDRESS" not in env
        assert "DBUS_STARTER_BUS_TYPE" not in env

    def test_disables_the_a11y_bus_lookup(self):
        # The private bus does no service activation, so GTK's a11y-bus lookup
        # would only `ServiceUnknown`-warn; the agent reads the widget tree directly.
        env = _private_bus_env(dict(_AMBIENT_BUS_ENV), _PRIVATE)
        assert env["GTK_A11Y"] == "none"

    def test_leaves_unrelated_env_alone(self):
        env = _private_bus_env(
            {**_AMBIENT_BUS_ENV, "FAUNA_E2E_AGENT_PORT": "45001"}, _PRIVATE
        )
        assert env["FAUNA_E2E_AGENT_PORT"] == "45001"


class TestBuildLaunchEnvIsolatesTheInstance:
    def test_two_launches_with_no_xdg_base_get_different_dirs(self, tmp_path):
        # The false-green risk this whole seam exists to close: two seats
        # sharing an agent socket would pair GREEN while proving nothing.
        tmp_a = tmp_path / "a"
        tmp_a.mkdir()
        tmp_b = tmp_path / "b"
        tmp_b.mkdir()
        env_a = build_launch_env({}, 40001, str(tmp_a))
        env_b = build_launch_env({}, 40002, str(tmp_b))

        assert env_a["XDG_RUNTIME_DIR"] != env_b["XDG_RUNTIME_DIR"]
        assert env_a["FAUNA_KEYRING_APP"] != env_b["FAUNA_KEYRING_APP"]
        assert env_a["FAUNA_E2E_CREDENTIAL_DIR"] != env_b["FAUNA_E2E_CREDENTIAL_DIR"]
        for d in (
            env_a["XDG_DATA_HOME"],
            env_a["XDG_CONFIG_HOME"],
            env_a["XDG_RUNTIME_DIR"],
            env_a["FAUNA_E2E_CREDENTIAL_DIR"],
        ):
            assert os.path.isdir(d)

    def test_build_launch_env_isolates_the_instance(self, tmp_path):
        env = build_launch_env({}, 12345, str(tmp_path))
        assert env["FAUNA_E2E_AGENT_PORT"] == "12345"
        assert env["FAUNA_KEYRING_APP"] == "fauna-e2e-agent-12345"
        for var, sub in (
            ("XDG_DATA_HOME", "data"),
            ("XDG_CONFIG_HOME", "config"),
            ("FAUNA_E2E_CREDENTIAL_DIR", "creds"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ):
            assert env[var] == str(tmp_path / sub)
            assert os.path.isdir(env[var])

    def test_build_launch_env_applies_config_environment(self, tmp_path):
        env = build_launch_env(
            {"environment": {"FAUNA_TEST_FLAG": "1"}}, 1, str(tmp_path)
        )
        assert env["FAUNA_TEST_FLAG"] == "1"

    def test_a_caller_supplied_xdg_base_wins(self, tmp_path):
        # The existing carve-out `preserve_state_across_relaunch` relies on:
        # a relaunch reuses the SAME pinned dirs rather than the driver's
        # fresh-per-launch default.
        pinned = tmp_path / "pinned"
        pinned.mkdir()
        scratch = tmp_path / "scratch"
        scratch.mkdir()

        first = build_launch_env({"xdg_base": str(pinned)}, 2, str(scratch))
        second = build_launch_env({"xdg_base": str(pinned)}, 3, str(scratch))

        assert first["XDG_DATA_HOME"] == second["XDG_DATA_HOME"] == str(pinned / "data")
        assert (
            first["XDG_CONFIG_HOME"]
            == second["XDG_CONFIG_HOME"]
            == str(pinned / "config")
        )
        assert (
            first["XDG_RUNTIME_DIR"]
            == second["XDG_RUNTIME_DIR"]
            == str(pinned / "runtime")
        )

    def test_a_caller_supplied_keyring_app_and_credential_dir_win(self, tmp_path):
        creds = tmp_path / "chosen-creds"
        env = build_launch_env(
            {"keyring_app": "chosen-app", "credential_dir": str(creds)},
            4,
            str(tmp_path),
        )
        assert env["FAUNA_KEYRING_APP"] == "chosen-app"
        assert env["FAUNA_E2E_CREDENTIAL_DIR"] == str(creds)
        assert os.path.isdir(creds)

    def test_use_real_keyring_mode_skips_the_credential_dir_and_runtime_isolation(
        self, tmp_path
    ):
        """`use_real_keyring` reproduces the production path: a stable keyring
        namespace + stable XDG dirs, and deliberately NO
        `FAUNA_E2E_CREDENTIAL_DIR` (every credential slot routes to the real
        Secret Service instead). It does not isolate `XDG_RUNTIME_DIR` either
        — an existing gap this lift preserves rather than introduces; only
        `test_onboarding_launch_routing_smoke.py` uses this mode, on a real
        desktop session where the ambient runtime dir is what the box already
        isolates per-user.
        """
        env = build_launch_env(
            {
                "use_real_keyring": True,
                "xdg_base": str(tmp_path),
                "keyring_app": "stable-app",
            },
            5,
            str(tmp_path),
        )
        assert env["FAUNA_KEYRING_APP"] == "stable-app"
        assert env["XDG_DATA_HOME"] == str(tmp_path / "data")
        assert env["XDG_CONFIG_HOME"] == str(tmp_path / "config")
        assert "FAUNA_E2E_CREDENTIAL_DIR" not in env
        assert env.get("XDG_RUNTIME_DIR") == os.environ.get("XDG_RUNTIME_DIR")
