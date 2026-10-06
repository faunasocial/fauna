"""tier_1 unit tests for drivers/tui.py pure logic — no driver, no app, no nest.

Template: test_linux_bridge_env.py. Listed in conftest's
_CLIENT_INDEPENDENT_FILES so it never acquires a client parametrization.
"""

import os
import sys
import tempfile
from pathlib import Path, PurePosixPath

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import create_driver  # noqa: E402
from drivers.tui import (  # noqa: E402
    _AGENT_SOCKET_RELPATH,
    _SUN_PATH_MAX,
    TuiDriver,
    build_launch_env,
    launch_tmp_root,
)

pytestmark = pytest.mark.tier_1


def test_create_driver_returns_tui_driver():
    driver = create_driver("tui")
    assert isinstance(driver, TuiDriver)
    assert not driver.is_mobile()
    assert not driver.is_web()
    assert not driver.is_linux()


def test_build_launch_env_isolates_the_instance(tmp_path):
    env = build_launch_env({}, 12345, str(tmp_path))
    assert env["FAUNA_E2E_AGENT_PORT"] == "12345"
    assert env["FAUNA_KEYRING_APP"] == "fauna-e2e-agent-12345"
    assert env["TERM"] == "xterm-256color"
    # Fresh per-launch XDG dirs + file-backed credential store, all created.
    for var, sub in (
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("FAUNA_E2E_CREDENTIAL_DIR", "creds"),
    ):
        assert env[var] == str(tmp_path / sub)
        assert os.path.isdir(env[var])


def test_build_launch_env_applies_config_environment(tmp_path):
    env = build_launch_env({"environment": {"FAUNA_TEST_FLAG": "1"}}, 1, str(tmp_path))
    assert env["FAUNA_TEST_FLAG"] == "1"


def test_build_launch_env_keeps_an_exported_rust_log_beside_the_configured_one(
    tmp_path, monkeypatch
):
    """A human's exported ``RUST_LOG`` survives a launch config that sets its own.

    The harness's launch configs set ``RUST_LOG`` to their diagnostic targets,
    and ``env.update`` used to replace the exported value wholesale — a
    diagnosis run asking for ``fauna_anon_client=debug`` got only the harness's
    targets, silently. Both sets of directives must reach the app.
    """
    monkeypatch.setenv("RUST_LOG", "fauna_anon_client=debug")
    env = build_launch_env(
        {"environment": {"RUST_LOG": "info,fauna_conversations=debug"}}, 1, str(tmp_path)
    )
    assert env["RUST_LOG"] == "info,fauna_conversations=debug,fauna_anon_client=debug"


