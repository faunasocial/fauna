"""tier_1: releasing a cloud-files sync root a finished run left under a temp tree
(`helpers/windows_cfapi.py` `release_sync_roots`).

Why it exists (`testing.md` § Test temp-dir hygiene). A windows on-demand binding
makes the bound directory a cfapi sync root, and the agent keeps that
registration when its process stops — by design, so a service restart
re-connects (`file-sync.md` § Per-file sync-status display, *The registration
lifecycle is persistent*). An e2e session binds folders under pytest's
`tmp_path`, and its agent dies with the session's app; the roots stayed
registered, pytest could not remove their placeholders (`WinError -2145452027`,
0x801F0005), renamed each numbered dir to `garbage-*`, and every later run on the
box retried and warned. Forty such dirs stood on 2026-11-10.

The release is the product's own full-teardown order (`cfapi_host.rs`, the
`KeepUnlessMarked` arm): the provider gone first (here: its process, which the
filter reports as provider status DISCONNECTED or TERMINATED), then the filter
registration (`CfUnregisterSyncRoot`), then the shell registration by its
path-keyed Id. A root whose provider is still connected, or that another provider
owns, is never touched.

These tests stage real filter roots inside their own `tmp_path` through the
helper's provider calls and unregister whatever is left in `finally`, so a red
run leaves no root behind. The shell half is witnessed against the OS:
`Unregister` of an Id that does not exist answers ERROR_NOT_FOUND, which proves
the call reaches the sync-root manager through the right interface slot.
"""

from __future__ import annotations

import ctypes
import shutil
import subprocess
import sys
import uuid
from pathlib import Path

import pytest

from drivers.port_util import popen_group_kwargs, reap_descendants_of

pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(sys.platform != "win32", reason="the Windows cloud-files filter"),
]

if sys.platform == "win32":
    from helpers import windows_cfapi as cf

#: `ProviderStatus` values the filter reports (measured 2026-11-10).
_CONNECTED_IDLE = 0x1
_TERMINATED = 0xC0000001


def _cleanup(*roots: Path) -> None:
    """The test's own teardown: whatever it registered goes, pass or fail."""
    cld = ctypes.WinDLL("cldapi")
    for root in roots:
        cld.CfUnregisterSyncRoot(str(root))


@pytest.fixture
def run_tree(tmp_path):
    """A finished run's tree: ``<tmp>/run/<test dir>/bound`` holding one file,
    the shape an e2e test's on-demand binding leaves under its ``tmp_path``."""
    bound = tmp_path / "run" / "test_binds_a_folder0" / "bound"
    bound.mkdir(parents=True)
    (bound / "kept.txt").write_text("a file the binding held")
    return tmp_path / "run", bound


def test_a_disconnected_fauna_root_is_released_and_its_tree_then_removes(run_tree):
    """The class this reclaims: a root whose provider is gone. Found by the
    walk, unregistered, and the tree it sat in removes like any other."""
    run, bound = run_tree
    cf.register_sync_root(bound)
    try:
        assert cf.is_sync_root(bound), "the fixture's registration did not take"
        assert cf.find_sync_roots(run) == [bound]

        result = cf.release_sync_roots(run)

        assert result.released == [bound], result
        assert result.spared == [], result
        assert not cf.is_sync_root(bound), "the root is still registered after its release"
        shutil.rmtree(run)
        assert not run.exists()
    finally:
        _cleanup(bound)


def test_a_root_whose_provider_is_connected_is_spared_until_it_disconnects(run_tree):
    """A live agent's root is never taken from under it: connected → spared,
    named with its status; the same root released once its provider is gone."""
    run, bound = run_tree
    cf.register_sync_root(bound)
    key = None
    try:
        key = cf.connect_sync_root(bound)
        provider = cf.sync_root_provider(bound)
        assert provider is not None and provider.status == _CONNECTED_IDLE, provider

        result = cf.release_sync_roots(run)

        assert result.released == [], result
        assert [p for p, _ in result.spared] == [bound], result
        assert "connected" in result.spared[0][1], result
        assert cf.is_sync_root(bound), "a connected root was unregistered"

        cf.disconnect_sync_root(key)
        key = None
        assert cf.release_sync_roots(run).released == [bound]
        assert not cf.is_sync_root(bound)
    finally:
        if key is not None:
            cf.disconnect_sync_root(key)
        _cleanup(bound)


#: A provider that connects to the root named by argv[2] and ends its process
#: without disconnecting — what a killed agent does.
_CONNECT_AND_EXIT = """
import os, sys
sys.path.insert(0, sys.argv[1])
from pathlib import Path
from helpers.windows_cfapi import connect_sync_root
connect_sync_root(Path(sys.argv[2]))
print("connected", flush=True)
os._exit(0)
"""


def test_a_root_whose_provider_process_ended_while_connected_is_released(run_tree):
    """The session-end case: the app's tree kill takes its agent while the agent
    is connected, and the filter then reports TERMINATED (0xC0000001), not
    DISCONNECTED (measured 2026-11-10). No provider holds the root, so it is
    released like a cleanly disconnected one — five of the 2026-11-10 leftovers
    read TERMINATED and stayed unreleasable until this case was counted."""
    run, bound = run_tree
    cf.register_sync_root(bound)
    try:
        child = subprocess.Popen(
            [sys.executable, "-c", _CONNECT_AND_EXIT, str(Path(__file__).resolve().parents[1]), str(bound)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            **popen_group_kwargs(),
        )
        reap_descendants_of(child.pid)
        out, _ = child.communicate(timeout=60)
        assert child.returncode == 0 and "connected" in out, out
        provider = cf.sync_root_provider(bound)
        assert provider is not None and provider.status == _TERMINATED, provider

        result = cf.release_sync_roots(run)

        assert result.released == [bound], result
        assert not cf.is_sync_root(bound)
    finally:
        _cleanup(bound)


def test_a_root_another_provider_registered_is_never_touched(run_tree):
    """The release is Fauna's own: a root under a temp tree that names another
    provider is spared, whatever its status."""
    run, bound = run_tree
    cf.register_sync_root(bound, provider="Another provider")
    try:
        result = cf.release_sync_roots(run)

        assert result.released == [], result
        assert [p for p, _ in result.spared] == [bound], result
        assert "Another provider" in result.spared[0][1], result
        assert cf.is_sync_root(bound)
    finally:
        _cleanup(bound)


def test_the_walk_finds_nothing_in_a_tree_without_roots(tmp_path):
    """The common case — a run that bound nothing: no root, nothing released."""
    (tmp_path / "a" / "b").mkdir(parents=True)
    (tmp_path / "a" / "b" / "f.txt").write_text("x")

    assert cf.find_sync_roots(tmp_path) == []
    assert cf.release_sync_roots(tmp_path) == cf.Released([], [])


def test_the_shell_unregister_reaches_the_sync_root_manager():
    """The shell half calls `StorageProviderSyncRootManager.Unregister` through
    its statics interface. An Id that cannot exist answers ERROR_NOT_FOUND
    (0x80070490): the OS's own answer, so the interface and the method slot are
    the right ones — a wrong slot would not come back with that code."""
    missing = f"Fauna!S-1-0-0!no-such-root-{uuid.uuid4().hex}"
    with pytest.raises(OSError, match="0x80070490"):
        cf.unregister_shell_sync_root(missing)
