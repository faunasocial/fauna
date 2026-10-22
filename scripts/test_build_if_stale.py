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


# ── --content-key: a clean tree at the recorded HEAD tree ids is fresh ─────────
# build-system.md § How the gate works → "Gating a cargo step" → content key.


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout


def _content_repo(tmp_path: Path) -> Path:
    """A committed repo with one tracked source tree and a .gitignore'd file."""
    repo = tmp_path / "repo"
    (repo / "libs" / "a").mkdir(parents=True)
    _git(repo.parent, "init", "-q", str(repo))
    _git(repo, "config", "user.email", "test@example.invalid")
    _git(repo, "config", "user.name", "test")
    (repo / "libs" / "a" / "lib.rs").write_text("fn a() {}\n")
    (repo / ".gitignore").write_text("libs/a/gen.rs\nlibs/a/pkg/\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "init")
    return repo


def _run_in(repo: Path, args: list[str], cmd: list[str]) -> subprocess.CompletedProcess:
    full = [sys.executable, str(_SCRIPT), *args, "--", *cmd]
    result = subprocess.run(full, capture_output=True, text=True, cwd=repo)
    assert result.returncode == 0, result.stderr + result.stdout
    return result


def _content_args(stamp: str = "stamp") -> list[str]:
    return ["--content-key", "--stamp", stamp, "--source", "libs",
            "--exclude", "*/pkg/*"]


def _bump_all_mtimes(repo: Path) -> None:
    """What a rebase or a seed's touch does: every source newer, no byte changed."""
    later = time.time() + 100
    for p in (repo / "libs").rglob("*"):
        if p.is_file():
            os.utime(p, (later, later))


def test_content_key_keeps_a_touched_clean_tree_fresh(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert marker.exists()
    marker.unlink()
    _bump_all_mtimes(repo)
    result = _run_in(repo, _content_args(), _touch_cmd(marker))
    assert not marker.exists(), "a byte-identical, mtime-bumped clean tree rebuilt"
    assert "content key unchanged" in result.stdout


def test_content_key_without_the_flag_stays_mtime_only(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    _bump_all_mtimes(repo)
    _run_in(repo, ["--stamp", "stamp", "--source", "libs"], _touch_cmd(marker))
    assert marker.exists()


def test_content_key_rebuilds_on_a_dirty_tree(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    (repo / "libs" / "a" / "lib.rs").write_text("fn b() {}\n")
    os.utime(repo / "libs" / "a" / "lib.rs", (time.time() + 100,) * 2)
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert marker.exists(), "an uncommitted edit was served from the old stamp"


def test_content_key_rebuilds_on_an_untracked_file(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    _touch(repo / "libs" / "a" / "new.rs", mtime=time.time() + 100)
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert marker.exists()


def test_content_key_rebuilds_on_a_new_commit(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    (repo / "libs" / "a" / "lib.rs").write_text("fn c() {}\n")
    _git(repo, "commit", "-q", "-am", "edit")
    os.utime(repo / "libs" / "a" / "lib.rs", (time.time() + 100,) * 2)
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert marker.exists(), "a committed change matched the old content key"


def test_content_key_still_reads_ignored_files_by_mtime(tmp_path: Path) -> None:
    """A generated, git-ignored file under a source has no tree id; its mtime
    is still the only signal, so it alone can make the gate stale."""
    repo = _content_repo(tmp_path)
    gen = repo / "libs" / "a" / "gen.rs"
    _touch(gen, mtime=time.time() - 100)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    _touch(gen, mtime=time.time() + 100)
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert marker.exists(), "an edited ignored source was hidden by the content key"


def test_content_key_ignores_excluded_ignored_files(tmp_path: Path) -> None:
    repo = _content_repo(tmp_path)
    marker = repo / "ran"
    _run_in(repo, _content_args(), _touch_cmd(marker))
    marker.unlink()
    _bump_all_mtimes(repo)
    _touch(repo / "libs" / "a" / "pkg" / "out.wasm", mtime=time.time() + 100)
    _run_in(repo, _content_args(), _touch_cmd(marker))
    assert not marker.exists()


def test_content_key_falls_back_outside_a_repo(tmp_path: Path) -> None:
    src = tmp_path / "libs" / "x.rs"
    _touch(src, mtime=time.time() + 100)
    stamp = tmp_path / "stamp"
    _touch(stamp, mtime=time.time())
    marker = tmp_path / "ran"
    _run_in(tmp_path, ["--content-key", "--stamp", str(stamp), "--source", "libs"],
            _touch_cmd(marker))
    assert marker.exists()


def test_content_key_needs_a_stamp(tmp_path: Path) -> None:
    src = tmp_path / "s"
    _touch(src)
    full = [sys.executable, str(_SCRIPT), "--content-key", "--target", str(tmp_path / "t"),
            "--source", str(src), "--", "true"]
    result = subprocess.run(full, capture_output=True, text=True)
    assert result.returncode == 2 and "--content-key" in result.stderr
