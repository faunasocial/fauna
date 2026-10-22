"""The last-green directory: when each collected test last passed, per platform.

`docs/goal/architecture/delayed-testing.md` § The last-green directory owns the key,
the record, the machine-local home, the published snapshot and the staleness reading;
this module is that section's implementation. Its writer is conftest's
`pytest_runtest_logreport` / `pytest_sessionfinish` — the same event that feeds the
feature ledger (`helpers/feature_ledger.py`), one event, two projections: this one is
the per-test truth for EVERY collected test, tagged or not, and the ledger stays its
feature-indexed view with its semantics unchanged. Its other two readers are the
merge-gate check's ledger harvest, which publishes the snapshot, and `scripts/features_catalog.py` (`load_last_green`, `behind_commits`).

**The key** is the test as pytest stamps it, repo-relative, WITH its parametrization
(`tests/e2e-unified/tests/test_x.py::test_y[tui]`) — app and non-default nest mode are
part of the key; the ledger's keys drop it, this one must not. **The platform** is the
host OS (`feature_ledger.host_platform`), one file per platform.

**The record:**

* `last_green` — `{commit, base, time, duration, origin, dirty}`: the newest green.
  `base` is the merge-base with `origin/main` (the ledger's field), `time` is UTC to
  the second, `duration` the test's setup + call + teardown seconds, `origin` who ran
  it (`session`, `check`, `tester` — `FAUNA_RUN_ORIGIN`, default `session`), `dirty`
  the ledger's tree mark.
* `last` — `{outcome, commit, time}`: the newest outcome, `green`, `red`, `error` or
  `skip:<class>` (convention 7's two declared classes; an environment or undeclared
  skip, an xfail and an xpass observe nothing about the test and record no outcome).
* `red_since` — `{tip, count}`: the commit at which the test went red and the count of
  consecutive reds since; cleared by a green.
* `flaky` — `{count, last}`: reds that went green on a re-run at the SAME commit — the
  tree did not change, so the outcome did not come from the code.
* `collected` — the day a collection last named the node id: the 45-day prune's clock.

**Never fatal.** A run that produced real results must not be turned red by a
bookkeeping failure; the conftest wrapper reports a broken write and moves on.
"""

from __future__ import annotations

import json
import os
import subprocess
from contextlib import contextmanager
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

try:  # POSIX
    import fcntl
except ImportError:  # pragma: no cover - windows
    fcntl = None
try:  # windows
    import msvcrt
except ImportError:
    msvcrt = None

REPO = Path(__file__).resolve().parents[3]
SCHEMA = 1
#: The published snapshot's directory, repo-relative — committed by the merge-gate
#: check's ledger publish and by nothing else (§ The published snapshot).
SNAPSHOT_REL = "docs/features/last-green"
#: A record whose node id no collection has named for this many days is dropped at
#: the snapshot's publish — the work queue's eviction age, for the same reason.
PRUNE_DAYS = 45
ORIGINS = ("session", "check", "tester")
#: Overrides the machine-local home — for the tests of this module and of the
#: harvest, which must never write into the box's real directory.
HOME_ENV = "FAUNA_LAST_GREEN_DIR"
ORIGIN_ENV = "FAUNA_RUN_ORIGIN"

GREEN, RED, ERROR = "green", "red", "error"
#: Worst-outcome-wins within one session, as the ledger orders it: a test whose call
#: passed and whose teardown blew up was an error.
_SEVERITY = {ERROR: 0, RED: 1, "skip": 2, GREEN: 3}

# ── per-session state, set by conftest ──────────────────────────────────────
#: {"commit", "base", "dirty", "origin", "node_ids": {pytest nodeid: key}, "today"}
_run: dict = {}
#: {pytest nodeid: {"outcome": str | None, "duration": float, "time": str | None}}
_observed: dict = {}


def run_origin() -> str:
    value = os.environ.get(ORIGIN_ENV, "").strip()
    return value if value in ORIGINS else "session"


