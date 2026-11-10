"""A cross-checkout FFI cache entry never holds fewer slices than its key's shape.

Measured 2026-11-09 on macOS: the background tester's iOS verify run restored
a `full-2slice:test-helpers` cache entry (`apple-ffi: cache HIT …`) whose
`FaunaFFI.xcframework` held `macos-arm64` alone, and the iOS app's prebuild
refused its link (`has no 'ios-arm64-simulator' slice`). The populate step
copies whatever framework is staged at that moment, and a host-flavor build in
the populating checkout (`mac-debug`, `swift-test` — both reassemble the one
staging path host-only) had replaced it after the bindgen. Every checkout on
the box restoring that key failed the same way until the entry aged out.

Two guards, both pinned here by RUNNING the justfile's own lines, never a copy:

  (a) `_apple-ffi-build-impl` counts the staged framework's slice directories
      against `SLICE_TRIPLES` before it publishes, stamps or stashes anything;
      a short count discards the framework, the bindgen stamp, the fast-path
      stamp and the flavor marker, and fails the recipe saying why.
  (b) `_apple-ffi-flavor`'s cache restore counts the entry's slices against
      the shape (`2slice` → 2) and drops a short entry, so one already
      poisoned is rebuilt rather than restored.

tier_1: reads the justfile and runs two of its snippets in bash over a tmp tree.
"""

import os
import re
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"
# Git Bash on Windows (no `/bin/bash` there); the system bash elsewhere.
_BASH = "bash" if os.name == "nt" else "/bin/bash"


def _snippet(start: str, end_marker: str) -> str:
    """The justfile's lines from the one starting with `start` through the first
    line after it that is exactly `end_marker` (indentation ignored), dedented."""
    lines = _JUSTFILE.read_text(encoding="utf-8").splitlines()
    first = [i for i, line in enumerate(lines) if line.strip().startswith(start)]
    assert len(first) == 1, f"expected one justfile line starting {start!r}, found {len(first)}"
    i = first[0]
    out = []
    for line in lines[i:]:
        out.append(line)
        if line.strip() == end_marker and len(out) > 1:
            break
    else:
        raise AssertionError(f"no {end_marker!r} after {start!r}")
    indent = len(out[0]) - len(out[0].lstrip())
    return "\n".join(line[indent:] for line in out)


def _make_framework(root: Path, slices: list[str]) -> Path:
    xcf = root / "FaunaFFI.xcframework"
    xcf.mkdir(parents=True)
    (xcf / "Info.plist").write_text("plist", encoding="utf-8")
    for s in slices:
        (xcf / s).mkdir()
    return xcf


def _bash(script: str, cwd: Path) -> subprocess.CompletedProcess:
    # Under the recipes' own shell options: both run `set -euo pipefail`, and the
    # first cut of these guards passed a plain-bash test while failing every cache
    # MISS silently there (a `find` of an absent entry inside `$(…)`, 2026-11-09).
    return subprocess.run([_BASH, "-c", "set -euo pipefail\n" + script],
                          cwd=cwd, capture_output=True, text=True)


def test_the_build_refuses_to_publish_a_framework_another_build_replaced(tmp_path: Path) -> None:
    guard = _snippet("STAGED_SLICES=", "fi")
    apple = tmp_path / "apps" / "fauna-apple"
    xcf = _make_framework(apple, ["macos-arm64"])
    (apple / ".ffi-cache.stamp").write_text("key", encoding="utf-8")
    (apple / ".ffi-flavor").write_text("full-2slice:test-helpers", encoding="utf-8")
    stamp = tmp_path / "bindgen.stamp"
    stamp.write_text("", encoding="utf-8")
    script = (
        "SLICE_TRIPLES=(aarch64-apple-ios-sim aarch64-apple-darwin)\n"
        f"BINDGEN_STAMP={stamp.as_posix()}\n" + guard + "\necho REACHED-POPULATE\n"
    )
    proc = _bash(script, tmp_path)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    assert "REACHED-POPULATE" not in proc.stdout
    assert "holds 1 slice(s) where 2 were built" in proc.stderr
    assert not xcf.exists() and not stamp.exists()
    assert not (apple / ".ffi-cache.stamp").exists() and not (apple / ".ffi-flavor").exists()


