"""The preflight's donor line and the prebuild timings record — what a cold
target would seed from, and what a stale prebuild job costs cold.

Owner: ``docs/goal/architecture/build-slot-pool.md`` § Cross-pool acquisition
order → *The preflight* (the plan header) and
``build-target-layout-macos.md`` § Warm-target seeding (mac) → *The standing
donor*, point 8. The machine's seed tool owns the donor layout; this module only
reads it, through ``SEED_SOURCE`` — conftest points it at the build-slot tool's
seeder loader, and where that fleet tooling is absent every line here reads
``unknown``.

* **The donor line.** For a target with no built profile:
  ``would seed from standing donor gen-<N> (<commit>, <age>, lock matches |
  lock differs)`` or ``no donor: <why>``. On the zfs backend no
  generation is minted — the transient snapshot of the gate check's dataset
  ``main`` is the generation — so the line names ``main``'s snapshot and reads
  its manifest off ``workspace-test-check``'s last-green stamp, the gate whose
  pass compiles the whole workspace's tests into ``main``
  (``build-target-layout-linux.md``, the standing-donor stance paragraph). For
  a built target: ``not needed``.
* **The timings record.** Each prebuild job the run executes appends one JSON
  line — job, commit, ``seeded | cold | warm``, wall seconds, ok — to
  ``PREBUILD_TIMINGS_LOG``. The state is the TARGET's when the run's first job
  started: ``cold`` with no built profile, ``seeded`` when built and the seed
  ledger names the target, ``warm`` otherwise. A ``stale`` job's plan line
  carries the median wall of that job's ``cold``, ``ok`` lines at least
  ``MIN_COLD_WALL_S`` long — a shorter one compiled nothing worth pricing — or
  ``no record yet``.
"""

from __future__ import annotations

import datetime
import hashlib
import json
import os
import statistics
import subprocess
import time
from pathlib import Path

_REPO = Path(__file__).resolve().parents[3]

#: The per-machine timings record. A module constant, never a knob: tests
#: redirect it (tests/e2e-unified/tests/conftest.py).
PREBUILD_TIMINGS_LOG = os.path.expanduser("~/.cache/fauna-e2e-prebuild-timings.jsonl")
#: A cold line shorter than this compiled nothing worth pricing.
MIN_COLD_WALL_S = 60.0
#: The gate whose pass builds `main`'s target on the zfs backend, whose
#: last-green stamp is that machine's donor manifest.
UBU_DONOR_GATE = "workspace-test-check"

#: A callable returning the machine's seed tool module, or None where it is
#: absent. Set by conftest (the build-slot tool's seeder loader); tests set
#: their own.
SEED_SOURCE = None
_SEED = None


def seed_module():
    """The seed tool, asked of SEED_SOURCE once; None where there is none."""
    global _SEED
    if _SEED is None and SEED_SOURCE is not None:
        _SEED = SEED_SOURCE()
    return _SEED


def target_root(repo: Path = _REPO) -> str:
    """Where this checkout's cargo writes: CARGO_TARGET_DIR (the zfs backend's per-checkout
    dataset) or `<checkout>/target`."""
    return os.environ.get("CARGO_TARGET_DIR") or str(repo / "target")


def _age(seconds: float) -> str:
    seconds = max(0.0, seconds)
    if seconds < 3600:
        return f"{int(seconds // 60)} min old"
    if seconds < 172800:
        return f"{seconds / 3600:.1f} h old"
    return f"{seconds / 86400:.1f} d old"


