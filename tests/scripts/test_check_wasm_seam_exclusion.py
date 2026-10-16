"""Unit-pin `scripts/check-wasm-seam-exclusion.py`'s flavor diff.

wasm-bindgen numbers its closure-invoke shims by count
(`wasm_bindgen_<hash>___convert__closures_____invoke_______true__3_`), so a
test flavor holding one more closure than production declares a shim the
production flavor lacks. That shim is bindgen glue, not an export anyone wrote:
the checker must leave it out of the flavor diff, while still reporting a real
non-seam export that only the test flavor carries.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "check-wasm-seam-exclusion.py"

_PROD_DTS = """\
export function startOnboarding(a: string): void;
export interface InitOutput {
  readonly startOnboarding: (a: number, b: number) => void;
  readonly wasm_bindgen_e91729bb3487008___convert__closures_____invoke_______true__2_: (a: number) => void;
}
"""

_SHIM = "wasm_bindgen_e91729bb3487008___convert__closures_____invoke_______true__3_"


def _run(tmp_path: Path, test_extra: str) -> subprocess.CompletedProcess[str]:
    prod, test = tmp_path / "pkg", tmp_path / "pkg-test"
    prod.mkdir()
    test.mkdir()
    (prod / "x.d.ts").write_text(_PROD_DTS, encoding="utf-8")
    (prod / "x.js").write_text("export function startOnboarding(a) {}\n", encoding="utf-8")
    test_dts = _PROD_DTS.replace(
        "export interface InitOutput {",
        "export interface InitOutput {\n"
        "  readonly enableFakeThingForTest: () => void;\n" + test_extra,
    )
    (test / "x.d.ts").write_text(test_dts, encoding="utf-8")
    return subprocess.run(
        [sys.executable, str(_SCRIPT), "--prod-pkg", str(prod), "--test-pkg", str(test), "--stem", "x"],
        capture_output=True,
        text=True,
    )


def test_a_numbered_bindgen_closure_shim_is_not_a_misplaced_export(tmp_path: Path) -> None:
    r = _run(tmp_path, f"  readonly {_SHIM}: (a: number) => void;\n")
    assert r.returncode == 0, r.stderr
    assert _SHIM not in r.stdout


def test_a_real_test_only_non_seam_export_still_fails(tmp_path: Path) -> None:
    r = _run(tmp_path, "  readonly leakedHelper: (a: number) => void;\n")
    assert r.returncode == 1
    assert "leakedHelper" in r.stderr