def test_build_launch_env_uses_the_configured_rust_log_when_none_is_exported(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("RUST_LOG", raising=False)
    env = build_launch_env(
        {"environment": {"RUST_LOG": "info,fauna_conversations=debug"}}, 1, str(tmp_path)
    )
    assert env["RUST_LOG"] == "info,fauna_conversations=debug"


def test_build_launch_env_lets_a_caller_own_the_runtime_dir(tmp_path):
    """A caller-passed ``XDG_RUNTIME_DIR`` wins over the private default.

    The isolation default (a per-launch runtime dir, so the agent's control
    socket can't be the machine-global one) must not clobber an explicit
    caller value: that dir is also the Linux external-media handoff base, so a
    test asserting on the handed-off path has to name it. The carve-out mirrors
    the linux driver's caller-owned ``DBUS_SESSION_BUS_ADDRESS``.
    """
    chosen = tmp_path / "caller-runtime"
    env = build_launch_env(
        {"environment": {"XDG_RUNTIME_DIR": str(chosen)}}, 2, str(tmp_path)
    )
    assert env["XDG_RUNTIME_DIR"] == str(chosen)
    assert os.path.isdir(env["XDG_RUNTIME_DIR"])
    # …and with nothing passed, the private default still applies.
    default = build_launch_env({}, 3, str(tmp_path))
    assert default["XDG_RUNTIME_DIR"] == str(tmp_path / "runtime")


def test_build_launch_env_relocates_home_on_macos(tmp_path):
    """A macOS tui launch must relocate ``HOME`` — the agent socket derives from it.

    ``fauna_ipc::unix_transport::default_socket_path`` branches on the platform:
    linux reads ``$XDG_RUNTIME_DIR`` (which this driver already isolates), but
    **macOS derives the socket from ``dirs::home_dir()``** —
    ``~/Library/Application Support/Fauna/sync-agent.sock`` — and consults no XDG
    variable at all. So on darwin the XDG isolation below buys nothing for the
    agent: a launch inheriting the real ``HOME`` resolves the *machine-global*
    socket, which on a dev box is bound by the installed ``/Applications/
    Fauna.app`` agent. The tui app would then drive the developer's real
    sync agent — binding e2e folders and pushing content keys into the real
    install, against the live nest.

    That is testing.md point 10's machine-global leak class, and the same shape
    that hid the broken windows and macOS seats behind a shared agent
    (2026-07-24). It is caught here rather than at runtime because the symptom
    is silent: the seat looks bound and syncs someone else's state.
    """
    env = build_launch_env({}, 9, str(tmp_path), sys_platform="darwin")
    home = str(tmp_path / "home")
    assert env["HOME"] == home
    assert env["CFFIXED_USER_HOME"] == home
    assert os.path.isdir(home)
    # The derived socket is the launch's own, never the real user's.
    sock = Path(env["HOME"]) / _AGENT_SOCKET_RELPATH
    assert str(sock).startswith(str(tmp_path))
    assert sock != Path(os.path.expanduser("~")) / _AGENT_SOCKET_RELPATH


def test_build_launch_env_leaves_home_alone_off_macos(tmp_path):
    """Only darwin relocates ``HOME``; linux isolation is XDG-derived already.

    The linux socket path comes from ``$XDG_RUNTIME_DIR`` (isolated above), so
    linux needs no ``HOME`` move — and its tui seat is the one with a long green
    record. Relocating it there would be an unverified change to the only
    working seat, so the branch is deliberately darwin-only.
    """
    env = build_launch_env({}, 10, str(tmp_path), sys_platform="linux")
    assert env["HOME"] == os.environ.get("HOME", "")
    assert "CFFIXED_USER_HOME" not in env


def test_build_launch_env_lets_a_caller_pin_the_macos_home(tmp_path):
    """An explicit ``config["home"]`` wins, mirroring ``drivers/macos.py``."""
    chosen = tmp_path / "pinned"
    env = build_launch_env(
        {"home": str(chosen)}, 11, str(tmp_path), sys_platform="darwin"
    )
    assert env["HOME"] == str(chosen)
    assert env["CFFIXED_USER_HOME"] == str(chosen)
    assert os.path.isdir(env["HOME"])


def test_build_launch_env_rejects_an_unbindable_macos_home(tmp_path):
    """An over-long pinned home fails LOUDLY at build, not at bind.

    macOS caps ``sun_path`` at 104 bytes and the socket adds a fixed 50-byte
    suffix to HOME. An overshoot makes the agent spawn and die at bind with a
    bare ``path must be shorter than SUN_LEN`` while the app reports only
    ``agent unreachable`` — the failure that stopped the macOS seat on
    2026-07-24 and cost three wrong diagnoses. The twin of ``drivers/macos.py``'s
    launch-time guard, at the tui driver's own door.
    """
    long_home = tmp_path / ("d" * 90)
    real_agent = {"environment": {"FAUNA_E2E_REAL_SYNC_AGENT": "1"}}
    with pytest.raises(RuntimeError, match="sun_path"):
        build_launch_env(
            {"home": str(long_home), **real_agent}, 12, str(tmp_path),
            sys_platform="darwin",
        )
    # Gated on the real-agent marker: a launch that spawns no agent may sit at a
    # long path (pytest's own tmp_path already does), and only isolation applies.
    env = build_launch_env(
        {"home": str(long_home)}, 13, str(tmp_path), sys_platform="darwin"
    )
    assert env["HOME"] == str(long_home)


def test_tui_launch_root_derives_a_bindable_sync_agent_socket():
    """The default macOS tui launch root must fit the 104-byte ``sun_path``.

    Measured against a REAL ``mkdtemp`` under the driver's own root, so the
    opaque-suffix length is counted rather than assumed — macOS's default
    ``TMPDIR`` (``/var/folders/<28 opaque chars>/T/``) overshoots on its own.
    The macOS driver's twin of this gate is
    ``test_driver_isolation.py::test_macos_launch_root_derives_a_bindable_sync_agent_socket``.
    """
    root = launch_tmp_root("darwin")
    if not os.path.isdir(root):
        # A box with no `/tmp` (windows) still measures the load-bearing half:
        # the SAME mkdtemp suffix, drawn for real in the box's own temp dir,
        # composed onto the macOS root. Only the symlink half below needs the
        # real root, and that is a macOS box property.
        real = tempfile.mkdtemp(prefix="fauna-e2e-tui-agent-")
        Path(real).rmdir()
        sock = PurePosixPath(root) / Path(real).name / "home" / _AGENT_SOCKET_RELPATH
        length = len(str(sock).encode())
        assert length < _SUN_PATH_MAX, (
            f"the default tui launch root {root!r} derives a {length}-byte agent "
            f"socket path, over the {_SUN_PATH_MAX - 1}-byte usable sun_path "
            f"budget:\n  {sock}\nThe real sync agent cannot bind this."
        )
        return
    probe = tempfile.mkdtemp(prefix="fauna-e2e-tui-agent-", dir=root)
    try:
        sock = Path(probe) / "home" / _AGENT_SOCKET_RELPATH
        length = len(str(sock).encode())
        assert length < _SUN_PATH_MAX, (
            f"the default tui launch root {root!r} derives a {length}-byte agent "
            f"socket path, over the {_SUN_PATH_MAX - 1}-byte usable sun_path "
            f"budget:\n  {sock}\nThe real sync agent cannot bind this."
        )
        resolved = len(
            str(Path(probe).resolve() / "home" / _AGENT_SOCKET_RELPATH).encode()
        )
        assert resolved < _SUN_PATH_MAX, (
            f"the symlink-resolved launch root derives {resolved} bytes, over budget"
        )
    finally:
        Path(probe).rmdir()


def test_build_launch_env_isolates_the_windows_sync_agent(tmp_path):
    """All three windows agent isolations are named, and the binary is PINNED.

    The windows twin of the macOS HOME relocation above, and needed for the same
    reason at a different seam. On unix the agent seam is path-derived, so an
    isolated XDG world (linux) or HOME (macOS) closes it. Windows rendezvouses on the machine-global
    ``\\\\.\\pipe\\fauna-sync.<SID>`` and defaults its state to ``%LOCALAPPDATA%``,
    so a launch that names none of these drives the developer's own installed
    agent (testing.md § point 10). The pin matters just as much: an unpinned
    miss falls *through* to that same installed agent instead of failing.
    """
    app = tmp_path / "bin" / "fauna-tui.exe"
    app.parent.mkdir(parents=True, exist_ok=True)
    app.write_bytes(b"")
    env = build_launch_env(
        {"app_path": str(app)}, 4321, str(tmp_path), sys_platform="win32"
    )
    assert env["FAUNA_E2E_SYNC_PIPE"] == "fauna-sync-e2e-tui-4321"
    assert env["FAUNA_E2E_SYNC_AGENT_DATA_DIR"] == str(tmp_path / "sync-agent")
    # The sibling `just tui-debug` builds beside the app — one builder, not two.
    assert env["FAUNA_E2E_SYNC_AGENT_BIN"] == str(app.parent / "fauna-sync-agent.exe")


def test_build_launch_env_lets_a_caller_own_the_windows_agent_isolation(tmp_path):
    """A caller-passed value wins, the same carve-out shape ``XDG_RUNTIME_DIR``
    has: a test that pre-spawns its own agent on a chosen pipe must be able to
    point the app at it rather than have the driver mint a second name."""
    env = build_launch_env(
        {
            "app_path": str(tmp_path / "fauna-tui.exe"),
            "environment": {
                "FAUNA_E2E_SYNC_PIPE": "caller-pipe",
                "FAUNA_E2E_SYNC_AGENT_BIN": r"C:\chosen\fauna-sync-agent.exe",
            },
        },
        5,
        str(tmp_path),
        sys_platform="win32",
    )
    assert env["FAUNA_E2E_SYNC_PIPE"] == "caller-pipe"
    assert env["FAUNA_E2E_SYNC_AGENT_BIN"] == r"C:\chosen\fauna-sync-agent.exe"
    # The un-named one still gets the per-launch default.
    assert env["FAUNA_E2E_SYNC_AGENT_DATA_DIR"] == str(tmp_path / "sync-agent")
    # Driven through the explicit `sys_platform` parameter, like the macOS
    # gates above: the windows branch is then covered on ANY machine, so the
    # one platform whose agent seam is machine-global never has its isolation
    # go unproven just because the run happened elsewhere (convention 7).


def test_build_launch_env_relocates_localappdata_on_windows(tmp_path):
    r"""The unified account-store root is NOT XDG-derived on windows.

    tui's flat base is `session::config_dir` → `xdg_app_config_dir`, isolated by
    the `XDG_CONFIG_HOME` relocation on every platform including windows. The
    account store is not: `account_scope::erase` passes
    `StoreRoot::platform().base()` as a SECOND root, and shared Rust's windows
    arm reads `LOCALAPPDATA` and consults no XDG variable. Un-relocated, a
    `--app tui` sign-out on windows ran `remove_dir_all` over every 64-hex actor
    dir in the developer's real ``%LOCALAPPDATA%\Fauna\sync``.

    Driven through the explicit ``sys_platform`` parameter like the gates above,
    so the windows branch is proven on any machine.
    """
    env = build_launch_env(
        {"app_path": str(tmp_path / "fauna-tui.exe")},
        4321,
        str(tmp_path),
        sys_platform="win32",
    )
    assert env["LOCALAPPDATA"] == str(tmp_path / "localappdata")
    assert (tmp_path / "localappdata").is_dir()
    real = os.environ.get("LOCALAPPDATA")
    if real:
        assert Path(env["LOCALAPPDATA"]).resolve() != Path(real).resolve()


def test_build_launch_env_localappdata_follows_a_pinned_xdg_base(tmp_path):
    """Derived from `xdg_base`, so `preserve_state_across_relaunch()` preserves it.

    A caller that pins `xdg_base` to keep the flat base across a relaunch must
    keep the account-store root too — a relaunch that came back with a fresh unified
    root would be a new device for half the state and the same one for the other
    half. A caller-passed `LOCALAPPDATA` still wins outright.
    """
    pinned = tmp_path / "pinned-xdg"
    env = build_launch_env(
        {"app_path": str(tmp_path / "fauna-tui.exe"), "xdg_base": str(pinned)},
        4321,
        str(tmp_path),
        sys_platform="win32",
    )
    assert env["LOCALAPPDATA"] == str(pinned / "localappdata")

    owned = build_launch_env(
        {
            "app_path": str(tmp_path / "fauna-tui.exe"),
            "environment": {"LOCALAPPDATA": str(tmp_path / "chosen")},
        },
        4321,
        str(tmp_path),
        sys_platform="win32",
    )
    assert owned["LOCALAPPDATA"] == str(tmp_path / "chosen")


def test_build_launch_env_leaves_localappdata_alone_off_windows(tmp_path):
    """Unix derives the store root from XDG, so there is nothing to relocate —
    and assigning a bogus `%LOCALAPPDATA%` on a box that has none would be noise
    in every launch env the suite prints on failure."""
    env = build_launch_env(
        {"app_path": str(tmp_path / "fauna-tui")}, 4321, str(tmp_path), sys_platform="linux"
    )
    assert env.get("LOCALAPPDATA") == os.environ.get("LOCALAPPDATA")


def test_build_launch_env_headless_store_mode(tmp_path):
    """``headless_store`` drives the SEALED backend: the e2e file backend is
    disabled (empty reads as unset in ``cred_file_dir``) and the test-only
    force var skips the keyring probe (apps/tui.md § Credential storage)."""
    env = build_launch_env({"headless_store": True}, 7, str(tmp_path))
    assert env["FAUNA_E2E_CREDENTIAL_DIR"] == ""
    assert env["FAUNA_E2E_FORCE_HEADLESS_STORE"] == "1"
    # The default mode is untouched: file backend on, no force var.
    env = build_launch_env({}, 8, str(tmp_path))
    assert env["FAUNA_E2E_CREDENTIAL_DIR"]
    assert "FAUNA_E2E_FORCE_HEADLESS_STORE" not in env
