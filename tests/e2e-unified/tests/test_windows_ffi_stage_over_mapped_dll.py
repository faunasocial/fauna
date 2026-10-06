"""tier_1: staging a rebuilt fauna_ffi.dll must succeed while the old one is mapped.

The failure:
a `pytest --app windows` run on a tree whose FFI test flavor was stale errored
EVERY windows test at setup. Pytest imports every test module during collection,
a dozen of them `import fauna_ffi` at module top, and on windows that maps
`target/debug/windows-ffi/<rid>/test-helpers/fauna_ffi.dll` into the pytest
process. The collection-time `app[windows]` prebuild (`just windows-debug` →
`windows-ffi-test dev` → `_windows-ffi-flavor`) then rebuilt the dll and tried to
`cp` it over the mapped file: `Device or resource busy`, the retry died the same
way, and the whole run was lost after the cold build had been paid. Windows
refuses to overwrite or delete a mapped image but does allow a RENAME, so the
staging step renames the old file aside and copies into the freed name
(`build-target-layout-windows.md` § Cargo target dir layout (win)).

The loader now builds the flavor before it maps it (`fauna_ffi._find_cdylib`),
so the prebuild of the same run finds the gate fresh and copies nothing — this
pins the other half: any process still holding an OLDER dll (a second pytest in
the same checkout, a bindgen run) must never be able to wedge a rebuild again.

Real state, no mocks: a real DLL is loaded by a real second process, the
precondition (a plain overwrite really is refused) is asserted first so the test
cannot pass vacuously, then the shipped staging script runs against it.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(sys.platform != "win32", reason="mapped-image locking is windows-only"),
]

_REPO = Path(__file__).resolve().parents[3]
_STAGE = _REPO / "scripts" / "win-stage-dll.sh"

_HOLDER = (
    "import ctypes, sys\n"
    "ctypes.CDLL(sys.argv[1])\n"
    "print('mapped', flush=True)\n"
    "sys.stdin.read()\n"
)


def _a_real_dll() -> Path:
    """Any loadable DLL will do; the interpreter's own stable-ABI one is always
    beside it and has no side effects on load."""
    for candidate in (Path(sys.base_prefix) / "python3.dll",
                      Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "version.dll"):
        if candidate.exists():
            return candidate
    pytest.fail("no loadable DLL found to map")


def _stage(src: Path, dst: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", _STAGE.as_posix(), src.as_posix(), dst.as_posix()],
        capture_output=True, text=True, timeout=60,
    )


def _assert_overwrite_refused(dst: Path) -> None:
    try:
        with open(dst, "r+b"):
            pass
    except PermissionError:
        return
    pytest.fail(f"precondition: {dst} should be un-overwritable while mapped")


def test_a_rebuilt_dll_stages_over_one_another_process_has_mapped(tmp_path):
    slot = tmp_path / "windows-ffi" / "win-arm64" / "test-helpers"
    slot.mkdir(parents=True)
    staged = slot / "fauna_ffi.dll"
    shutil.copyfile(_a_real_dll(), staged)
    rebuilt = tmp_path / "fauna_ffi.dll"
    rebuilt.write_bytes(b"the freshly built dll")

    from drivers.port_util import popen_group_kwargs, reap_descendants_of

    holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER, str(staged)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
        **popen_group_kwargs(),
    )
    reap_descendants_of(holder.pid)
    try:
        assert holder.stdout.readline().strip() == "mapped"
        _assert_overwrite_refused(staged)

        proc = _stage(rebuilt, staged)
        assert proc.returncode == 0, (
            "staging over a mapped dll must succeed (rename aside, then copy) — "
            f"a plain cp dies `Device or resource busy`:\n{proc.stdout}{proc.stderr}"
        )
        assert staged.read_bytes() == b"the freshly built dll"
        # The aside may already be gone — MSYS `rm` unlinks with POSIX
        # semantics, which windows allows on a mapped image — but the holder
        # must keep running on the image it mapped.
        assert holder.poll() is None, "the process holding the old dll must be unaffected"
    finally:
        holder.communicate(input="", timeout=30)


def test_the_next_stage_sweeps_asides_no_longer_mapped(tmp_path):
    staged = tmp_path / "fauna_ffi.dll"
    staged.write_bytes(b"old")
    rebuilt = tmp_path / "rebuilt.dll"
    rebuilt.write_bytes(b"new")
    (tmp_path / "fauna_ffi.dll.old.1234.5678").write_bytes(b"a leftover from a mapped pass")

    assert _stage(rebuilt, staged).returncode == 0
    rebuilt.write_bytes(b"newer")
    assert _stage(rebuilt, staged).returncode == 0

    assert staged.read_bytes() == b"newer"
    leftovers = sorted(p.name for p in tmp_path.glob("fauna_ffi.dll.old.*"))
    assert leftovers == [], (
        "an aside nothing maps — an earlier pass's, or this pass's own — must be "
        f"deleted, or every rebuild leaves a 150 MB file behind: {leftovers}"
    )
