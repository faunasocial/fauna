"""Tests for ``docker/cargo-content-freshness.sh``, the image build's
content-keyed mtimes over the machine-wide shared cargo target mount
(``docs/goal/architecture/image-pipeline.md``, compile-cache gotcha 4).

**The failure class.** The ``rust-target-*`` cache mounts are shared by every
checkout that builds the image. Inside the image, cargo records no checksum
(``CARGO_UNSTABLE_CHECKSUM_FRESHNESS`` measured inert, 2026-11-11), so a crate is
judged by mtime alone. COPY keeps host mtimes, so a file whose content changed but
whose mtime is older than another checkout's fingerprint read "fresh". A
``fauna-client-caldav`` compile then ran against another tree's
``fauna-client-drafts`` and failed on a method the tree defines. A stale crate
that still compiles would have shipped the other checkout's code.

The tests pin the script's contract against cargo's actual rule: a unit is stale
when a source file's mtime is newer than the unit's fingerprint.
  - Content the mount has not seen gets an mtime newer than every fingerprint.
  - Content it has seen gets back exactly the mtime recorded for it, so the
    units built from it stay fresh, a cancelled build's included.
  - Content that returns after another checkout replaced it is new again.
The wiring test pins that both compile RUNs run the script before their first
cargo build.

No docker, no cargo, no network. Run with:
``pytest tests/docker/test_cargo_content_freshness.py -v``
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parent.parent.parent
_SCRIPT = _REPO / "docker" / "cargo-content-freshness.sh"
_DOCKERFILE = _REPO / "Dockerfile"

#: Stands in for a COPY'd host mtime older than every fingerprint.
_OLD = 1_000_000_000

_DRAFTS = Path("libs/fauna client drafts/src/sync.rs")  # a space, on purpose
_CALDAV = Path("libs/caldav/src/lib.rs")


def _run(src: Path, now: int) -> str:
    """One compile RUN's call, at clock ``now`` (the script's test clock)."""
    out = subprocess.run(
        ["sh", str(_SCRIPT), str(src / "target"), str(src)],
        capture_output=True, text=True, check=True,
        env={**os.environ, "FAUNA_FRESHNESS_NOW": str(now)},
    )
    return out.stdout


def _tree(tmp_path: Path) -> Path:
    src = tmp_path / "src"
    for rel, body in ((_DRAFTS, "pub fn a() {}\n"), (_CALDAV, "pub fn b() {}\n")):
        (src / rel).parent.mkdir(parents=True, exist_ok=True)
        (src / rel).write_text(body)
    (src / "target").mkdir()
    (src / "target" / "artifact").write_text("built\n")
    _copy_mtimes(src)
    return src


def _copy_mtimes(src: Path, when: int = _OLD) -> None:
    """What a fresh COPY does: every source file gets some host mtime."""
    for path in src.rglob("*"):
        if path.is_file() and "target" not in path.relative_to(src).parts:
            os.utime(path, (when, when))


def _mtime(src: Path, rel: Path) -> float:
    return (src / rel).stat().st_mtime


#: Successive build clocks; a unit compiled during the build at ``T`` has a
#: fingerprint at ``T + 1``.
T1, T2, T3 = 2_000_000_000, 2_000_000_100, 2_000_000_200


def test_the_first_run_stamps_every_file_new(tmp_path):
    src = _tree(tmp_path)
    out = _run(src, T1)
    assert _mtime(src, _DRAFTS) == T1
    assert _mtime(src, _CALDAV) == T1
    assert "2 of 2 files are new or changed" in out


def test_changed_content_is_newer_than_another_checkouts_fingerprint(tmp_path):
    src = _tree(tmp_path)
    _run(src, T1)
    fingerprint = T1 + 1  # another checkout's build, compiled after its run

    # This tree: drafts changed, its COPY'd mtime is old.
    (src / _DRAFTS).write_text("pub fn a() {}\npub fn save_on_leave() {}\n")
    _copy_mtimes(src)
    out = _run(src, T2)

    assert _mtime(src, _DRAFTS) > fingerprint, "changed content must read stale to cargo"
    assert _mtime(src, _CALDAV) < fingerprint, "unchanged content must read fresh to cargo"
    assert "1 of 2 files are new or changed" in out


def test_unchanged_content_gets_its_recorded_mtime_back_whatever_copy_left(tmp_path):
    src = _tree(tmp_path)
    _run(src, T1)

    _copy_mtimes(src, when=T3 + 10_000)  # a COPY mtime in the future
    _run(src, T2)
    assert _mtime(src, _CALDAV) == T1

    _copy_mtimes(src)  # and one long past
    out = _run(src, T3)
    assert _mtime(src, _CALDAV) == T1
    assert "0 of 2 files are new or changed" in out


def test_a_cancelled_build_keeps_the_units_it_finished(tmp_path):
    src = _tree(tmp_path)
    _run(src, T1)
    finished = T1 + 1  # drafts compiled, then the build was cancelled

    _copy_mtimes(src)  # the retry: the same tree, COPY'd afresh
    _run(src, T2)
    assert _mtime(src, _DRAFTS) < finished, "a finished unit must not be rebuilt on retry"


def test_content_that_returns_after_another_checkout_is_new_again(tmp_path):
    src = _tree(tmp_path)
    _run(src, T1)                                      # checkout A records drafts v1
    (src / _DRAFTS).write_text("pub fn other() {}\n")  # checkout B builds v2
    _copy_mtimes(src)
    _run(src, T2)
    b_built = T2 + 1

    (src / _DRAFTS).write_text("pub fn a() {}\n")      # A again, v1
    _copy_mtimes(src)
    _run(src, T3)
    assert _mtime(src, _DRAFTS) > b_built, "B's unit must read stale to A"


def test_the_target_dir_is_never_hashed_or_stamped(tmp_path):
    src = _tree(tmp_path)
    os.utime(src / "target" / "artifact", (_OLD, _OLD))
    _run(src, T1)
    assert (src / "target" / "artifact").stat().st_mtime == _OLD
    assert "artifact" not in (src / "target" / ".fauna-content-mtimes").read_text()


def _compile_runs() -> list[str]:
    """Every `RUN --network=none` instruction of the Dockerfile whose body runs a
    cargo build over a shared `rust-target-*` mount, as one string each."""
    text = _DOCKERFILE.read_text()
    runs = re.findall(r"^RUN --network=none \\\n(?:.*\\\n)*.*$", text, flags=re.M)
    return [r for r in runs if "rust-target-" in r and "cargo build" in r]


def test_every_compile_run_stamps_mtimes_before_its_first_cargo_build():
    runs = _compile_runs()
    assert len(runs) == 2, f"expected the rust-native and rust-wasm compile RUNs, found {len(runs)}"
    for run in runs:
        call = run.find("cargo-content-freshness.sh /src/target /src")
        assert call != -1 and call < run.find("cargo build"), run[:200]


def test_the_script_is_copied_into_both_cargo_stages():
    text = _DOCKERFILE.read_text()
    assert text.count(
        "COPY docker/cargo-content-freshness.sh /usr/local/bin/cargo-content-freshness.sh") == 2


def _wasm_pack_run() -> str:
    """The rust-wasm stage's wasm-pack RUN, as one string."""
    text = _DOCKERFILE.read_text()
    runs = [r for r in re.findall(r"^RUN (?:.*\\\n)*.*$", text, flags=re.M)
            if "wasm-pack build" in r]
    assert len(runs) == 1, runs
    return runs[0]


def test_the_wasm_pack_run_never_reads_the_shared_target_mount():
    """The mount's lock is held per RUN, so between the compile RUN and the
    wasm-pack RUN another checkout's build can take the mount and leave its own
    units there. They read fresh, nothing compiles, and wasm-pack would package
    that checkout's wasm. So the wasm-pack RUN mounts no shared target at all."""
    assert "rust-target-" not in _wasm_pack_run()


def test_the_wasm_compile_run_snapshots_its_target_for_the_wasm_pack_run():
    """The compile RUN copies its target into its own layer while it still holds
    the mount's lock, and the wasm-pack RUN puts that copy at the path cargo
    built it under, before its first wasm-pack build."""
    compile_run = next(r for r in _compile_runs() if "wasm32-unknown-unknown" in r)
    snap = compile_run.find("cp -a /src/target /wasm-target")
    assert snap > compile_run.rfind("cargo build"), compile_run[-300:]
    pack = _wasm_pack_run()
    link = pack.find("ln -s /wasm-target /src/target")
    assert link != -1 and link < pack.find("wasm-pack build"), pack[:400]