def begin_run(*, commit: str, base: str | None, dirty: bool, node_ids: dict,
              today: str | None = None, live: bool = False) -> None:
    """The run's context, resolved once at collection beside the ledger's — the same
    commit, base and dirt, so the two projections can never disagree about them.

    **The first call of a session wins.** A session collects once; a second call is a
    tier_1 test driving the live collection hook over fixture items, which must not
    replace the live run's selection with its own (measured: every record of the
    session but one was written outcome-less).

    `live` is true only when the caller is a genuine pytest session (its `config` is
    a `pytest.Config`): a run begun over fixture items may be merged into an explicit
    directory but never into the box's real home (`write`)."""
    if _run:
        return
    _run.clear()
    _run.update({"commit": commit, "base": base or None, "dirty": bool(dirty),
                 "origin": run_origin(), "node_ids": dict(node_ids), "live": bool(live),
                 "today": today or date.today().isoformat()})
    _observed.clear()


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def classify(outcome: str | None, skip_class: str | None) -> str | None:
    """A report outcome as the directory records it, or `None` for no observation."""
    if outcome == "passed":
        return GREEN
    if outcome == "failed":
        return RED
    if outcome == "error":
        return ERROR
    if outcome == "skipped" and skip_class in ("unbuilt", "absence"):
        return f"skip:{skip_class}"
    return None


def _rank(kind: str) -> int:
    return _SEVERITY["skip" if kind.startswith("skip:") else kind]


def note(nodeid: str, *, duration: float, outcome: str | None = None,
         skip_class: str | None = None, time: str | None = None) -> None:
    """One report phase. Every phase adds its duration; a phase with an outcome
    records it unless a worse one already stands for this test this session."""
    if not _run or nodeid not in _run["node_ids"]:
        return
    seen = _observed.setdefault(nodeid, {"outcome": None, "duration": 0.0, "time": None})
    seen["duration"] = round(seen["duration"] + (duration or 0.0), 3)
    kind = classify(outcome, skip_class)
    if kind is None:
        return
    if seen["outcome"] is None or _rank(kind) < _rank(seen["outcome"]):
        seen["outcome"] = kind
    seen["time"] = time or _now()


# ── the record ──────────────────────────────────────────────────────────────
def empty_record() -> dict:
    return {"collected": None, "flaky": {"count": 0, "last": None}, "last": None,
            "last_green": None, "red_since": None}


def observe(record: dict | None, *, outcome: str | None, time: str | None,
            duration: float, run: dict) -> dict:
    """Fold one observation into a record — pure, so the fixture world pins it.

    Newest observation wins: when the record already holds a `last` newer than this
    one (a sibling run wrote while this one ran), only a newer green than its
    `last_green` still lands, and the red/flaky bookkeeping is left to the newer run.
    """
    rec = json.loads(json.dumps(record)) if record else empty_record()
    rec["collected"] = run["today"]
    if outcome is None:
        return rec
    green = None
    if outcome == GREEN:
        green = {"base": run["base"], "commit": run["commit"], "dirty": run["dirty"],
                 "duration": round(duration, 3), "origin": run["origin"], "time": time}
    prev = rec.get("last")
    if prev and (prev.get("time") or "") > (time or ""):
        if green and (rec.get("last_green") or {}).get("time", "") < time:
            rec["last_green"] = green
        return rec
    if green:
        if prev and prev.get("outcome") in (RED, ERROR) and prev.get("commit") == run["commit"]:
            rec["flaky"] = {"count": (rec.get("flaky") or {}).get("count", 0) + 1, "last": time}
        rec["last_green"] = green
        rec["red_since"] = None
    elif outcome in (RED, ERROR):
        since = rec.get("red_since")
        rec["red_since"] = ({"count": since["count"] + 1, "tip": since["tip"]} if since
                            else {"count": 1, "tip": run["commit"]})
    rec["last"] = {"commit": run["commit"], "outcome": outcome, "time": time}
    return rec


def newest(a: dict | None, b: dict | None) -> dict | None:
    """Of two records for one key, the one holding the newer observation."""
    def stamp(rec):
        return ((rec.get("last") or {}).get("time") or "", rec.get("collected") or "")
    if a is None or b is None:
        return a if b is None else b
    return b if stamp(b) >= stamp(a) else a


