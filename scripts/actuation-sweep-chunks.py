#!/usr/bin/env python
"""Run a permissive actuation sweep in RESUMABLE CHUNKS, appending to ONE
marker log.

WHY THIS EXISTS. Convention 11's enumeration step wants one permissive sweep
over every test that can drive a control. On a shared dev box a whole-suite
sweep is a many-hour monolith, and a monolith that dies loses everything: the
windows leg lost three in a row (2026-09-10 x1, 2026-09-11 x2), each killed
mid-prebuild with no verdict -- and, measured 2026-09-11, with NO Windows crash
record in either death window (System + Application logs carry no Application
Error, no WER, no Resource-Exhaustion-Detector), so they were terminated rather
than crashed and no amount of build-hygiene would have saved them. linux lost
two six-hour sweeps the same way, to its probe rather than to a kill.

Chunking changes only the topology, never the enumeration: every chunk appends
to the SAME `--actuation-log`, so the union of chunks is the sweep. A kill costs
the in-flight chunk, and `--start-at` resumes from the next one.

WHY ONLY tier_2/3/4. An actuation marker is emitted by the agent or bridge that
actuates -- reachable ONLY through a driver. A tier_1 test is in-process by
definition (no driver, no external process), so it cannot emit one. Because
marker enforcement is strict (collection fails on any selected test lacking a
`tier_N` marker) the four tiers PARTITION the selection exactly, which makes
"tier_2+ is the complete set of tests that can enumerate" an exact claim rather
than a hopeful one -- verify it for your app with `--plan-only`, which prints
the tier census and asserts the partition. On the 2026-09-11 windows selection
that was 2672 + 128 + 802 + 20 == 3622, so the skipped tier_1s removed 74% of
the run and zero possible markers.

Chunks keep MODULE boundaries intact: the `app` fixture relaunches every capable
app at each test-module boundary (convention 10's per-module cold relaunch), so
a module is already the harness's own unit of isolation.

The known-positive control runs FIRST and ALONE (convention 11: a sweep whose
probe never ran cannot be told apart from a clean one), which also warms the
nest and app builds every later chunk reuses.

Each chunk's pytest acquires the machine-wide `e2e` slot itself, so do NOT wrap
this in `build-slot.py` -- that is the cross-pool order violation build-slot.py
refuses.
"""

import argparse
import datetime as _dt
import json
import os
import subprocess
import sys

_REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_E2E = os.path.join(_REPO, "tests", "e2e-unified")

#: Roughly how many tests go in one tier_3 chunk. Sized so a chunk is tens of
#: minutes, not hours: small enough that losing one is cheap, large enough that
#: per-chunk collection + prebuild + e2e-slot overhead stays amortised.
TIER3_CHUNK_TESTS = 110

#: The known-positive control per app -- the test that drives a disabled control
#: on purpose. Its marker MUST appear in the log or the sweep is not an
#: enumeration (convention 11, signal 2).
_PROBE = {
    "windows": "tests/test_windows_disabled_actuation.py",
    "linux": "tests/test_linux_disabled_actuation.py",
}


def _tree_sha():
    """The checkout's HEAD at the moment this chunk finished.

    Recorded per chunk because a sweep is only an enumeration of ONE tree. A
    rebase landed while the sweep is still running -- easy to do without
    noticing, since a long sweep spans hours and integrating often is the
    habit -- silently splits the run across two trees, so a module swept BEFORE
    the rebase was never measured against the code that shipped. The report
    turns this into signal 3 rather than leaving it to whoever remembers to
    mention it in a handoff note.
    """
    try:
        return subprocess.run(
            ["git", "-C", _REPO, "rev-parse", "HEAD"],
            capture_output=True, text=True,
        ).stdout.strip() or None
    except Exception:
        return None


def _collect(app, tier):
    """Module -> selected-test count, for one tier of one app's selection."""
    out = subprocess.run(
        [sys.executable, "-m", "pytest", "tests/", "--app", app,
         "--permissive-actuation", "--tier", str(tier), "--collect-only", "-q"],
        cwd=_E2E, capture_output=True, text=True, errors="replace",
    ).stdout
    counts = {}
    for line in out.splitlines():
        if "::" not in line:
            continue
        mod = line.split("::", 1)[0].strip()
        if mod.endswith(".py"):
            counts[mod] = counts.get(mod, 0) + 1
    return counts


def build_chunks(app):
    """The ordered chunk list. Deterministic, so `--start-at` is stable."""
    t2, t3, t4 = _collect(app, 2), _collect(app, 3), _collect(app, 4)
    chunks = []

    probe = _PROBE.get(app)
    if probe and probe in t3:
        chunks.append(("probe", [probe], t3.pop(probe)))

    if t2:
        chunks.append(("tier2", sorted(t2), sum(t2.values())))

    # Largest modules first so chunk sizes pack evenly.
    cur, cur_n = [], 0
    for mod, n in sorted(t3.items(), key=lambda kv: (-kv[1], kv[0])):
        if cur and cur_n + n > TIER3_CHUNK_TESTS:
            chunks.append((f"tier3-{len(chunks)}", cur, cur_n))
            cur, cur_n = [], 0
        cur.append(mod)
        cur_n += n
    if cur:
        chunks.append((f"tier3-{len(chunks)}", cur, cur_n))

    if t4:
        chunks.append(("tier4", sorted(t4), sum(t4.values())))

    return chunks


