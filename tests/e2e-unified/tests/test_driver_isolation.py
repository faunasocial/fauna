"""Verify drivers use no shared state for port discovery."""
import os
import tempfile
from pathlib import Path, PurePosixPath

import pytest

pytestmark = pytest.mark.tier_1


def test_ios_driver_has_no_shared_port_file():
    """The iOS driver should not reference a shared port file path."""
    ios_src = Path(__file__).parent.parent / "drivers" / "ios.py"
    text = ios_src.read_text()
    assert "/tmp/apple-bridge-port.txt" not in text
    assert "_PORT_FILE" not in text


def test_macos_driver_has_no_shared_port_file():
    """The macOS driver should not reference a shared port file path."""
    macos_src = Path(__file__).parent.parent / "drivers" / "macos.py"
    text = macos_src.read_text()
    assert "/tmp/apple-bridge-port.txt" not in text
    assert "_PORT_FILE" not in text


def test_drivers_use_per_session_log_files():
    """Both drivers should use temp files for port discovery, not shared paths.

    The port env var is `FAUNA_E2E_AGENT_PORT` — the name every app's driver
    uses since the apple drivers moved onto the in-process agent (
    2026-06-13). This guard still asserted the retired `APPLE_BRIDGE_PORT` and so
    had been failing on `main` ever since; the drivers are the contract, the test
    was the drift.
    """
    for name in ("ios", "macos"):
        src = Path(__file__).parent.parent / "drivers" / f"{name}.py"
        text = src.read_text()
        assert "tempfile" in text, f"{name} driver doesn't use per-session temp files"
        assert "FAUNA_E2E_AGENT_PORT" in text, (
            f"{name} driver doesn't parse FAUNA_E2E_AGENT_PORT"
        )


def test_macos_launch_root_derives_a_bindable_sync_agent_socket():
    """The default macOS launch HOME must fit macOS's 104-byte ``sun_path``.

    Isolation and bindability pull against each other here: the sync agent's
    socket is derived from the launch HOME plus a fixed 50-byte
    ``Library/Application Support/Fauna/sync-agent.sock`` suffix, so relocating
    HOME for isolation (testing.md point 10) can push the socket past the
    kernel's limit. macOS's default ``TMPDIR`` — ``/var/folders/<28 opaque
    chars>/T/`` — does exactly that, and the failure is silent in the worst way:
    the agent spawns, dies at bind with the bare ``path must be shorter than
    SUN_LEN``, and the app reports only ``agent unreachable``. That is what
    stopped the macOS seat of the tri-machine live filesync run on 2026-07-24.

    Asserted against a REAL ``mkdtemp`` under the driver's own root, so the
    opaque-suffix length is measured rather than assumed. Shared Rust rejects an
    over-long path by name too (``fauna_ipc::unix_transport::
    check_socket_path_len``); this is the harness-side half.
    """
    from drivers.macos import (
        _AGENT_SOCKET_RELPATH,
        _LAUNCH_TMP_ROOT,
        _SUN_PATH_MAX,
    )

    if not os.path.isdir(_LAUNCH_TMP_ROOT):
        # A box with no `/tmp` (windows) still measures the load-bearing half:
        # the SAME mkdtemp suffix, drawn for real in the box's own temp dir,
        # composed onto the macOS root the driver will use. Only the symlink
        # half below needs the real root, and that is a macOS box property.
        real = tempfile.mkdtemp(prefix="fauna-e2e-macos-agent-")
        Path(real).rmdir()
        sock = PurePosixPath(_LAUNCH_TMP_ROOT) / Path(real).name / "home" / _AGENT_SOCKET_RELPATH
        length = len(str(sock).encode())
        assert length < _SUN_PATH_MAX, (
            f"the default macOS launch root {_LAUNCH_TMP_ROOT!r} derives a "
            f"{length}-byte agent socket path, over the {_SUN_PATH_MAX - 1}-byte "
            f"usable sun_path budget:\n  {sock}\n"
            "The real sync agent cannot bind this; pick a shorter launch root."
        )
        return

    probe = tempfile.mkdtemp(prefix="fauna-e2e-macos-agent-", dir=_LAUNCH_TMP_ROOT)
    try:
        sock = Path(probe) / "home" / _AGENT_SOCKET_RELPATH
        length = len(str(sock).encode())
        assert length < _SUN_PATH_MAX, (
            f"the default macOS launch root {_LAUNCH_TMP_ROOT!r} derives a "
            f"{length}-byte agent socket path, over the {_SUN_PATH_MAX - 1}-byte "
            f"usable sun_path budget:\n  {sock}\n"
            "The real sync agent cannot bind this; pick a shorter launch root."
        )
        # Also hold when something resolves the /tmp -> private/tmp symlink.
        resolved = len(str(Path(probe).resolve() / "home" / _AGENT_SOCKET_RELPATH).encode())
        assert resolved < _SUN_PATH_MAX, (
            f"the symlink-resolved launch root derives {resolved} bytes, over budget"
        )
    finally:
        Path(probe).rmdir()