def prune(tests: dict, today: date, *, days: int = PRUNE_DAYS) -> tuple:
    """`(kept, dropped ids)`: a record no collection has named for `days` days goes."""
    floor = (today - timedelta(days=days)).isoformat()
    kept, dropped = {}, []
    for key, rec in tests.items():
        if (rec.get("collected") or "") < floor:
            dropped.append(key)
        else:
            kept[key] = rec
    return kept, sorted(dropped)


# ── the file ────────────────────────────────────────────────────────────────
def main_checkout(repo: Path = REPO) -> Path:
    """The checkout whose `.git` every linked checkout of this repository shares —
    the machine-local home's anchor, so a session's checkout, the check's pinned
    tree and the tester all write one file. Falls back to `repo` when git cannot say."""
    try:
        common = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=10, check=True).stdout.strip()
    except Exception:
        return repo
    common_dir = Path(common)
    return common_dir.parent if common_dir.name == ".git" else repo


def home(repo: Path = REPO) -> Path:
    """`<main checkout>/.claude/last-green/` — machine-local and authoritative."""
    override = os.environ.get(HOME_ENV)
    return Path(override) if override else main_checkout(repo) / ".claude" / "last-green"


def load(path: Path) -> dict:
    """The file's `tests` map; `{}` when it is absent, unreadable or another schema."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return (data.get("tests") or {}) if data.get("schema") == SCHEMA else {}


def dumps(platform: str, tests: dict) -> str:
    """One record per line: valid JSON, a reviewable diff, and a fraction of the
    ledger's indented size at ~8,400 records a platform."""
    rows = [f"{json.dumps(key)}: {json.dumps(rec, sort_keys=True, separators=(',', ':'))}"
            for key, rec in sorted(tests.items())]
    body = ",\n".join(rows)
    return (f'{{"platform": {json.dumps(platform)}, "schema": {SCHEMA}, "tests": {{\n'
            + (body + "\n" if body else "") + "}}\n")


def save(path: Path, platform: str, tests: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(dumps(platform, tests), encoding="utf-8", newline="\n")
    os.replace(tmp, path)


@contextmanager
def _locked(directory: Path):
    """Every pytest run on the box merges into one file: serialise the read-modify-
    write so two runs finishing together cannot drop each other's records."""
    directory.mkdir(parents=True, exist_ok=True)
    with open(directory / ".lock", "a+") as fh:
        if fcntl is not None:
            fcntl.flock(fh, fcntl.LOCK_EX)
        elif msvcrt is not None:  # pragma: no cover - windows
            fh.seek(0)
            msvcrt.locking(fh.fileno(), msvcrt.LK_LOCK, 1)
        try:
            yield
        finally:
            if fcntl is not None:
                fcntl.flock(fh, fcntl.LOCK_UN)
            elif msvcrt is not None:  # pragma: no cover - windows
                fh.seek(0)
                msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)


def write(directory: Path | None = None, *, platform: str | None = None) -> Path | None:
    """Merge this session into `<home>/<platform>.json`; the path, or `None` when the
    run recorded nothing (`--no-feature-ledger`, an empty selection)."""
    if not _run or not _run["node_ids"]:
        return None
    if directory is None and not os.environ.get(HOME_ENV) and not _run.get("live"):
        # Fixture items driving the collection hook (a tier_1 test of the hook) must
        # never reach the box's real directory: junk keys there ride the check's
        # publish into the snapshot and live 45 days.
        return None
    from helpers.feature_ledger import host_platform

    platform = platform or host_platform()
    directory = directory or home()
    path = directory / f"{platform}.json"
    with _locked(directory):
        tests = load(path)
        for nodeid, key in _run["node_ids"].items():
            seen = _observed.get(nodeid) or {}
            tests[key] = observe(tests.get(key), outcome=seen.get("outcome"),
                                 time=seen.get("time"), duration=seen.get("duration", 0.0),
                                 run=_run)
        save(path, platform, tests)
    return path


def reset_session_state() -> None:
    """Test-only: drop everything accumulated so a unit test starts clean."""
    _run.clear()
    _observed.clear()