def test_the_build_publishes_a_framework_holding_every_built_slice(tmp_path: Path) -> None:
    guard = _snippet("STAGED_SLICES=", "fi")
    apple = tmp_path / "apps" / "fauna-apple"
    xcf = _make_framework(apple, ["ios-arm64-simulator", "macos-arm64"])
    script = (
        "SLICE_TRIPLES=(aarch64-apple-ios-sim aarch64-apple-darwin)\n"
        "BINDGEN_STAMP=unused\n" + guard + "\necho REACHED-POPULATE\n"
    )
    proc = _bash(script, tmp_path)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "REACHED-POPULATE" in proc.stdout
    assert xcf.is_dir()


def test_the_build_fails_saying_why_when_the_staged_framework_is_gone(tmp_path: Path) -> None:
    guard = _snippet("STAGED_SLICES=", "fi")
    (tmp_path / "apps" / "fauna-apple").mkdir(parents=True)
    script = ("SLICE_TRIPLES=(aarch64-apple-ios-sim aarch64-apple-darwin)\n"
              f"BINDGEN_STAMP={(tmp_path / 'b.stamp').as_posix()}\n" + guard + "\necho REACHED-POPULATE\n")
    proc = _bash(script, tmp_path)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    assert "holds 0 slice(s) where 2 were built" in proc.stderr


def test_a_cache_miss_falls_through_to_the_build(tmp_path: Path) -> None:
    """No entry at the key: the count reads 0, nothing is dropped, and the
    recipe goes on to its build — it does not die inside the count."""
    count = _snippet("HIT_SLICES=", "fi")
    hit = tmp_path / "cache" / "KEY"  # never created
    script = (f"SHAPE=2slice\nHIT={hit.as_posix()}\nCACHE_KEY=KEY\n" + count
              + '\nif [ -d "$HIT/FaunaFFI.xcframework" ]; then echo RESTORE; else echo REBUILD; fi\n')
    proc = _bash(script, tmp_path)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "REBUILD" in proc.stdout
    assert "poisoned" not in proc.stdout


@pytest.mark.parametrize(
    ("shape", "slices", "restored"),
    [
        ("2slice", ["macos-arm64"], False),
        ("2slice", ["ios-arm64-simulator", "macos-arm64"], True),
        ("3slice", ["ios-arm64-simulator", "macos-arm64"], False),
        ("3slice", ["ios-arm64", "ios-arm64-simulator", "macos-arm64"], True),
    ],
)
def test_the_restore_drops_a_cache_entry_short_of_its_shape(
    tmp_path: Path, shape: str, slices: list[str], restored: bool
) -> None:
    count = _snippet("HIT_SLICES=", "fi")
    hit = tmp_path / "cache" / "KEY"
    _make_framework(hit, slices)
    # The restore's own condition line, with its subshell body replaced by a marker.
    lines = _JUSTFILE.read_text(encoding="utf-8").splitlines()
    cond = [line.strip() for line in lines if line.strip().startswith('if [ -d "$HIT/FaunaFFI.xcframework" ] && [ "$HIT_SLICES" = ')]
    assert len(cond) == 1, cond
    test_expr = re.sub(r"\s*&&\s*\($", "", cond[0])
    script = (
        f"SHAPE={shape}\nHIT={hit.as_posix()}\nCACHE_KEY=KEY\n" + count + "\n"
        + test_expr + "; then echo RESTORE; else echo REBUILD; fi\n"
    )
    proc = _bash(script, tmp_path)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert ("RESTORE" if restored else "REBUILD") in proc.stdout, proc.stdout
    assert ("dropping the poisoned entry" in proc.stdout) is (not restored), proc.stdout
