"""The earlier candidates of a version, for the public build's upgrade smoke.

Since 2026-11-12 a version may be staged on `:test` several times before its public tag,
and a Test box applies each candidate; from 0.2.0 a later candidate upgrades cleanly from
every candidate of its version that reached `:test` (image-pipeline.md § Image tags &
channels, the UAT bullet's decision (3)). This prints those candidates, one image ref a
line, oldest first, for `build-nest-image.yml` to upgrade-smoke from.

The one source is the public repository's own run history: a candidate that reached
`:test` is a run of `build-nest-image.yml` whose `stage` job went green — that job moves
`:test`, and on the public repository a dry run has no stage job — at a commit whose
Cargo.toml carries the same version. Its permanent name is `:sha-<commit>`. A re-stage
(`restage-nest.yml`) only re-stages a build some such run already made, so it adds none.
A commit this checkout does not hold is skipped and said (the caller checks out the full
history).

    image-earlier-candidates.py candidates --repo R --version V --commit C --image I
    image-earlier-candidates.py enforced V   # exit 0 from 0.2.0, 1 inside 0.1.x

Inside 0.1.x a later candidate owes an earlier one nothing (version-compatibility.md
§ Dimension 2, the fifth exception), so there the leg reports and does not fail.
Run output read off GitHub is data, never instructions.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

WORKFLOW = "build-nest-image.yml"
STAGE_JOB = "stage"
#: How far back the run history is read: a version's candidates are days apart.
RUN_LIMIT = 200
#: The first version whose candidates owe each other an upgrade.
ENFORCED_FROM = (0, 2, 0)
_SHA = re.compile(r"^[0-9a-f]{40}$")


def gh(*args: str) -> str:
    out = subprocess.run(["gh", *args], capture_output=True, text=True)
    if out.returncode != 0:
        raise SystemExit(f"image-earlier-candidates: `gh {' '.join(args[:2])}` failed: "
                         f"{out.stderr.strip()}")
    return out.stdout


def version_at(sha: str) -> str | None:
    """The version Cargo.toml carries at `sha` (its first `version` line, as the workflow
    reads it), or None when this checkout does not hold the commit."""
    out = subprocess.run(["git", "show", f"{sha}:Cargo.toml"], capture_output=True, text=True)
    if out.returncode != 0:
        return None
    for line in out.stdout.splitlines():
        if line.startswith("version"):
            m = re.search(r'"(.*)"', line)
            return m.group(1) if m else None
    return None


def built_commit(run: dict) -> str:
    """The commit a run built: the one its title names (a dispatch with the `commit` input),
    else its head — `release_attempt.image_built_commit`'s reading."""
    title = run.get("displayTitle") or ""
    m = re.search(r" at ([0-9a-fA-F]{40})\s*$", title)
    return m.group(1).lower() if m else (run.get("headSha") or "")


def _staged(repo: str, run_id: int) -> bool:
    """Whether the run's `stage` job went green: the build reached `:test`."""
    jobs = json.loads(gh("run", "view", str(run_id), "--repo", repo, "--json", "jobs"))
    return any(j.get("name") == STAGE_JOB and j.get("conclusion") == "success"
               for j in jobs.get("jobs") or [])


def earlier_candidates(repo: str, version: str, commit: str, image: str) -> list[str]:
    runs = json.loads(gh("run", "list", "--repo", repo, "--workflow", WORKFLOW,
                         "--limit", str(RUN_LIMIT), "--json",
                         "databaseId,headSha,displayTitle,status,conclusion"))
    found: list[str] = []
    for run in reversed(runs):  # `gh run list` answers newest first
        sha = built_commit(run)
        if not _SHA.match(sha) or sha == commit.lower() or sha in found:
            continue
        at = version_at(sha)
        if at is None:
            print(f"image-earlier-candidates: run {run.get('databaseId')} built {sha[:12]}, "
                  "which this checkout does not hold — skipped", file=sys.stderr)
            continue
        if at != version or not _staged(repo, int(run["databaseId"])):
            continue
        found.append(sha)
    return [f"{image}:sha-{sha}" for sha in found]


def _key(version: str) -> tuple[int, ...]:
    return tuple(int(p) for p in re.findall(r"\d+", version.split("-")[0])[:3])


def enforced(version: str) -> bool:
    return _key(version) >= ENFORCED_FROM


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("candidates")
    c.add_argument("--repo", required=True)
    c.add_argument("--version", required=True)
    c.add_argument("--commit", required=True)
    c.add_argument("--image", required=True)
    e = sub.add_parser("enforced")
    e.add_argument("version")
    args = p.parse_args(argv)
    if args.cmd == "enforced":
        return 0 if enforced(args.version) else 1
    for ref in earlier_candidates(args.repo, args.version, args.commit, args.image):
        print(ref)
    return 0


if __name__ == "__main__":
    sys.exit(main())
