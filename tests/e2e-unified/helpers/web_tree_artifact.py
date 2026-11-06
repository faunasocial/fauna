"""The served web tree, consumed off Linux — never built there.

`build-system.md` § The Deno build sandbox → *The web tree is consumed off
Linux, never built there*. Every green `build-nest-image.yml` run uploads the
tree the nest image serves (the Dockerfile's `web-tree` stage, its
`asset-manifest.json` naming the commit) as the run artifact `web-tree`. A
harness off Linux — macos and ios on macOS, windows on Windows — downloads the
newest CURRENT one with `gh run download` and serves it as `served_spa_dir`,
so the private share link viewer is witnessed on those columns too.

CURRENT is the gate this module exists for: the run's commit is an ancestor
of the checkout's `HEAD` AND every path in `VIEWER_INPUTS` has the same git
object id at that commit as at `HEAD`. A viewer that is not `HEAD`'s would
record a witness of code the checkout does not hold (`testing.md` § Default
app and nest mode → *A docker-mode run observes the IMAGE's nest*, the
incident class).

Every way to come up empty — no `gh`, no login, no network, nothing in
retention, every candidate stale, a bad download — raises one
`WebTreeUnavailable` whose message names the reason; the fixture turns it into
an environment skip, so the column keeps its previous record instead of
showing a red that accuses the artifact's age. Linux never comes here: it
builds the tree itself (`just web-test`).
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

#: `gh` runs inside the checkout and resolves the repository from its own
#: remote, so a clone of either repository reads that repository's runs.
WORKFLOW = "build-nest-image.yml"
ARTIFACT = "web-tree"
#: How many green runs to consider, newest first.
RUN_LIMIT = 30
#: Bounds on every network call, so a per-test timeout never reads one as a hang.
LIST_TIMEOUT_S = 60
DOWNLOAD_TIMEOUT_S = 120
FETCH_TIMEOUT_S = 120

#: The declared viewer inputs — this tuple is their ONE home. Every path whose
#: change can change what the nest-served viewer page does: the viewer entry
#: and shell, its wasm chunk and every crate module that chunk wraps
#: (`fauna_client_share`'s `fauna_core` / `fauna_protocol` imports, the shared
#: strings it renders), and the nest's arm that serves it. Over-approximating
#: is the safe direction: a path added costs coverage frequency, a path missed
#: records a witness of a viewer that is not HEAD's.
VIEWER_INPUTS = (
    # the viewer entry and shell
    "apps/fauna-web/share-viewer",
    "apps/fauna-web/src/lib/share-viewer",
    "apps/fauna-web/vite.share-viewer.config.ts",
    "apps/fauna-web/src/lib/i18n/strings.ts",
    # its wasm chunk and the crates it wraps
    "libs/fauna-wasm-share",
    "libs/fauna-client-share",
    "libs/fauna-core/src/share.rs",
    "libs/fauna-core/src/chunk.rs",
    "libs/fauna-core/src/chunk_crypto.rs",
    "libs/fauna-core/src/crypto.rs",
    "libs/fauna-core/src/data.rs",
    "libs/fauna-core/src/file_download.rs",
    "libs/fauna-core/src/identity.rs",
    "libs/fauna-core/src/label_custody.rs",
    "libs/fauna-core/src/path_crypto.rs",
    "libs/fauna-protocol/src/share.rs",
    "libs/fauna-i18n",
    "i18n/strings/en.yaml",
    # the nest's arm
    "bins/fauna-nest/src/share_routes.rs",
)

_REPO_ROOT = Path(__file__).resolve().parents[3]
_MANIFEST_SCRIPT = _REPO_ROOT / "scripts" / "web-asset-manifest.py"
_VIEWER_PAGE = "share-viewer.html"
_MANIFEST = "asset-manifest.json"


class WebTreeUnavailable(Exception):
    """No current web tree can be had here; the message names why."""


@dataclass(frozen=True)
class Candidate:
    run_id: int
    commit: str
    created_at: str


def default_cache_root() -> Path:
    return Path.home() / ".cache" / "fauna" / "web-tree"


def _gh() -> str:
    gh = shutil.which("gh")
    if gh is None:
        raise WebTreeUnavailable("no `gh` on PATH")
    return gh


def _first_line(text: str) -> str:
    line = next((ln.strip() for ln in text.splitlines() if ln.strip()), "")
    return line[:200]


def candidates(repo: Path = _REPO_ROOT) -> list[Candidate]:
    """The green `build-nest-image.yml` runs, newest first."""
    argv = [
        _gh(), "run", "list", "-w", WORKFLOW, "-s", "success",
        "--json", "databaseId,headSha,createdAt", "-L", str(RUN_LIMIT),
    ]
    try:
        proc = subprocess.run(
            argv, capture_output=True, text=True, timeout=LIST_TIMEOUT_S, cwd=repo
        )
    except subprocess.TimeoutExpired:
        raise WebTreeUnavailable(
            f"no network: `gh run list` did not answer within {LIST_TIMEOUT_S}s"
        ) from None
    if proc.returncode != 0:
        err = proc.stderr.lower()
        if "gh auth login" in err or "not logged" in err or "authentication" in err:
            raise WebTreeUnavailable("no `gh` login (run `gh auth login`)")
        raise WebTreeUnavailable(
            f"no network: `gh run list` exited {proc.returncode}: {_first_line(proc.stderr)}"
        )
    try:
        runs = json.loads(proc.stdout or "[]")
        found = [
            Candidate(int(r["databaseId"]), str(r["headSha"]), str(r["createdAt"]))
            for r in runs
        ]
    except (ValueError, KeyError, TypeError):
        raise WebTreeUnavailable("`gh run list` answered something that is not its run list") from None
    # ISO-8601 UTC timestamps sort as strings.
    return sorted(found, key=lambda c: c.created_at, reverse=True)


def _git(repo: Path, *args: str, timeout: int = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, timeout=timeout
    )


def _has_commit(repo: Path, commit: str) -> bool:
    return _git(repo, "cat-file", "-e", f"{commit}^{{commit}}").returncode == 0


def _object_id(repo: Path, rev: str, path: str) -> str | None:
    proc = _git(repo, "rev-parse", "--verify", "--quiet", f"{rev}:{path}")
    return proc.stdout.strip() if proc.returncode == 0 else None


class _Gate:
    """`is_current` over one `HEAD`, fetching `origin main` at most ONCE."""

    def __init__(self, repo: Path, head: str, inputs=VIEWER_INPUTS):
        self.repo = repo
        self.head = head
        self.inputs = tuple(inputs)
        self._head_ids = {p: _object_id(repo, head, p) for p in self.inputs}
        self._fetched = False

    def staleness(self, commit: str) -> str | None:
        """None when `commit` is current for `HEAD`; else why it is not."""
        if not _has_commit(self.repo, commit) and not self._fetched:
            self._fetched = True
            try:
                _git(self.repo, "fetch", "origin", "main", timeout=FETCH_TIMEOUT_S)
            except subprocess.TimeoutExpired:
                pass
        if not _has_commit(self.repo, commit):
            return "its commit is not in the object store"
        if _git(self.repo, "merge-base", "--is-ancestor", commit, self.head).returncode != 0:
            return "its commit is not an ancestor of HEAD"
        moved = [
            p for p in self.inputs
            if self._head_ids[p] is None
            or _object_id(self.repo, commit, p) != self._head_ids[p]
        ]
        if moved:
            return "viewer inputs moved since: " + ", ".join(moved)
        return None


def is_current(commit: str, head: str, repo: Path = _REPO_ROOT, inputs=VIEWER_INPUTS) -> bool:
    return _Gate(repo, head, inputs).staleness(commit) is None


def _manifest_commit(tree: Path) -> str:
    proc = subprocess.run(
        [sys.executable, str(_MANIFEST_SCRIPT), "commit", str(tree / _MANIFEST)],
        capture_output=True, text=True, timeout=30,
    )
    if proc.returncode != 0:
        raise WebTreeUnavailable(f"a bad download: {_MANIFEST} is unreadable")
    return proc.stdout.strip()


def _verified(tree: Path, commit: str) -> bool:
    if not (tree / _VIEWER_PAGE).is_file() or not (tree / _MANIFEST).is_file():
        return False
    try:
        return _manifest_commit(tree) == commit
    except WebTreeUnavailable:
        return False


class _NoArtifact(Exception):
    """The run holds no `web-tree` artifact (expired, or before the producer)."""


def _download(cand: Candidate, cache_root: Path, repo: Path) -> Path:
    dest = cache_root / cand.commit
    partial = cache_root / f"{cand.commit}.partial"
    shutil.rmtree(partial, ignore_errors=True)
    partial.mkdir(parents=True)
    argv = [_gh(), "run", "download", str(cand.run_id), "-n", ARTIFACT, "-D", str(partial)]
    try:
        proc = subprocess.run(
            argv, capture_output=True, text=True, timeout=DOWNLOAD_TIMEOUT_S, cwd=repo
        )
    except subprocess.TimeoutExpired:
        shutil.rmtree(partial, ignore_errors=True)
        raise WebTreeUnavailable(
            f"no network: `gh run download {cand.run_id}` did not finish within {DOWNLOAD_TIMEOUT_S}s"
        ) from None
    if proc.returncode != 0:
        shutil.rmtree(partial, ignore_errors=True)
        err = proc.stderr.lower()
        if "no artifact" in err or "no valid artifact" in err or "expired" in err:
            raise _NoArtifact()
        raise WebTreeUnavailable(
            f"a bad download: `gh run download {cand.run_id}` exited {proc.returncode}: "
            f"{_first_line(proc.stderr)}"
        )
    if not (partial / _VIEWER_PAGE).is_file():
        shutil.rmtree(partial, ignore_errors=True)
        raise WebTreeUnavailable(f"a bad download: run {cand.run_id}'s tree holds no {_VIEWER_PAGE}")
    got = _manifest_commit(partial) if (partial / _MANIFEST).is_file() else None
    if got != cand.commit:
        shutil.rmtree(partial, ignore_errors=True)
        raise WebTreeUnavailable(
            f"a bad download: run {cand.run_id}'s {_MANIFEST} names commit {got}, "
            f"the run built {cand.commit}"
        )
    shutil.rmtree(dest, ignore_errors=True)
    partial.rename(dest)
    return dest


def ensure_tree(
    repo: Path = _REPO_ROOT, cache_root: Path | None = None, inputs=VIEWER_INPUTS
) -> Path:
    """The newest current `web-tree`, cached by commit; raises `WebTreeUnavailable`."""
    cache_root = cache_root or default_cache_root()
    head_proc = _git(repo, "rev-parse", "HEAD")
    if head_proc.returncode != 0:
        raise WebTreeUnavailable("the checkout has no HEAD")
    head = head_proc.stdout.strip()
    cands = candidates(repo)
    if not cands:
        raise WebTreeUnavailable(f"nothing in retention: no green {WORKFLOW} run")
    gate = _Gate(repo, head, inputs)
    newest_stale: tuple[Candidate, str] | None = None
    for cand in cands:
        why = gate.staleness(cand.commit)
        if why is not None:
            newest_stale = newest_stale or (cand, why)
            continue
        cached = cache_root / cand.commit
        if _verified(cached, cand.commit):
            return cached
        try:
            return _download(cand, cache_root, repo)
        except _NoArtifact:
            continue
    if newest_stale is None:
        raise WebTreeUnavailable(
            f"nothing in retention: no current {WORKFLOW} run still holds a `{ARTIFACT}` artifact"
        )
    cand, why = newest_stale
    raise WebTreeUnavailable(
        f"every candidate is stale — the newest artifact's commit {cand.commit[:12]} "
        f"(run {cand.run_id}): {why}"
    )


if __name__ == "__main__":
    try:
        print(ensure_tree())
    except WebTreeUnavailable as e:
        print(f"web-tree unavailable: {e}", file=sys.stderr)
        sys.exit(1)
