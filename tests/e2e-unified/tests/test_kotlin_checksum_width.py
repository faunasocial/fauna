"""The Kotlin bindings' API-checksum reads take the native u16 at its own width
(`scripts/kotlin-checksum-width.py`).

uniffi 0.31.2's Kotlin generator declares every `*_checksum_*` function as
returning `Int` and compares it with the unsigned constant. The native function
returns a u16, and on aarch64 Linux (the host JVM of the android unit tests, and
arm64 Android devices) the upper half of the return register is not defined:
the build leaves a checksum over 32767 sign-extended, so the comparison fails
and every binding class fails its initializer. The rewrite restores the shape
the 0.31.0 generator wrote — `Short` return, `N.toShort()` comparison — which
JNA reads at 16 bits.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SCRIPT = _REPO / "scripts" / "kotlin-checksum-width.py"

_GENERATED = """\
internal interface IntegrityCheckingUniffiLib : Library {
    fun ffi_x_uniffi_contract_version(
): Int
    external fun uniffi_x_checksum_func_format_price(
): Int
external fun uniffi_x_checksum_method_machine_snapshot(
): Int
}
private fun uniffiCheckApiChecksums(lib: IntegrityCheckingUniffiLib) {
    if (lib.uniffi_x_checksum_func_format_price() != 63858) {
        throw RuntimeException("UniFFI API checksum mismatch")
    }
    if (lib.uniffi_x_checksum_method_machine_snapshot() != 1234) {
        throw RuntimeException("UniFFI API checksum mismatch")
    }
}
"""

_WANTED = """\
internal interface IntegrityCheckingUniffiLib : Library {
    fun ffi_x_uniffi_contract_version(
): Int
    external fun uniffi_x_checksum_func_format_price(
): Short
external fun uniffi_x_checksum_method_machine_snapshot(
): Short
}
private fun uniffiCheckApiChecksums(lib: IntegrityCheckingUniffiLib) {
    if (lib.uniffi_x_checksum_func_format_price() != 63858.toShort()) {
        throw RuntimeException("UniFFI API checksum mismatch")
    }
    if (lib.uniffi_x_checksum_method_machine_snapshot() != 1234.toShort()) {
        throw RuntimeException("UniFFI API checksum mismatch")
    }
}
"""


def _run(tree: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(_SCRIPT), str(tree)],
        capture_output=True,
        text=True,
        check=False,
    )


def test_the_checksum_reads_take_the_u16_at_its_own_width(tmp_path: Path) -> None:
    kt = tmp_path / "com" / "fauna" / "ffi" / "x.kt"
    kt.parent.mkdir(parents=True)
    kt.write_text(_GENERATED)
    done = _run(tmp_path)
    assert done.returncode == 0, done.stderr
    assert kt.read_text() == _WANTED
    # The contract-version read is a real u32 and stays an Int.
    assert "contract_version(\n): Int" in kt.read_text()


def test_a_rewritten_tree_is_left_as_it_is(tmp_path: Path) -> None:
    kt = tmp_path / "x.kt"
    kt.write_text(_WANTED)
    done = _run(tmp_path)
    assert done.returncode == 0, done.stderr
    assert kt.read_text() == _WANTED


def test_a_half_rewritten_file_is_refused(tmp_path: Path) -> None:
    # A declaration rewritten without its comparison (or the reverse) would
    # compare a Short with an Int: refuse rather than write a file that cannot
    # pass its own check.
    kt = tmp_path / "x.kt"
    kt.write_text(_GENERATED.replace("!= 1234)", "!= 1234.toShort())"))
    done = _run(tmp_path)
    assert done.returncode != 0
    assert "x.kt" in done.stderr
