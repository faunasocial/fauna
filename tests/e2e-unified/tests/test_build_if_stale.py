"""build-if-stale.py `--stamp` — the freshness mode that lets a CARGO step be
gated outside its build slot.

Why a stamp exists at all: wasm-pack rewrites its outputs on every run, so the
wasm gates can key freshness on the artifacts themselves. cargo does NOT — a
no-op `cargo build` leaves the .so untouched — so keying a gate on a cargo
artifact goes permanently stale the first time a source is touched without a
semantic change (rebase mtime churn being the everyday case): source mtime >
artifact mtime forever, and every run queues for a build slot to do nothing.
That unconditional queue is the e2e-fixture timeout inversion
(build-system.md § Build/e2e slot locks, the ⚠⚠ bullet): a healthy contended
queue outlasting the 900s per-test pytest-timeout fails a perfectly good test.

`--stamp` semantics under test:
  * The stamp is THE freshness signal (compared against the newest source);
    `--target` entries remain EXISTENCE checks only — outputs cargo may
    legitimately leave untouched on a no-op must not drive freshness.
  * The stamp's mtime is the PRE-run instant, committed only on success — a
    source edited while the build ran stays newer than the stamp, so the next
    run re-runs (the mid-build-edit hole the wasm gates already paid for:
    memory build-if-stale-mtime-gate-stale-on-edit-during-build).
  * A failed command commits nothing.

Spec: build-system.md § How the gate works → "Gating a cargo step".
"""

import os
import re
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_SCRIPT = str(Path(__file__).resolve().parents[3] / "scripts" / "build-if-stale.py")

# Mtime anchors well apart, set explicitly with os.utime — ordering is asserted
# state, never a sleep (testing.md § point 14).
_OLD = time.time() - 3600
_NEW = time.time() + 3600


def _touch(path: Path, mtime: float) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text("x")
    os.utime(path, (mtime, mtime))
    return path


def _run_gate(tmp_path, *, stamp, targets=(), sources, cmd):
    argv = [sys.executable, _SCRIPT, "--label", "utest", "--stamp", str(stamp)]
    for t in targets:
        argv += ["--target", str(t)]
    for s in sources:
        argv += ["--source", str(s)]
    argv += ["--", *cmd]
    return subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)


def _sentinel_cmd(tmp_path):
    """A command that proves it ran by creating a file."""
    ran = tmp_path / "ran.txt"
    return ran, [sys.executable, "-c", f"open({str(ran)!r}, 'w').write('ran')"]


def test_fresh_stamp_skips_the_command_entirely(tmp_path):
    """THE warm-tree pin: with a fresh stamp the wrapped command never runs —
    and since the justfile composes gate OUTSIDE {{slot_build}}, "never runs"
    means no build slot is ever queued for. A fully warm tree must not wait."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = _touch(tmp_path / "gate.stamp", _OLD + 60)
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert not ran.exists(), (
        "a fresh stamp must skip the wrapped command entirely — running it would "
        "queue for a build slot with nothing to do (the warm-tree inversion)"
    )
    assert "up-to-date" in result.stdout


def test_missing_stamp_runs_and_commits_on_success(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = tmp_path / "gate.stamp"
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert ran.exists(), "a missing stamp means never-built — the command must run"
    assert stamp.exists(), "a successful run must commit the stamp"
    # And the second run is a warm no-op.
    ran.unlink()
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert not ran.exists(), "the run just committed must leave the gate fresh"


def test_failed_command_commits_nothing(tmp_path):
    """rc passes through and the stamp is not written: a failed build must stay
    stale, or the next run would silently skip a broken step."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = tmp_path / "gate.stamp"
    result = _run_gate(
        tmp_path, stamp=stamp, sources=[src.parent],
        cmd=[sys.executable, "-c", "import sys; sys.exit(3)"],
    )
    assert result.returncode == 3, "the wrapped command's exit code must pass through"
    assert not stamp.exists(), "a failed run must not commit the stamp"


