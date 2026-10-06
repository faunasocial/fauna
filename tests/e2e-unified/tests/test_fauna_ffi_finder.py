"""tier_1: the win cdylib loader builds `_windows-ffi-flavor`'s test-helpers slot
through `just windows-ffi-test dev` on EVERY call, then loads exactly that slot.

Two regressions this replaces a pure disk scan to close:

* The scan named `windows-ffi/<flavor>/` without the RID segment the recipe
  added on 2026-08-24, so its private candidates never matched and every load
  fell through to the SHARED `target/<profile>/fauna_ffi.dll` any host build
  overwrites. The loader now names the one slot the recipe writes, RID included.
* The scan loaded whatever it found, never building and never checking
  staleness — at COLLECTION, since a dozen modules import `fauna_ffi` at top
  level. A stale dll silently ran old fixture code, and the run's own
  `app[windows]` prebuild then had to copy over the dll this process had mapped
  (`Device or resource busy`, every windows test errored). Routing through the build-if-stale-gated recipe first, the same way
  linux/mac route through `just e2e-ffi` (`test_fauna_ffi_rebuilds_via_just.py`),
  makes the mapped dll the fresh one and the later prebuild a no-op.

Only fauna_ffi's own `sys`/`platform`/`subprocess.run` names are swapped, never
the real modules, so the win branch is exercised on every machine.
"""

import os
import sys
import types

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import fauna_ffi  # noqa: E402

pytestmark = pytest.mark.tier_1


class _FakeProc:
    def __init__(self, returncode: int) -> None:
        self.returncode = returncode
        # The loader pipes the build's output to keep its tail on a failure.
        self.stdout = ""


def _slot(root, rid):
    return root / "target" / "debug" / "windows-ffi" / rid / "test-helpers" / "fauna_ffi.dll"


def _as_win(monkeypatch, root, machine, returncode=0, writes=True):
    calls = []

    def fake_run(cmd, cwd, check, **_pipe_kwargs):
        calls.append(cmd)
        if writes:
            rid = "win-arm64" if len(cmd) == 3 else "win-x64"
            lib = _slot(root, rid)
            lib.parent.mkdir(parents=True, exist_ok=True)
            lib.write_bytes(b"not a real dll")
        return _FakeProc(returncode)

    monkeypatch.setattr(fauna_ffi, "_find_repo_root", lambda: root)
    monkeypatch.setattr(
        fauna_ffi, "sys", types.SimpleNamespace(platform="win32", executable=sys.executable)
    )
    monkeypatch.setattr(fauna_ffi, "platform", types.SimpleNamespace(machine=lambda: machine))
    monkeypatch.setattr(fauna_ffi.subprocess, "run", fake_run)
    return calls


def test_every_call_builds_before_loading_even_with_a_populated_slot(tmp_path, monkeypatch):
    calls = _as_win(monkeypatch, tmp_path, "ARM64")
    first = fauna_ffi._find_cdylib()
    # The slot is populated now — a present-or-not check would skip this build.
    second = fauna_ffi._find_cdylib()
    assert calls == [["just", "windows-ffi-test", "dev"]] * 2, (
        "the build-if-stale gate decides staleness, never this loader: a "
        f"populated slot must not short-circuit the build: {calls!r}"
    )
    assert first == second == _slot(tmp_path, "win-arm64")


def test_the_loader_never_falls_back_to_the_shared_slot(tmp_path, monkeypatch):
    shared = tmp_path / "target" / "debug" / "fauna_ffi.dll"
    shared.parent.mkdir(parents=True)
    shared.write_bytes(b"whichever host build ran last")
    _as_win(monkeypatch, tmp_path, "ARM64", returncode=1, writes=False)
    with pytest.raises(RuntimeError, match="just windows-ffi-test dev"):
        fauna_ffi._find_cdylib()


def test_an_x64_host_builds_and_loads_its_own_rid(tmp_path, monkeypatch):
    calls = _as_win(monkeypatch, tmp_path, "AMD64")
    assert fauna_ffi._find_cdylib() == _slot(tmp_path, "win-x64")
    assert calls == [["just", "windows-ffi-test", "dev", "x86_64-pc-windows-msvc"]]


def test_a_successful_build_that_left_no_file_still_raises(tmp_path, monkeypatch):
    _as_win(monkeypatch, tmp_path, "ARM64", returncode=0, writes=False)
    with pytest.raises(RuntimeError, match="did not produce"):
        fauna_ffi._find_cdylib()
