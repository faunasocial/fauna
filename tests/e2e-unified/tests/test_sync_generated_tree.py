"""tier_1: `sync-generated-tree.py` upholds the generator/mtime contract.

The contract (build-system.md § How the gate works → Generators and the mtime
contract): a generator must never rewrite an unchanged file inside a tree some
`build-if-stale` gate watches as `--source`. `build-if-stale` compares mtimes,
not bytes, so a byte-identical rewrite silently marks every watching gate stale
— observed 2026-07-24 when `uniffi-bindgen-go`'s unconditional rewrite of
`libs/fauna-mail-go/` invalidated all nine `--source libs` wasm chunk gates and
the next web e2e run paid a ~10-minute wasm rebuild for zero semantic change.

`scripts/sync-generated-tree.py` is the directory-shaped implementation of the
contract (per-file generators compare content themselves): the generator stages
into a dir outside every watched tree, and the sync writes into the checked-in
destination only the files whose bytes differ. These tests pin the load-bearing
properties: identical files keep their mtimes, changed/new files land, dropped
files are removed, and `--keep` shields hand-written neighbors (go.mod).
"""

import os
import subprocess
import sys

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_SCRIPT = os.path.join(_REPO, "scripts", "sync-generated-tree.py")


def _run(*args, expect_rc=0):
    proc = subprocess.run(
        [sys.executable, _SCRIPT, *map(str, args)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == expect_rc, (
        f"rc={proc.returncode} (wanted {expect_rc})\n"
        f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    )
    return proc


def _write(root, rel, content):
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    return path


def test_identical_file_keeps_mtime(tmp_path):
    """The core mtime-contract property: a byte-identical file is left
    completely untouched, so gates watching the destination stay fresh."""
    staged = tmp_path / "staged"
    dest = tmp_path / "dest"
    _write(staged, "pkg/binding.go", "package pkg\n")
    out = _write(dest, "pkg/binding.go", "package pkg\n")
    # Backdate so any rewrite would be observable as an mtime bump.
    old = 1_000_000_000
    os.utime(out, (old, old))

    proc = _run(staged, dest)

    assert out.stat().st_mtime == old, "identical file was rewritten"
    assert "0 wrote, 1 unchanged, 0 removed" in proc.stdout


def test_changed_and_new_files_are_written(tmp_path):
    staged = tmp_path / "staged"
    dest = tmp_path / "dest"
    _write(staged, "a.go", "new content\n")
    _write(staged, "sub/b.go", "brand new\n")
    _write(dest, "a.go", "old content\n")

    proc = _run(staged, dest)

    assert (dest / "a.go").read_text() == "new content\n"
    assert (dest / "sub/b.go").read_text() == "brand new\n"
    assert "2 wrote, 0 unchanged, 0 removed" in proc.stdout


def test_dropped_file_is_removed(tmp_path):
    """A binding the generator no longer emits must not linger and keep
    satisfying an import."""
    staged = tmp_path / "staged"
    dest = tmp_path / "dest"
    _write(staged, "kept.go", "x\n")
    _write(dest, "kept.go", "x\n")
    _write(dest, "stale/dropped.go", "gone from the generator\n")

    proc = _run(staged, dest)

    assert not (dest / "stale/dropped.go").exists()
    assert "0 wrote, 1 unchanged, 1 removed" in proc.stdout


def test_keep_shields_hand_written_files(tmp_path):
    """`--keep` names non-generated neighbors (the go.mod skeleton) that must
    survive even though the generator does not emit them."""
    staged = tmp_path / "staged"
    dest = tmp_path / "dest"
    _write(staged, "binding.go", "x\n")
    _write(dest, "binding.go", "x\n")
    kept = _write(dest, "go.mod", "module example\n")
    _write(dest, "unkept.txt", "removed\n")

    _run(staged, dest, "--keep", "go.mod")

    assert kept.read_text() == "module example\n"
    assert not (dest / "unkept.txt").exists()


def test_missing_staged_dir_fails_loudly(tmp_path):
    """A generator that failed to populate staging must fail the recipe, not
    silently sync nothing (which would present as a fresh gate)."""
    proc = _run(tmp_path / "does-not-exist", tmp_path / "dest", expect_rc=1)
    assert "staged dir does not exist" in proc.stderr


def test_dest_created_when_missing(tmp_path):
    staged = tmp_path / "staged"
    _write(staged, "pkg/binding.go", "fresh\n")

    _run(staged, tmp_path / "dest")

    assert (tmp_path / "dest/pkg/binding.go").read_text() == "fresh\n"


def test_exec_bit_is_copied(tmp_path):
    staged = tmp_path / "staged"
    dest = tmp_path / "dest"
    tool = _write(staged, "tool.sh", "#!/bin/sh\n")
    tool.chmod(0o755)

    _run(staged, dest)

    assert os.access(dest / "tool.sh", os.X_OK)