def test_missing_target_forces_a_run_despite_a_fresh_stamp(tmp_path):
    """The `cargo clean -p` hole: freshness says nothing if the artifact itself
    is gone. --target entries are existence checks."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = _touch(tmp_path / "gate.stamp", _OLD + 60)
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_gate(
        tmp_path, stamp=stamp, targets=[tmp_path / "libfoo.so"],
        sources=[src.parent], cmd=cmd,
    )
    assert result.returncode == 0, result.stderr
    assert ran.exists(), (
        "a missing --target must force a run even when the stamp is fresh "
        "(cargo clean -p removed the artifact; the stamp survived)"
    )


def test_target_mtime_never_drives_freshness_when_a_stamp_is_given(tmp_path):
    """The exact reason --stamp exists: a cargo no-op leaves its artifact's
    mtime OLDER than the sources forever. With a stamp, an old-but-present
    target must not re-stale the gate."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD + 30)
    lib = _touch(tmp_path / "libfoo.so", _OLD)  # older than the source, present
    stamp = _touch(tmp_path / "gate.stamp", _OLD + 60)
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_gate(tmp_path, stamp=stamp, targets=[lib], sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert not ran.exists(), (
        "an existing target OLDER than the sources must not re-stale a fresh stamp "
        "— that permanent-stale loop is what --stamp replaces (a cargo no-op never "
        "touches its artifact, so the artifact can never win an mtime race)"
    )


def test_source_edited_during_the_build_keeps_the_gate_stale(tmp_path):
    """With no slot grant to report (an unwrapped command), the stamp's mtime is
    the PRE-run instant: a source edit landing while the
    command runs is newer than the stamp, so the NEXT run re-runs and picks it
    up. Committing the post-run instant instead would silently absorb the edit."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = tmp_path / "gate.stamp"
    # The command simulates a mid-build edit: it rewrites the source file, so the
    # source's mtime lands AFTER the run began.
    cmd = [
        sys.executable, "-c",
        f"open({str(src)!r}, 'w').write('edited mid-build')",
    ]
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert stamp.exists()
    assert src.stat().st_mtime > stamp.stat().st_mtime, (
        "the stamp must carry the PRE-run instant, or a mid-build edit is absorbed"
    )
    ran, cmd2 = _sentinel_cmd(tmp_path)
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd2)
    assert result.returncode == 0, result.stderr
    assert ran.exists(), (
        "a source edited during the previous run must leave the gate stale — the "
        "next run is what picks the edit up"
    )


def _queued_build_cmd(src: Path, edit_after_grant: bool) -> list[str]:
    """A stand-in for `{{slot_build}} cargo …` on a busy box: the source is
    edited while the build waits for its slot, the slot script records the
    grant instant in $FAUNA_SLOT_GRANT_FILE (build-slot.py's contract), and
    optionally the source is edited again while "cargo" runs."""
    return [sys.executable, "-c", (
        "import os, time\n"
        f"src = {str(src)!r}\n"
        "now = time.time()\n"
        "os.utime(src, (now + 50, now + 50))\n"
        "open(os.environ['FAUNA_SLOT_GRANT_FILE'], 'w').write(repr(now + 100))\n"
        + ("os.utime(src, (now + 150, now + 150))\n" if edit_after_grant else "")
    )]


@pytest.mark.parametrize("edit_after_grant,fresh", [(False, True), (True, False)],
                         ids=["edit-while-queued-is-fresh", "edit-after-grant-is-stale"])
def test_stamp_is_dated_at_the_slot_grant(tmp_path, edit_after_grant, fresh):
    """When the command is slot-wrapped, the stamp carries the build-slot GRANT
    instant, not the request: an edit made while the build queued was compiled
    by it (fresh — a re-check takes no slot), while one made after the grant
    may have missed the compile (stale)."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = tmp_path / "gate.stamp"
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent],
                       cmd=_queued_build_cmd(src, edit_after_grant))
    assert result.returncode == 0, result.stderr
    assert stamp.exists()
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_gate(tmp_path, stamp=stamp, sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert ran.exists() != fresh, (
        "an edit made while the build QUEUED must count as compiled (fresh)"
        if fresh else
        "an edit made after the slot GRANT must leave the gate stale"
    )


def test_plain_target_mode_is_unchanged(tmp_path):
    """No --stamp → the historical semantics: target mtimes drive freshness.
    The nine wasm gates depend on this; --stamp must be additive."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    target = _touch(tmp_path / "out.wasm", _OLD + 60)
    ran, cmd = _sentinel_cmd(tmp_path)
    argv = [
        sys.executable, _SCRIPT, "--label", "utest",
        "--target", str(target), "--source", str(src.parent), "--", *cmd,
    ]
    result = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    assert not ran.exists(), "fresh target, no stamp: must skip (historical mode)"
    _touch(src, _NEW)
    result = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert result.returncode == 0, result.stderr
    assert ran.exists(), "stale target, no stamp: must run (historical mode)"


# ── The generate-then-validate hazard ────────────────
#
# Non-stamp mode is safe for a command whose ONLY effect is producing its
# targets: if it fails, either the target is absent (rebuild next time) or it is
# whole. It is NOT safe for a command that WRITES its targets and only then
# VALIDATES something — a bindgen that generates bindings and finishes with a
# seam witness. There, a failure leaves targets both present and newer than the
# sources, and in non-stamp mode the target IS the freshness signal, so the next
# run reports up-to-date and exits 0. The red disappears on the remedy a session
# reaches for first: run it again.
#
# Measured against `_android-ffi-flavor` on 2026-08-22 with a real seam
# violation in the tree. The two tests below are the mechanism, stated so the
# hazard cannot be reintroduced by "simplifying" a stamp away.


def _generate_then_fail_cmd(tmp_path, out: Path):
    """A bindgen-shaped command: write the target, THEN fail validation."""
    ran = tmp_path / "gen-count.txt"
    script = (
        f"import os, pathlib, sys\n"
        f"out = pathlib.Path({str(out)!r})\n"
        f"out.mkdir(parents=True, exist_ok=True)\n"
        f"(out / 'bindings.kt').write_text('generated')\n"
        f"c = pathlib.Path({str(ran)!r})\n"
        f"c.write_text(str(int(c.read_text()) + 1 if c.exists() else 1))\n"
        f"sys.exit(7)\n"  # the seam witness, running last, refuses
    )
    return ran, [sys.executable, "-c", script]


def test_non_stamp_mode_lets_a_failed_generate_then_validate_go_green_on_rerun(tmp_path):
    """THE HAZARD, pinned so it stays visible: this is why the FFI bindgen gates
    may not run in non-stamp mode. Not a bug in the script — non-stamp mode's
    contract is "the target is the freshness signal", and this command lies to
    it by writing targets before deciding it failed."""
    src = _touch(tmp_path / "libfauna_ffi.so", _OLD)
    out = tmp_path / "stage" / "uniffi"
    count, cmd = _generate_then_fail_cmd(tmp_path, out)
    argv = [
        sys.executable, _SCRIPT, "--label", "utest",
        "--target", str(out), "--source", str(src), "--", *cmd,
    ]

    first = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert first.returncode == 7, "run 1 must surface the witness's refusal"
    assert count.read_text() == "1"

    second = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert second.returncode == 0, (
        "documenting the hazard: with no stamp, run 2 reports up-to-date and "
        "exits 0 — the real red is gone and the violation is still staged"
    )
    assert count.read_text() == "1", "run 2 did not even invoke the command"


def test_stamp_mode_keeps_a_failed_generate_then_validate_red_on_rerun(tmp_path):
    """THE FIX (option (a)). The stamp commits only on rc == 0, so a
    witness failure leaves nothing fresh and the next run re-runs and fails
    again — no source change, no green."""
    src = _touch(tmp_path / "libfauna_ffi.so", _OLD)
    out = tmp_path / "stage" / "uniffi"
    stamp = tmp_path / "android-ffi-bindgen-debug.stamp"
    count, cmd = _generate_then_fail_cmd(tmp_path, out)
    argv = [
        sys.executable, _SCRIPT, "--label", "utest", "--stamp", str(stamp),
        "--target", str(out), "--source", str(src), "--", *cmd,
    ]

    first = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert first.returncode == 7, "run 1 must surface the witness's refusal"

    second = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert second.returncode == 7, (
        "a re-run with no source change must fail again — this is the whole "
        "point of row 48: a real red must not be curable by re-running"
    )
    assert count.read_text() == "2", "run 2 must actually re-invoke the command"
    assert not stamp.exists(), "no stamp may be committed while the command fails"


def test_stamp_mode_still_skips_a_warm_tree_once_the_witness_passes(tmp_path):
    """The other half of the success criteria: the fix must not cost the
    warm-tree skip the gate exists for. Once the command succeeds, an unchanged
    .so must skip the expensive bindgen run entirely."""
    src = _touch(tmp_path / "libfauna_ffi.so", _OLD)
    out = tmp_path / "stage" / "uniffi"
    stamp = tmp_path / "android-ffi-bindgen-debug.stamp"
    ran, cmd = _sentinel_cmd(tmp_path)
    argv = [
        sys.executable, _SCRIPT, "--label", "utest", "--stamp", str(stamp),
        "--target", str(out), "--source", str(src), "--", *cmd,
    ]

    out.mkdir(parents=True, exist_ok=True)  # the command's target, as bindgen leaves it
    first = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert first.returncode == 0, first.stderr
    assert ran.exists(), "run 1 must build"

    ran.unlink()
    second = subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)
    assert second.returncode == 0, second.stderr
    assert not ran.exists(), "run 2 on a warm tree must skip the bindgen"


# ── The bindgen-gate ratchets' shared machinery ───────────────────────────────
#
# Three platforms run the same generate-then-validate bindgen shape, and every
# one of their build-if-stale gates needs the same two properties: `--stamp` mode
# (or a witness failure goes green on the next run) and a stamp keyed by
# everything the LABEL is keyed by (or a flavor switch serves the wrong flavor's
# stamp, trading one false green for another).
#
# Each used to assert that by pinning its gate's whole `--label "…"` literal, and
# that is why all three are rewritten here. A label gains a discriminator
# whenever a new build axis appears, and a pinned literal then dies with a bare
# `ValueError: substring not found` — on the IMPROVEMENT, not on a regression,
# which teaches the next session to loosen the ratchet rather than trust it. The
# dist-profile work walked exactly that path in one day, adding `-$PROFILE` to
# android and to apple; windows is the same shape and
# had simply not been reached yet.
#
# So: enumerate EVERY build-if-stale gate carrying both a label and a stamp, read
# whatever discriminators each label carries today, and require each to survive
# into the stamp path — resolving the stamp through its directory variables,
# since several gates key indirectly (apple's host gates under `$ARTIFACTS`,
# windows under `$FFI_DIR`, whose own definitions carry the axes). Enumerating
# rather than anchoring on one occurrence is deliberate: `apple-ffi-host-` names
# TWO gates (the cargo one and the bindgen one), and a prefix anchor silently
# graded whichever came first. A new axis passes; a stamp that forgets one does
# not; and the failure names the axis.


def _justfile_text():
    return (Path(__file__).resolve().parents[3] / "justfile").read_text()


def _last_def_before(text, pos, var):
    """The nearest preceding `VAR="…"` assignment — shell semantics, and what
    keeps `$FFI_DIR` resolving to the windows recipe's rather than a later
    recipe's."""
    matches = list(re.finditer(rf'^\s*{re.escape(var)}="([^"]*)"', text[:pos], re.M))
    return matches[-1].group(1) if matches else None


def _resolve_chain(text, pos, path, depth=5):
    """Every successive expansion of a stamp path, using the justfile's own
    nearest preceding definitions: `["$BINDGEN_STAMP", "$FFI_DIR/…", "target/…
    $RID/…"]`.

    The CHAIN rather than the final string, because expansion both reveals and
    destroys: one step turns `$ARTIFACTS` into a path that names the profile, and
    one more turns `$PROFILE_DIR` into a literal. An axis counts as keyed if it
    is visible at ANY level, which is exactly the question being asked — does
    switching this axis move the stamp — and is robust to however many
    indirections a platform happens to use.
    """
    chain = [path]
    for _ in range(depth):
        expanded = None
        for m in re.finditer(r"\$\{?([A-Z][A-Z0-9_]*)\}?", path):
            value = _last_def_before(text, pos, m.group(1))
            if value is not None:
                expanded = path[: m.start()] + value + path[m.end():]
                break
        if expanded is None:
            break
        path = expanded
        chain.append(path)
    return chain


def _stamp_gates(text, label_prefix):
    """Every `build-if-stale.py --label … --stamp …` invocation whose label starts
    with `label_prefix`, as (label, stamp, expansion-chain)."""
    gates = []
    for m in re.finditer(r'--label "([^"]+)"((?:.|\n){0,600}?)--stamp "([^"]+)"', text):
        label, stamp = m.group(1), m.group(3)
        if label.startswith(label_prefix):
            gates.append((label, stamp, _resolve_chain(text, m.start(), stamp)))
    return gates


def _assert_gates_keyed_like_their_labels(text, label_prefix, expected):
    """`build-if-stale` never reads the label for freshness, so every axis a
    label distinguishes runs by must reach the stamp PATH — directly, or through
    a directory variable that carries it."""
    gates = _stamp_gates(text, label_prefix)
    assert len(gates) == expected, (
        f"expected {expected} build-if-stale gate(s) labelled {label_prefix}*, "
        f"found {len(gates)}: {[g[0] for g in gates]} — a gate that lost its "
        f"--stamp is exactly what this ratchet exists to catch, and a NEW gate "
        f"needs its own accounting here"
    )
    for label, stamp, chain in gates:
        missing = [
            key for key in re.findall(r"\$\{?[A-Z][A-Z0-9_]*", label)
            if not any(key in step for step in chain)
        ]
        assert not missing, (
            f"the gate labelled {label!r} is keyed by {', '.join(missing)} but "
            f"its stamp path is not (stamp={stamp!r}, expanding to {chain[1:]!r}) "
            f"— build-if-stale never reads the label for freshness, so a stamp "
            f"keyed on less than the label trades the witness-goes-green hazard "
            f"for a flavor-switch one: switching {missing[0]} serves the other "
            f"flavor's stamp"
        )


def test_the_android_bindgen_gate_uses_stamp_mode(tmp_path):
    """A ratchet, not a unit test: the mechanism above only protects anything
    while the real gate actually uses it. Reading the justfile is the cheapest
    way to keep row 48 fixed — the defect it closes is invisible in every green
    build, so nothing else would notice a revert.

    ⚠ apple and windows carried the SAME generate-then-validate shape: their
    gates share one pair of targets across flavors, so their stamp paths must
    be keyed by flavor or they trade this false green for a flavor-switch one.
    Both are covered by their own sibling ratchets below,
    and all three now share one rule — see the note above it.
    """
    _assert_gates_keyed_like_their_labels(_justfile_text(), "android-ffi-", 1)


def test_the_apple_bindgen_gates_use_stamp_mode(tmp_path):
    """A ratchet, not a unit test — the apple twin of the android one above. `_apple-ffi-bindgen` runs the
    same generate-then-validate shape (bindings written, THEN the seam witness
    runs), so every apple gate needs `--stamp` for the identical reason.

    ⚠ Unlike android, the apple gates share ONE pair of targets
    (`FaunaFFI.xcframework` / `FaunaFFISwift/Sources/FaunaFFI.swift`) across
    every flavor and distinguish runs only by `--label`, which build-if-stale
    never reads for freshness — a stamp keyed on less than the label's own
    discriminators would trade this false-green class for a flavor-switch one
    instead of fixing it.

    THREE gates carry the prefix, and enumerating them is the point: the full
    `apple-ffi` gate keys its stamp directly, while `apple-ffi-host-cargo` and
    `apple-ffi-host` both key indirectly through `$ARTIFACTS`, whose own
    definition carries the profile and features. Anchoring on one occurrence of
    `apple-ffi-host-` graded whichever of the two came first in the file.
    """
    _assert_gates_keyed_like_their_labels(_justfile_text(), "apple-ffi-", 3)


def test_the_windows_bindgen_gate_uses_stamp_mode(tmp_path):
    """A ratchet, not a unit test — the windows twin of the android/apple ones
    above. `_windows-ffi-bindgen`
    runs the same generate-then-validate shape (bindings + runtime .dll + fake
    bases written, THEN the `*ForTest` grep and `check-ffi-seam-diff.py` seam
    witnesses run), so it needs `--stamp` for the identical reason.

    ⚠ Like apple, the WinUI project consumes ONE fixed pair of target paths
    regardless of flavor (`Generated/uniffi/` + `runtimes/$RID/native/
    fauna_ffi.dll`), distinguishing runs only by `--label`. Windows keys
    indirectly twice over — the stamp is `$BINDGEN_STAMP`, defined under
    `$FFI_DIR`, which carries RID, profile and features — which is why the shared
    rule follows the whole expansion chain rather than one substitution.
    """
    _assert_gates_keyed_like_their_labels(_justfile_text(), "windows-ffi-", 2)


# ── --check: report freshness via exit code, never run <cmd> ──────────────────
#
# The precheck a fan-out wrapper (`[linux] wasm:` / `[linux] web-test:`) runs
# for each of its 8 chunks BEFORE acquiring any out-dir mutex or build slot
# (build-system.md § Build/e2e slot locks, same-class residue as the
# mail-bridge/prev-build fixes `--stamp` already closed). `cmd`/`--` become
# optional under `--check`: the probe never has anything to run.


def _run_check(tmp_path, *, targets=(), stamp=None, sources, cmd=None, quiet=True):
    argv = [sys.executable, _SCRIPT, "--label", "utest", "--check"]
    if quiet:
        argv.append("-q")
    for t in targets:
        argv += ["--target", str(t)]
    if stamp is not None:
        argv += ["--stamp", str(stamp)]
    for s in sources:
        argv += ["--source", str(s)]
    if cmd is not None:
        argv += ["--", *cmd]
    return subprocess.run(argv, capture_output=True, text=True, cwd=tmp_path)


def test_check_fresh_target_exits_zero_without_running_cmd(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    target = _touch(tmp_path / "out.wasm", _OLD + 60)
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_check(tmp_path, targets=[target], sources=[src.parent], cmd=cmd)
    assert result.returncode == 0, result.stderr
    assert not ran.exists(), "--check must never run <cmd>, fresh or not"


def test_check_stale_target_exits_one_without_running_cmd(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    target = _touch(tmp_path / "out.wasm", _OLD - 60)  # older than the source
    ran, cmd = _sentinel_cmd(tmp_path)
    result = _run_check(tmp_path, targets=[target], sources=[src.parent], cmd=cmd)
    assert result.returncode == 1, result.stderr
    assert not ran.exists(), "--check must never run <cmd>, even to report staleness"


def test_check_missing_target_exits_one(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    missing_target = tmp_path / "never-built.wasm"
    result = _run_check(tmp_path, targets=[missing_target], sources=[src.parent])
    assert result.returncode == 1, result.stderr


def test_check_cmd_and_separator_are_optional(tmp_path):
    """The exact fan-out precheck shape: no `--`, no cmd at all — just the
    freshness question."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    target = _touch(tmp_path / "out.wasm", _OLD + 60)
    result = _run_check(tmp_path, targets=[target], sources=[src.parent], cmd=None)
    assert result.returncode == 0, result.stderr


def test_check_respects_quiet_on_both_outcomes(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    fresh_target = _touch(tmp_path / "fresh.wasm", _OLD + 60)
    stale_target = _touch(tmp_path / "stale.wasm", _OLD - 60)
    fresh = _run_check(tmp_path, targets=[fresh_target], sources=[src.parent], quiet=True)
    stale = _run_check(tmp_path, targets=[stale_target], sources=[src.parent], quiet=True)
    assert fresh.stdout == "" and fresh.stderr == "", (fresh.stdout, fresh.stderr)
    assert stale.stdout == "" and stale.stderr == "", (stale.stdout, stale.stderr)


def test_check_with_fresh_stamp_exits_zero(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = _touch(tmp_path / "gate.stamp", _OLD + 60)
    result = _run_check(tmp_path, stamp=stamp, sources=[src.parent])
    assert result.returncode == 0, result.stderr


def test_check_with_missing_stamp_exits_one(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _OLD)
    stamp = tmp_path / "gate.stamp"  # never created
    result = _run_check(tmp_path, stamp=stamp, sources=[src.parent])
    assert result.returncode == 1, result.stderr


def test_check_with_stale_stamp_exits_one(tmp_path):
    src = _touch(tmp_path / "src" / "lib.rs", _NEW)
    stamp = _touch(tmp_path / "gate.stamp", _OLD)  # older than the source
    result = _run_check(tmp_path, stamp=stamp, sources=[src.parent])
    assert result.returncode == 1, result.stderr


def test_check_target_mtime_does_not_drive_staleness_under_stamp(tmp_path):
    """--check + --stamp must inherit the same "stamp is the freshness signal,
    target is existence-only" rule as normal --stamp mode — an old-but-present
    target (the cargo-no-op signature) must not read as stale."""
    src = _touch(tmp_path / "src" / "lib.rs", _OLD + 30)
    lib = _touch(tmp_path / "libfoo.so", _OLD)  # older than the source, present
    stamp = _touch(tmp_path / "gate.stamp", _OLD + 60)
    result = _run_check(tmp_path, targets=[lib], stamp=stamp, sources=[src.parent])
    assert result.returncode == 0, result.stderr
