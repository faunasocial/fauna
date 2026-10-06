#!/usr/bin/env -S uv run --quiet --no-project
"""Run a build command only if any source is newer than the oldest target.

Usage:
    build-if-stale.py --target T [--target T2 ...] \
                      --source S [--source S2 ...] \
                      [-q] -- <build-cmd...>

Each --target may be a file or a directory. For a directory, freshness is the
mtime of its newest file (which is what a fresh build produces). Each --source
may be a file or a directory (recursively scanned). --exclude takes glob
pattern(s) matched against each source file's path; matching files are ignored
(e.g. `--exclude '*/pkg/*'` to skip wasm-pack build outputs when watching a
whole crate tree).

Rebuild is triggered when:
  - any target is missing, OR
  - the newest source mtime is greater than the OLDEST target's freshness signal
    (so a single stale target file in a multi-target generator forces a rerun).

Exits 0 silently if up-to-date. Otherwise runs <cmd> and exits with its rc.

--stamp: freshness for commands whose outputs don't move on a no-op (cargo).
    A `cargo build` that concludes "nothing to do" leaves its artifact's mtime
    untouched, so keying freshness on the artifact goes PERMANENTLY stale the
    first time a source's mtime churns without a semantic change (a rebase does
    this to the whole tree) — and if the command is slot-wrapped, every such run
    queues for a machine-wide build slot to do nothing. With `--stamp PATH`:
      * the stamp is THE freshness signal (its mtime vs the newest source);
      * `--target` entries become EXISTENCE checks only (`cargo clean -p`
        removes the artifact but not the stamp — a missing target forces a run);
      * on success the stamp is committed with the instant the command STARTED
        as its mtime — the build-slot GRANT when the command is slot-wrapped
        (build-slot.py hands it back via FAUNA_SLOT_GRANT_FILE; see
        run_for_stamp), else the pre-run instant — so a source edited while the
        build ran stays newer than the stamp and the next run picks it up (a
        post-run mtime would silently absorb it), while one edited while the
        build only QUEUED was compiled by it and counts as fresh;
      * a failed command commits nothing.
    Spec: docs/goal/architecture/build-system.md § How the gate works →
    "Gating a cargo step".

--check: report freshness via exit code (0 fresh / 1 stale-or-missing) WITHOUT
    running <cmd>. `cmd` and the `--` separator become optional — a probe has
    nothing to run. Lets a caller decide "is there ANY work?" across several
    gates before acquiring a shared resource (a build slot, an out-dir mutex)
    that a fully warm invocation would otherwise queue for needlessly. Same
    --target/--source/--exclude/--stamp semantics as normal mode; -q silences
    the status line for either outcome (normal mode's -q only silences the
    up-to-date line, since the stale/missing lines there also announce that a
    build is about to run).
"""
from __future__ import annotations
import argparse
import fnmatch
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def _target_freshness(path: Path) -> float:
    """Mtime signal for a single target. Files: own mtime. Dirs: newest file inside."""
    if path.is_file():
        return path.stat().st_mtime
    if path.is_dir():
        mtimes = [p.stat().st_mtime for p in path.rglob("*") if p.is_file()]
        return max(mtimes) if mtimes else 0.0
    return 0.0


def _excluded(p: Path, excludes: list[str]) -> bool:
    s = p.as_posix()
    return any(fnmatch.fnmatch(s, pat) for pat in excludes)


def _source_newest(path: Path, excludes: list[str]) -> float:
    """Newest mtime across a source path. Files: own mtime. Dirs: newest file
    inside, skipping files whose path matches any --exclude glob."""
    if path.is_file():
        return path.stat().st_mtime
    if path.is_dir():
        return max(
            (
                p.stat().st_mtime
                for p in path.rglob("*")
                if p.is_file() and not _excluded(p, excludes)
            ),
            default=0.0,
        )
    raise SystemExit(f"build-if-stale: source not found: {path}")


#: build-slot.py writes the instant it granted the slot into the file this
#: variable names (build-slot.py's module docstring § Grant instant).
GRANT_FILE_ENV = "FAUNA_SLOT_GRANT_FILE"


