"""The web tree consumed off Linux: `helpers/web_tree_artifact.py`.

`build-system.md` § The Deno build sandbox → *The web tree is consumed off
Linux, never built there*, bound (b): an artifact is served only when it is
CURRENT — its commit an ancestor of `HEAD` and every declared viewer input at
the same git object id — and every way to come up empty is a named
`WebTreeUnavailable` reason. The gate runs over a throwaway two-commit repo;
`gh` is a fake executable on PATH, so nothing here touches the network.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from helpers import web_tree_artifact as wta
from helpers.web_tree_artifact import WebTreeUnavailable

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]

_INPUTS = ("viewer", "lib/share.rs")

_TITLE = "Build, stage and verify the nest Docker image"

_FAKE_GH = r'''
import json, os, shutil, sys
args = sys.argv[1:]
mode = os.environ.get("FAKE_GH_MODE", "")
if args[:2] == ["run", "list"]:
    if mode == "nologin":
        sys.stderr.write("To get started with GitHub CLI, please run:  gh auth login\n")
        sys.exit(4)
    if mode == "offline":
        sys.stderr.write("error connecting to api.github.com\n")
        sys.exit(1)
    sys.stdout.write(open(os.environ["FAKE_GH_RUNS"]).read())
    sys.exit(0)
if args[:2] == ["run", "download"]:
    run_id = args[2]
    with open(os.environ["FAKE_GH_LOG"], "a") as log:
        log.write(run_id + "\n")
    dest = args[args.index("-D") + 1]
    src = os.path.join(os.environ["FAKE_GH_TREES"], run_id)
    if not os.path.isdir(src):
        sys.stderr.write("no artifact matches any of the names or patterns provided\n")
        sys.exit(1)
    shutil.copytree(src, dest, dirs_exist_ok=True)
    sys.exit(0)
sys.exit(2)
'''


def _git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout.strip()


def _commit(repo, files: dict, message: str) -> str:
    for rel, text in files.items():
        path = repo / rel
        if text is None:
            path.unlink()
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "config", "user.email", "fixture@example.invalid")
    _git(repo, "config", "user.name", "fixture")
    _git(repo, "config", "commit.gpgsign", "false")
    return repo


@pytest.fixture
def fake_gh(tmp_path, monkeypatch):
    """A fake `gh` first on PATH; returns a handle to set its run list and trees."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    script = bindir / "fake_gh.py"
    script.write_text(_FAKE_GH)
    if sys.platform == "win32":
        (bindir / "gh.cmd").write_text(f'@"{sys.executable}" "{script}" %*\n')
    else:
        gh = bindir / "gh"
        gh.write_text(f'#!{sys.executable}\n' + _FAKE_GH)
        gh.chmod(0o755)
    monkeypatch.setenv("PATH", str(bindir) + os.pathsep + os.environ.get("PATH", ""))
    runs = tmp_path / "runs.json"
    runs.write_text("[]")
    trees = tmp_path / "trees"
    trees.mkdir()
    log = tmp_path / "downloads.log"
    log.write_text("")
    monkeypatch.setenv("FAKE_GH_RUNS", str(runs))
    monkeypatch.setenv("FAKE_GH_TREES", str(trees))
    monkeypatch.setenv("FAKE_GH_LOG", str(log))
    monkeypatch.delenv("FAKE_GH_MODE", raising=False)

    class Handle:
        def set_runs(self, runs_list):
            """Each run is (id, headSha, createdAt[, displayTitle])."""
            runs.write_text(json.dumps([
                {"databaseId": r[0], "headSha": r[1], "createdAt": r[2],
                 "displayTitle": r[3] if len(r) > 3 else _TITLE}
                for r in runs_list
            ]))

        def add_tree(self, run_id, commit, *, viewer=True):
            tree = trees / str(run_id)
            tree.mkdir()
            if viewer:
                (tree / "share-viewer.html").write_text("<html></html>")
            (tree / "asset-manifest.json").write_text(json.dumps({"commit": commit}))

        def downloads(self):
            return log.read_text().split()

        def mode(self, value):
            monkeypatch.setenv("FAKE_GH_MODE", value)

    return Handle()


# -- the gate -------------------------------------------------------------


def test_an_unchanged_input_set_is_current(repo):
    old = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    head = _commit(repo, {"unrelated.txt": "x"}, "two")
    assert wta.is_current(old, head, repo, _INPUTS)


def test_a_changed_input_is_stale(repo):
    old = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    head = _commit(repo, {"viewer/a.ts": "2"}, "two")
    assert not wta.is_current(old, head, repo, _INPUTS)
    why = wta._Gate(repo, head, _INPUTS).staleness(old)
    assert why == "viewer inputs moved since: viewer"


def test_an_input_missing_at_the_old_commit_is_stale(repo):
    old = _commit(repo, {"viewer/a.ts": "1"}, "one")
    head = _commit(repo, {"lib/share.rs": "1"}, "two")
    assert "lib/share.rs" in wta._Gate(repo, head, _INPUTS).staleness(old)