def _census(app):
    """Per-tier selected counts plus the whole selection, for the partition
    assertion `--plan-only` prints."""
    per_tier = {t: sum(_collect(app, t).values()) for t in (1, 2, 3, 4)}
    out = subprocess.run(
        [sys.executable, "-m", "pytest", "tests/", "--app", app,
         "--permissive-actuation", "--collect-only", "-q"],
        cwd=_E2E, capture_output=True, text=True, errors="replace",
    ).stdout
    whole = sum(1 for ln in out.splitlines() if "::" in ln)
    return per_tier, whole


def main():
    ap = argparse.ArgumentParser(
        description="Resumable chunked permissive actuation sweep.")
    ap.add_argument("--app", default="windows", help="app under sweep")
    ap.add_argument("--marker-log", help="the ONE marker log every chunk appends to")
    ap.add_argument("--progress", help="JSONL file, one line per finished chunk")
    ap.add_argument("--log-dir", help="directory the per-chunk pytest logs land in")
    ap.add_argument("--start-at", type=int, default=0,
                    help="resume: index of the first chunk to run")
    ap.add_argument("--stop-after", type=int, default=None,
                    help="last chunk index to run (inclusive)")
    ap.add_argument("--plan-only", action="store_true",
                    help="print the tier census and chunk plan, then exit")
    args = ap.parse_args()

    chunks = build_chunks(args.app)
    total = sum(c[2] for c in chunks)

    if args.plan_only:
        per_tier, whole = _census(args.app)
        print(f"[census] --app {args.app} whole selection: {whole}")
        for t in (1, 2, 3, 4):
            print(f"  tier_{t}: {per_tier[t]}")
        summed = sum(per_tier.values())
        verdict = "EXACT" if summed == whole else f"MISMATCH ({summed} != {whole})"
        print(f"  partition: {summed} vs {whole} -> {verdict}")
        print(f"  sweepable (tier_2+): {per_tier[2] + per_tier[3] + per_tier[4]}")

    print(f"[chunks] {len(chunks)} chunks, {total} tests total", flush=True)
    for i, (name, mods, n) in enumerate(chunks):
        print(f"  [{i:2d}] {name:10s} {n:4d} tests, {len(mods):3d} module(s)",
              flush=True)
    if args.plan_only:
        return 0

    missing = [f for f in ("marker_log", "progress", "log_dir")
               if getattr(args, f) is None]
    if missing:
        ap.error("required unless --plan-only: "
                 + ", ".join("--" + m.replace("_", "-") for m in missing))

    os.makedirs(args.log_dir, exist_ok=True)
    last = args.stop_after if args.stop_after is not None else len(chunks) - 1

    for i, (name, mods, n) in enumerate(chunks):
        if i < args.start_at or i > last:
            continue
        started = _dt.datetime.now().isoformat(timespec="seconds")
        chunk_log = os.path.join(args.log_dir, f"chunk-{i:02d}-{name}.log")
        print(f"\n[chunks] === {i} {name} ({n} tests) start {started} ===",
              flush=True)
        # Stamp the tree BEFORE the chunk as well as after. Stamping only after
        # is wrong in the UNSAFE direction: a chunk that ran against tree A and
        # finished after someone committed B is recorded as B, so a real split
        # reads as clean and the grader is never warned. Recording both ends
        # makes a mid-chunk move visible as `tree != tree_start` -- the one
        # shape a per-chunk stamp otherwise cannot see. (Measured 2026-09-11:
        # this run's own chunk 0 ran at one commit and is stamped with a later one,
        # because its launching session committed while it was running.)
        tree_start = _tree_sha()
        cmd = [sys.executable, "-m", "pytest", *mods, "-v",
               "--app", args.app, "--permissive-actuation",
               "--actuation-log", args.marker_log]
        with open(chunk_log, "w", encoding="utf-8", errors="replace") as fh:
            rc = subprocess.run(cmd, cwd=_E2E, stdout=fh,
                                stderr=subprocess.STDOUT).returncode
        ended = _dt.datetime.now().isoformat(timespec="seconds")
        with open(args.progress, "a", encoding="utf-8") as fh:
            fh.write(json.dumps({
                "index": i, "name": name, "tests": n, "modules": len(mods),
                "rc": rc, "started": started, "ended": ended, "log": chunk_log,
                "tree": _tree_sha(), "tree_start": tree_start, "modlist": mods,
            }) + "\n")
        print(f"[chunks] === {i} {name} rc={rc} end {ended} ===", flush=True)

    print("\n[chunks] ALL REQUESTED CHUNKS FINISHED", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
