"""tier_1: the linux driver's launch environment — pure, no app launched.

The linux twin of ``test_tui_driver_env.py``'s runtime-dir case: a
caller-passed ``XDG_RUNTIME_DIR`` owns the launch's runtime dir, so a second
linux seat can be pointed at the first one's — where the account's
conversations door sits beside the agent socket
(``test_cli_conversations_door.py``, ``conversations-oracle.md`` § The
custodian and the door).
"""

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from drivers.linux import LinuxBridgeDriver, build_launch_env  # noqa: E402

pytestmark = pytest.mark.tier_1


def test_build_launch_env_lets_a_caller_own_the_runtime_dir(tmp_path):
    """A caller-passed ``XDG_RUNTIME_DIR`` wins over the private default, and
    with nothing passed the private per-launch default still applies (the
    isolation property: never the box's real runtime dir)."""
    chosen = tmp_path / "caller-runtime"
    env = build_launch_env(
        {"environment": {"XDG_RUNTIME_DIR": str(chosen)}}, 2, str(tmp_path / "a")
    )
    assert env["XDG_RUNTIME_DIR"] == str(chosen)
    assert os.path.isdir(env["XDG_RUNTIME_DIR"])
    default = build_launch_env({}, 3, str(tmp_path / "b"))
    assert default["XDG_RUNTIME_DIR"] == str(tmp_path / "b" / "runtime")


def test_the_agent_socket_follows_the_runtime_dir_the_launch_used(tmp_path):
    """``sync_agent_socket`` reads back the runtime dir the launch was given —
    a caller-owned one included — never a re-derivation that could disagree."""
    driver = LinuxBridgeDriver.__new__(LinuxBridgeDriver)
    assert driver.sync_agent_socket is None
    driver._xdg_runtime = str(tmp_path / "caller-runtime")
    assert driver.sync_agent_socket == str(
        tmp_path / "caller-runtime" / "fauna" / "sync-agent.sock"
    )
