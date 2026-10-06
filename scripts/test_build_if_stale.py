"""Tests for scripts/build-if-stale.py.

Run with: pytest scripts/test_build_if_stale.py -v
"""
from __future__ import annotations
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest


_SCRIPT = Path(__file__).resolve().parent / "build-if-stale.py"


def _run(args: list[str], cmd: list[str], expect_rc: int = 0) -> subprocess.CompletedProcess:
    """Invoke build-if-stale and return the completed process."""
    full = [sys.executable, str(_SCRIPT), *args, "--", *cmd]
    result = subprocess.run(full, capture_output=True, text=True)
    assert result.returncode == expect_rc, (
        f"unexpected rc {result.returncode}: stderr={result.stderr} stdout={result.stdout}"
    )
    return result


def _touch_cmd(marker: Path) -> list[str]:
    """A build command that creates `marker`, with no shell in the way.

    This used to be `["sh", "-c", f"touch {marker}"]`, which is wrong wherever
    `tmp_path` is a Windows path: `sh` reads the backslashes as escapes, so
    `touch C:\\Users\\…\\ran` creates a single file literally named
    `C:UsersUser…ran` **in the current directory** — the repo root. Measured
    2026-09-07 on Windows: four of these tests failed their `marker.exists()`
    assertion and each left one such file untracked in the checkout, where a
    `git add -A` would have committed it.

    Quoting the path would fix the escaping and still leave the test needing a
    POSIX shell; running the interpreter that is already `sys.executable`
    removes both problems and the dependency.
    """
    return [sys.executable, "-c", f"open({str(marker)!r}, 'w').close()"]


def _touch(path: Path, mtime: float | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("x")
    if mtime is not None:
        os.utime(path, (mtime, mtime))


def test_missing_target_triggers_build(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    marker = tmp_path / "ran"
    _touch(src)

    _run(
        ["--target", str(tgt), "--source", str(src)],
        _touch_cmd(marker),
    )
    assert marker.exists(), "build cmd should have run"


def test_source_newer_triggers_build(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    marker = tmp_path / "ran"
    _touch(tgt, mtime=1000.0)
    _touch(src, mtime=2000.0)

    _run(
        ["--target", str(tgt), "--source", str(src)],
        _touch_cmd(marker),
    )
    assert marker.exists(), "build cmd should have run when source is newer"


def test_target_newer_skips_build(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    marker = tmp_path / "ran"
    _touch(src, mtime=1000.0)
    _touch(tgt, mtime=2000.0)

    _run(
        ["--target", str(tgt), "--source", str(src)],
        _touch_cmd(marker),
    )
    assert not marker.exists(), "build cmd should NOT have run when target is newer"


def test_multiple_targets_one_stale_triggers_build(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    fresh_tgt = tmp_path / "fresh.txt"
    stale_tgt = tmp_path / "stale.txt"
    marker = tmp_path / "ran"

    _touch(stale_tgt, mtime=1000.0)
    _touch(src, mtime=2000.0)
    _touch(fresh_tgt, mtime=3000.0)

    _run(
        ["--target", str(stale_tgt), "--target", str(fresh_tgt), "--source", str(src)],
        _touch_cmd(marker),
    )
    assert marker.exists(), "any stale target among multiple should trigger build"


def test_dir_target_uses_newest_file_inside(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt_dir = tmp_path / "build"
    marker = tmp_path / "ran"
    tgt_dir.mkdir()
    _touch(tgt_dir / "old.txt", mtime=1000.0)
    _touch(tgt_dir / "new.txt", mtime=3000.0)
    _touch(src, mtime=2000.0)

    _run(
        ["--target", str(tgt_dir), "--source", str(src)],
        _touch_cmd(marker),
    )
    assert not marker.exists(), (
        "dir target with newest file (3000) > source (2000) should be up-to-date"
    )


def test_dir_source_uses_newest_file_inside(tmp_path: Path) -> None:
    src_dir = tmp_path / "src"
    tgt = tmp_path / "tgt.txt"
    marker = tmp_path / "ran"
    src_dir.mkdir()
    _touch(src_dir / "a.txt", mtime=1000.0)
    _touch(src_dir / "b.txt", mtime=3000.0)
    _touch(tgt, mtime=2000.0)

    _run(
        ["--target", str(tgt), "--source", str(src_dir)],
        _touch_cmd(marker),
    )
    assert marker.exists(), (
        "dir source with newest file (3000) > target (2000) should rebuild"
    )


def test_missing_source_errors(tmp_path: Path) -> None:
    tgt = tmp_path / "tgt.txt"
    _touch(tgt)

    result = _run(
        ["--target", str(tgt), "--source", str(tmp_path / "does-not-exist")],
        ["echo", "should-not-run"],
        expect_rc=1,
    )
    assert "source not found" in result.stderr or "source not found" in result.stdout


def test_build_failure_propagates(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    _touch(src)

    _run(
        ["--target", str(tgt), "--source", str(src)],
        ["sh", "-c", "exit 7"],
        expect_rc=7,
    )


def test_quiet_suppresses_uptodate_message(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    _touch(src, mtime=1000.0)
    _touch(tgt, mtime=2000.0)

    result = _run(
        ["-q", "--target", str(tgt), "--source", str(src)],
        ["echo", "should-not-run"],
    )
    assert "up-to-date" not in result.stdout
    assert "up-to-date" not in result.stderr


def test_label_appears_in_output(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    tgt = tmp_path / "tgt.txt"
    _touch(src, mtime=1000.0)
    _touch(tgt, mtime=2000.0)

    result = _run(
        ["--label", "myrecipe", "--target", str(tgt), "--source", str(src)],
        ["echo", "noop"],
    )
    assert "myrecipe" in result.stdout