def test_a_commit_not_an_ancestor_of_head_is_stale(repo):
    base = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    _git(repo, "checkout", "-q", "-b", "side")
    side = _commit(repo, {"unrelated.txt": "side"}, "side")
    _git(repo, "checkout", "-q", "main")
    head = _commit(repo, {"other.txt": "main"}, "main")
    assert wta.is_current(base, head, repo, _INPUTS)
    assert wta._Gate(repo, head, _INPUTS).staleness(side) == "its commit is not an ancestor of HEAD"


def test_a_commit_absent_from_the_object_store_is_stale(repo):
    head = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    why = wta._Gate(repo, head, _INPUTS).staleness("0" * 40)
    assert why == "its commit is not in the object store"


def test_the_declared_inputs_exist_at_head():
    missing = [p for p in wta.VIEWER_INPUTS if not (_REPO / p).exists()]
    assert not missing, f"VIEWER_INPUTS names paths that no longer exist: {missing}"


# -- candidates and ensure_tree ---------------------------------------------


def test_the_newest_current_candidate_wins_over_a_newer_stale_one(repo, fake_gh, tmp_path):
    old = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    current = _commit(repo, {"unrelated.txt": "x"}, "two")
    stale = _commit(repo, {"viewer/a.ts": "2"}, "three")  # stale once HEAD moves back
    _commit(repo, {"more.txt": "y"}, "four")
    # Listed out of order: the helper sorts newest first.
    fake_gh.set_runs([
        (1, old, "2026-10-01T00:00:00Z"),
        (3, stale, "2026-10-03T00:00:00Z"),
        (2, current, "2026-10-02T00:00:00Z"),
    ])
    assert [c.run_id for c in wta.candidates(repo)] == [3, 2, 1]
    for rid, sha in ((1, old), (2, current), (3, stale)):
        fake_gh.add_tree(rid, sha)
    # HEAD ("four") holds the viewer at "2": run 3 is newest AND current.
    got = wta.ensure_tree(repo, tmp_path / "cache", _INPUTS)
    assert got == tmp_path / "cache" / stale
    _git(repo, "reset", "-q", "--hard", current)
    _commit(repo, {"later.txt": "z"}, "five")
    # Now HEAD's viewer is "1": run 3 is not an ancestor, run 2 (newer than 1) wins.
    got = wta.ensure_tree(repo, tmp_path / "cache2", _INPUTS)
    assert got == tmp_path / "cache2" / current
    assert (got / "share-viewer.html").is_file()