def run_for_stamp(cmd: list[str], run=subprocess.call, **kwargs):
    """Run `cmd` and return `(run's result, freshness instant)` — the ONE place
    a cargo freshness stamp's instant is decided (this script's `--stamp` and
    tests/common/nest.py's builders share it).

    The instant is when the build STARTED reading its inputs: the build-slot
    grant when `cmd` is slot-wrapped, else the pre-run instant (with no slot
    to wait for the two coincide — a solo checkout, an unwrapped command, or
    a slot script too old to know the handoff). An input edited while the
    build queued was compiled by it, so dating the stamp at the request would
    send the next freshness check back into the `build` queue for nothing.
    `run` is subprocess.call or subprocess.run; `kwargs` pass through.
    """
    pre = time.time()
    with tempfile.TemporaryDirectory(prefix="fauna-grant-") as d:
        grant = Path(d) / "instant"
        env = dict(kwargs.pop("env", None) or os.environ)
        env[GRANT_FILE_ENV] = str(grant)
        result = run(cmd, env=env, **kwargs)
        try:
            granted = float(grant.read_text())
        except (OSError, ValueError):
            granted = pre
    return result, max(pre, granted)


def commit_stamp(stamp: Path, instant: float, text: str) -> None:
    """Write a freshness stamp whose mtime is `instant` (from run_for_stamp)."""
    stamp.parent.mkdir(parents=True, exist_ok=True)
    stamp.write_text(text)
    os.utime(stamp, (instant, instant))


def main() -> int:
    if "--" in sys.argv:
        sep = sys.argv.index("--")
        parser_args = sys.argv[1:sep]
        cmd = sys.argv[sep + 1:]
    else:
        parser_args = sys.argv[1:]
        cmd = []

    parser = argparse.ArgumentParser()
    parser.add_argument("--target", action="append", default=[])
    parser.add_argument("--source", action="append", default=[], required=True)
    parser.add_argument("--exclude", action="append", default=[],
                        help="Glob(s) matched against source file paths; matches ignored")
    parser.add_argument("-q", "--quiet", action="store_true",
                        help="Suppress the status line (normal mode: up-to-date only; "
                             "--check mode: either outcome)")
    parser.add_argument("--label", default=None,
                        help="Short name shown in log lines (defaults to first target)")
    parser.add_argument("--stamp", default=None,
                        help="Freshness stamp for no-op-friendly commands (cargo): the "
                             "stamp's mtime is the freshness signal, --target entries are "
                             "existence checks, and success commits the build's start "
                             "instant (the slot grant) "
                             "(see module docstring)")
    parser.add_argument("--check", action="store_true",
                        help="Report freshness via exit code without running <cmd> "
                             "(see module docstring)")
    args = parser.parse_args(parser_args)

    if not args.check and not cmd:
        print("build-if-stale: missing build command after --", file=sys.stderr)
        print(__doc__, file=sys.stderr)
        return 2
    if not args.target and not args.stamp:
        parser.error("at least one --target (or --stamp) is required")

    targets = [Path(t) for t in args.target]
    sources = [Path(s) for s in args.source]
    label = args.label or (args.target[0] if args.target else args.stamp)

    def run_and_commit() -> int:
        if args.stamp is None:
            return subprocess.call(cmd)
        rc, instant = run_for_stamp(cmd)
        if rc == 0:
            commit_stamp(Path(args.stamp), instant,
                         "build-if-stale freshness stamp; mtime = build start (slot grant)\n")
        return rc

    missing = [t for t in targets if not t.exists()]
    if args.stamp is not None and not Path(args.stamp).exists():
        missing.insert(0, Path(args.stamp))
    if missing:
        if args.check:
            if not args.quiet:
                print(f"[build-if-stale] {label}: missing {missing[0]} → stale", flush=True)
            return 1
        print(f"[build-if-stale] {label}: missing {missing[0]} → building", flush=True)
        return run_and_commit()

    if args.stamp is not None:
        # Stamp mode: the stamp alone carries freshness — an existing target
        # OLDER than the sources is the cargo-no-op signature, not staleness.
        target_freshness = Path(args.stamp).stat().st_mtime
    else:
        target_freshness = min(_target_freshness(t) for t in targets)
    source_newest = max(_source_newest(s, args.exclude) for s in sources)

    if source_newest > target_freshness:
        if args.check:
            if not args.quiet:
                print(f"[build-if-stale] {label}: stale", flush=True)
            return 1
        print(f"[build-if-stale] {label}: stale → building", flush=True)
        return run_and_commit()

    if not args.quiet:
        print(f"[build-if-stale] {label}: up-to-date", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