def _lock_hash_at(checkout: str, commit: str) -> str | None:
    try:
        out = subprocess.run(["git", "-C", checkout, "show", f"{commit}:Cargo.lock"],
                             capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return hashlib.sha256(out.stdout).hexdigest() if out.returncode == 0 else None


def donor_line(repo: Path = _REPO, now: float | None = None) -> str:
    """The plan header's donor line (module docstring). Never raises."""
    try:
        return _donor_line(repo, time.time() if now is None else now)
    except Exception as exc:  # noqa: BLE001 — a plan line, never a failed plan
        return f"donor: unknown ({type(exc).__name__}: {exc})"


def _donor_line(repo: Path, now: float) -> str:
    seed = seed_module()
    if seed is None:
        return "donor: unknown (no seed tool in this checkout)"
    target = target_root(repo)
    if os.path.isdir(target) and seed.destination_is_populated(target):
        return f"donor: not needed (target {target} is already built in)"
    own_lock = seed.file_sha256(str(repo / "Cargo.lock"))
    main = seed.main_checkout_of(str(repo))
    if main is None:
        return "no donor: cannot find the main checkout"
    if seed.BACKEND == "zfs":
        stamp = Path(main) / ".claude" / "gate-check" / f"{UBU_DONOR_GATE}.last-green-tip"
        try:
            commit = stamp.read_text(encoding="utf-8").strip()
            mtime = stamp.stat().st_mtime
        except OSError:
            return f"no donor: no {UBU_DONOR_GATE} last-green stamp at {stamp}"
        if not commit:
            return f"no donor: {stamp} is empty"
        lock = "lock matches" if _lock_hash_at(str(repo), commit) == own_lock else "lock differs"
        return (f"would seed from main's snapshot ({commit[:12]} per {UBU_DONOR_GATE}'s "
                f"last green, {_age(now - mtime)}, {lock})")
    root = seed.donor_root(main, seed.machine_name())
    gen = seed.current_generation(root)
    if gen is None:
        return f"no donor: no standing-donor generation published in {root}"
    manifest = seed.read_donor_manifest(gen)
    if manifest is None:
        return f"no donor: {gen} has no readable manifest"
    commit = manifest.get("commit") or "?"
    published = manifest.get("published_epoch")
    age = _age(now - float(published)) if isinstance(published, (int, float)) else "age unknown"
    lock = "lock matches" if manifest.get("cargo_lock_sha256") == own_lock else "lock differs"
    return f"would seed from standing donor {os.path.basename(gen)} ({commit[:12]}, {age}, {lock})"


def target_state(repo: Path = _REPO) -> str:
    """`cold | seeded | warm` for this checkout's target, now (module docstring)."""
    try:
        seed = seed_module()
        if seed is None:
            return "unknown"
        target = target_root(repo)
        if not (os.path.isdir(target) and seed.destination_is_populated(target)):
            return "cold"
        want = os.path.normpath(target)
        try:
            with open(seed.SEED_LOG, encoding="utf-8") as fh:
                for line in fh:
                    f = line.rstrip("\n").split("\t")
                    if len(f) > 8 and f[8] == seed.PUBLISH_MARKER:
                        continue
                    if len(f) > 1 and os.path.normpath(f[1]).startswith(want):
                        return "seeded"
        except OSError:
            pass
        return "warm"
    except Exception:  # noqa: BLE001 — a record field, never a failed prebuild
        return "unknown"


def record_job(job: str, commit: str | None, state: str, wall_s: float, ok: bool) -> None:
    """Append one timings line. Best-effort: never fails the run."""
    line = {
        "at": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "job": job,
        "commit": commit,
        "target": state,
        "wall_s": round(wall_s, 1),
        "ok": ok,
    }
    try:
        os.makedirs(os.path.dirname(PREBUILD_TIMINGS_LOG), exist_ok=True)
        with open(PREBUILD_TIMINGS_LOG, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(line, sort_keys=True) + "\n")
    except OSError:
        pass


def cold_cost(job: str) -> str:
    """The cold-cost note a stale job's plan line carries."""
    walls = []
    try:
        with open(PREBUILD_TIMINGS_LOG, encoding="utf-8") as fh:
            for raw in fh:
                try:
                    rec = json.loads(raw)
                except ValueError:
                    continue
                if (rec.get("job") == job and rec.get("target") == "cold" and rec.get("ok")
                        and float(rec.get("wall_s", 0)) >= MIN_COLD_WALL_S):
                    walls.append(float(rec["wall_s"]))
    except OSError:
        pass
    if not walls:
        return "cold cost: no record yet"
    return f"cold cost ~{statistics.median(walls) / 60:.0f} min (median of {len(walls)} cold run(s))"


def head_commit(repo: Path = _REPO) -> str | None:
    try:
        out = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                             capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip() or None