def test_a_cache_hit_downloads_nothing(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    fake_gh.set_runs([(7, sha, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(7, sha)
    cache = tmp_path / "cache"
    first = wta.ensure_tree(repo, cache, _INPUTS)
    second = wta.ensure_tree(repo, cache, _INPUTS)
    assert first == second == cache / sha
    assert fake_gh.downloads() == ["7"]


def test_a_run_without_the_artifact_falls_through_to_the_next_current_one(repo, fake_gh, tmp_path):
    older = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    newer = _commit(repo, {"unrelated.txt": "x"}, "two")
    fake_gh.set_runs([(2, newer, "2026-10-02T00:00:00Z"), (1, older, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(1, older)  # run 2 predates the producer: no artifact
    assert wta.ensure_tree(repo, tmp_path / "cache", _INPUTS) == tmp_path / "cache" / older
    assert fake_gh.downloads() == ["2", "1"]


# -- the named reasons ------------------------------------------------------


def _reason(repo, tmp_path):
    with pytest.raises(WebTreeUnavailable) as e:
        wta.ensure_tree(repo, tmp_path / "cache", _INPUTS)
    return str(e.value)


def test_no_gh_on_path(repo, tmp_path, monkeypatch):
    _commit(repo, {"viewer/a.ts": "1"}, "one")
    monkeypatch.setattr(wta.shutil, "which", lambda name, *a, **k: None)
    assert _reason(repo, tmp_path) == "no `gh` on PATH"


def test_no_gh_login(repo, fake_gh, tmp_path):
    _commit(repo, {"viewer/a.ts": "1"}, "one")
    fake_gh.mode("nologin")
    assert _reason(repo, tmp_path).startswith("no `gh` login")


def test_no_network(repo, fake_gh, tmp_path):
    _commit(repo, {"viewer/a.ts": "1"}, "one")
    fake_gh.mode("offline")
    assert _reason(repo, tmp_path).startswith("no network:")


def test_nothing_in_retention(repo, fake_gh, tmp_path):
    _commit(repo, {"viewer/a.ts": "1"}, "one")
    assert _reason(repo, tmp_path).startswith("nothing in retention: no green")


def test_nothing_in_retention_when_no_current_run_has_the_artifact(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    fake_gh.set_runs([(1, sha, "2026-10-01T00:00:00Z")])
    assert _reason(repo, tmp_path).startswith("nothing in retention: no current")


def test_every_candidate_stale_names_the_moved_paths_and_the_newest_commit(repo, fake_gh, tmp_path):
    old = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    newer = _commit(repo, {"unrelated.txt": "x"}, "two")
    _commit(repo, {"lib/share.rs": "2"}, "three")
    fake_gh.set_runs([(1, old, "2026-10-01T00:00:00Z"), (2, newer, "2026-10-02T00:00:00Z")])
    reason = _reason(repo, tmp_path)
    assert reason.startswith("every candidate is stale")
    assert newer[:12] in reason and "run 2" in reason
    assert reason.endswith("viewer inputs moved since: lib/share.rs")
    assert fake_gh.downloads() == []


def test_a_manifest_commit_mismatch_is_a_bad_download(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    fake_gh.set_runs([(1, sha, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(1, "f" * 40)
    reason = _reason(repo, tmp_path)
    assert reason.startswith("a bad download:") and "f" * 40 in reason
    assert not (tmp_path / "cache" / sha).exists()


def test_a_tree_without_the_viewer_page_is_a_bad_download(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    fake_gh.set_runs([(1, sha, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(1, sha, viewer=False)
    assert "share-viewer.html" in _reason(repo, tmp_path)


# -- the ancestor tree (bound (d)): the run goes to the artifact's commit -----


def test_a_dispatch_naming_a_commit_is_read_at_the_commit_it_built(repo, fake_gh, tmp_path):
    # A dispatch with the `commit` input builds that commit while its head stays the
    # branch tip; the run's title names the built commit (the workflow's run-name).
    built = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    tip = _commit(repo, {"viewer/a.ts": "2"}, "two")
    fake_gh.set_runs([(7, tip, "2026-10-01T00:00:00Z", f"{_TITLE} at {built}")])
    fake_gh.add_tree(7, built)
    got = wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    assert (got.run_id, got.commit) == (7, built)
    assert got.stale_at_head == "viewer inputs moved since: viewer"
    assert [c.commit for c in wta.candidates(repo)] == [built]


def test_the_ancestor_tree_is_the_newest_ancestor_run_whatever_its_inputs(repo, fake_gh, tmp_path):
    old = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    mid = _commit(repo, {"unrelated.txt": "x"}, "two")
    _commit(repo, {"viewer/a.ts": "2"}, "three")  # an input moved at the tip
    fake_gh.set_runs([(1, old, "2026-10-01T00:00:00Z"), (2, mid, "2026-10-02T00:00:00Z")])
    fake_gh.add_tree(1, old)
    fake_gh.add_tree(2, mid)
    got = wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    assert (got.run_id, got.commit) == (2, mid)
    assert got.path == tmp_path / "cache" / mid and (got.path / "share-viewer.html").is_file()
    assert not got.current_at_head
    assert got.stale_at_head == "viewer inputs moved since: viewer"
    # ensure_tree, the gate at HEAD, is unchanged: both are stale for it.
    with pytest.raises(WebTreeUnavailable, match="every candidate is stale"):
        wta.ensure_tree(repo, tmp_path / "cache", _INPUTS)


def test_the_ancestor_tree_says_when_the_gate_reads_it_current_at_head(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    _commit(repo, {"unrelated.txt": "x"}, "two")
    fake_gh.set_runs([(5, sha, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(5, sha)
    got = wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    assert got.current_at_head and got.stale_at_head is None


def test_the_ancestor_tree_skips_a_run_off_the_ancestry_and_one_without_the_artifact(
        repo, fake_gh, tmp_path):
    base = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    expired = _commit(repo, {"unrelated.txt": "x"}, "two")
    _git(repo, "checkout", "-q", "-b", "side")
    side = _commit(repo, {"side.txt": "s"}, "side")
    _git(repo, "checkout", "-q", "main")
    _commit(repo, {"viewer/a.ts": "2"}, "three")
    fake_gh.set_runs([
        (3, side, "2026-10-03T00:00:00Z"),
        (2, expired, "2026-10-02T00:00:00Z"),
        (1, base, "2026-10-01T00:00:00Z"),
    ])
    fake_gh.add_tree(3, side)
    fake_gh.add_tree(1, base)  # run 2's artifact has expired
    got = wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    assert got.commit == base
    assert fake_gh.downloads() == ["2", "1"], "the side run is never downloaded"


def test_the_ancestor_tree_uses_a_verified_cache(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1", "lib/share.rs": "1"}, "one")
    fake_gh.set_runs([(7, sha, "2026-10-01T00:00:00Z")])
    fake_gh.add_tree(7, sha)
    wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
    assert fake_gh.downloads() == ["7"]


def test_no_ancestor_artifact_is_a_named_reason(repo, fake_gh, tmp_path):
    sha = _commit(repo, {"viewer/a.ts": "1"}, "one")
    fake_gh.set_runs([(1, sha, "2026-10-01T00:00:00Z")])
    with pytest.raises(WebTreeUnavailable, match="nothing in retention: no green .* at an ancestor"):
        wta.ancestor_tree(repo, tmp_path / "cache", _INPUTS)
