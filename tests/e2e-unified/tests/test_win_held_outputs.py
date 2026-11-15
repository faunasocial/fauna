"""tier_1: a build output a running process holds is moved aside before the retry.

The failure (measured 2026-11-10 on Windows): a tui run started beside a windows
run from the same checkout. The windows run's sync agent ran from
`target\\debug\\fauna-sync-agent.exe` and its mail bridge kept
`target\\fauna_ffi.dll` mapped; the tui run's prebuilds relinked both, and
windows refuses to delete or overwrite a mapped image — `tui-debug` died twice
on cargo's ``failed to remove file`` and the mail bridge on cp's ``Device or
resource busy``, so every tui test errored at setup. Linux and macOS replace a
running binary silently. `helpers/win_held_outputs.py` gives win the same
outcome: the held file is renamed aside (allowed on a mapped image) and the
retry writes the freed name; the holder runs on.

Real state where it matters: the windows case maps a real DLL in a second
process and asserts the plain delete is refused first, so it cannot pass
vacuously.
"""

import inspect
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from helpers.win_held_outputs import held_paths, move_aside_held_outputs

pytestmark = [pytest.mark.tier_1]

_CARGO_LINE = (
    "    [out] error: failed to remove file "
    "`C:\\build\\checkout\\target\\debug\\fauna-sync-agent.exe`"
)
_CP_LINE = (
    "cp: cannot create regular file "
    "'C:/build/checkout/target/fauna_ffi.dll': Device or resource busy"
)


def test_both_signatures_name_their_file_once_each():
    lines = [_CARGO_LINE, "Caused by:", _CP_LINE, _CARGO_LINE, "Compiling fauna-tui"]
    assert held_paths(lines) == [
        "C:\\build\\checkout\\target\\debug\\fauna-sync-agent.exe",
        "C:/build/checkout/target/fauna_ffi.dll",
    ]


def test_a_compile_error_names_nothing():
    assert held_paths(["error[E0308]: mismatched types", "error: could not compile `x`"]) == []


def test_off_windows_it_touches_nothing(tmp_path):
    held = tmp_path / "target" / "debug" / "app.exe"
    held.parent.mkdir(parents=True)
    held.write_bytes(b"x")
    line = f"error: failed to remove file `{held}`"
    assert move_aside_held_outputs([line], [tmp_path / "target"], platform="linux") == []
    assert held.read_bytes() == b"x"


def test_a_file_outside_the_target_roots_is_never_moved(tmp_path):
    root = tmp_path / "target"
    root.mkdir()
    outside = tmp_path / "elsewhere.exe"
    outside.write_bytes(b"not ours")
    line = f"error: failed to remove file `{outside}`"
    assert move_aside_held_outputs([line], [root], platform="win32") == []
    assert outside.read_bytes() == b"not ours"


def test_the_build_retry_frees_held_outputs_between_its_attempts():
    """The helper is only worth anything where the retry calls it."""
    import conftest

    source = inspect.getsource(conftest._build_via_just)
    retry_at = source.index("retrying once before")
    call_at = source.index("move_aside_held_outputs(recent")
    assert retry_at < call_at, "the held outputs are freed after attempt 1 fails, before the retry"


_HOLDER = (
    "import ctypes, sys\n"
    "ctypes.CDLL(sys.argv[1])\n"
    "print('mapped', flush=True)\n"
    "sys.stdin.read()\n"
)


def _a_real_dll() -> Path:
    for candidate in (Path(sys.base_prefix) / "python3.dll",
                      Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "version.dll"):
        if candidate.exists():
            return candidate
    pytest.fail("no loadable DLL found to map")


@pytest.mark.skipif(sys.platform != "win32", reason="mapped-image locking is windows-only")
def test_a_mapped_output_is_moved_aside_and_its_name_freed(tmp_path):
    root = tmp_path / "target"
    held = root / "debug" / "fauna_ffi.dll"
    held.parent.mkdir(parents=True)
    shutil.copyfile(_a_real_dll(), held)

    from drivers.port_util import popen_group_kwargs, reap_descendants_of

    holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER, str(held)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
        **popen_group_kwargs(),
    )
    reap_descendants_of(holder.pid)
    try:
        assert holder.stdout.readline().strip() == "mapped"
        with pytest.raises(PermissionError):
            os.remove(held)  # precondition: what cargo's uplift does, refused

        line = f"    [out] error: failed to remove file `{held}`"
        assert move_aside_held_outputs([line], [root]) == [held.resolve()]
        assert not held.exists(), "the held name must be free for the retry"
        held.write_bytes(b"the retry's output")
        assert held.read_bytes() == b"the retry's output"
        assert list(held.parent.glob("fauna_ffi.dll.old.*")), (
            "the mapped image lives on under its aside name"
        )
        assert holder.poll() is None, "the process holding the old image must be unaffected"
    finally:
        holder.communicate(input="", timeout=30)
