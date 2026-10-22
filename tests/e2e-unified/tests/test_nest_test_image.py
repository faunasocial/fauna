"""The local nest test image's tag is keyed on its source inputs, built once per
key under a machine-wide lock, and old keys are collected.

`image-pipeline.md` § Image tags & channels: an image only for tests is built
locally, never dispatched, and a later test may reuse it (owner ruling
2026-10-19). Reuse is safe only when the tag says what was built — the one fixed
`fauna-nest-test:local` it replaces was reused whatever tree had built it, and
the 2026-10-10 DKIM tier_4 failures were a stale one. These pins hold the key's
contract (same tree → same tag, an input edit → a new tag, a path the build
never sees → no change) and the one-build-per-key lock, with `docker` stubbed.

tier_1: git only, no docker daemon, no build.
"""

import datetime
import subprocess
import threading

import pytest

from helpers import nest_test_image as nti

pytestmark = pytest.mark.tier_1


def _git(repo, *args):
    subprocess.run(["git", "-C", str(repo), *args], check=True,
                   capture_output=True, text=True)


def _tree(root):
    root.mkdir()
    (root / ".dockerignore").write_text("# comment\ndocs/\ntarget/\n")
    (root / ".gitignore").write_text("*.gen\n")
    (root / "Dockerfile").write_text("FROM scratch\nCOPY libs/ libs/\n")
    (root / "libs").mkdir()
    (root / "libs" / "a.rs").write_text("fn a() {}\n")
    (root / "docs").mkdir()
    (root / "docs" / "x.md").write_text("doc\n")
    _git(root, "init", "-q")
    _git(root, "add", "-A")
    _git(root, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
         "commit", "-qm", "init")
    return root


@pytest.fixture()
def repo(tmp_path):
    return _tree(tmp_path / "one")


def test_the_same_tree_keys_the_same_in_another_checkout(repo, tmp_path):
    other = _tree(tmp_path / "two")
    assert nti.source_key(repo) == nti.source_key(other)


def test_an_edit_under_the_context_moves_the_key(repo):
    before = nti.source_key(repo)
    (repo / "libs" / "a.rs").write_text("fn a() { changed(); }\n")
    assert nti.source_key(repo) != before, "an uncommitted edit is an input"


def test_committing_an_edit_keeps_the_key_of_the_edited_tree(repo):
    (repo / "libs" / "a.rs").write_text("fn a() { changed(); }\n")
    dirty = nti.source_key(repo)
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
         "commit", "-qam", "edit")
    assert nti.source_key(repo) == dirty, "the key is the content, not the commit"


def test_a_path_dockerignore_excludes_does_not_move_the_key(repo):
    before = nti.source_key(repo)
    (repo / "docs" / "x.md").write_text("an edited doc\n")
    (repo / "docs" / "new.md").write_text("an untracked doc\n")
    assert nti.source_key(repo) == before


def test_an_untracked_file_in_the_context_moves_the_key(repo):
    before = nti.source_key(repo)
    (repo / "libs" / "b.rs").write_text("fn b() {}\n")
    assert nti.source_key(repo) != before


def test_a_gitignored_file_does_not_move_the_key(repo):
    before = nti.source_key(repo)
    (repo / "libs" / "out.gen").write_text("generated\n")
    assert nti.source_key(repo) == before


def test_a_deleted_file_moves_the_key(repo):
    before = nti.source_key(repo)
    (repo / "libs" / "a.rs").unlink()
    assert nti.source_key(repo) != before


def test_the_dockerfile_itself_is_an_input(repo):
    before = nti.source_key(repo)
    (repo / "Dockerfile").write_text("FROM scratch\n")
    assert nti.source_key(repo) != before


def test_the_tag_is_the_keyed_form(repo):
    nti.local_image_tag.cache_clear()
    tag = nti.local_image_tag(repo)
    assert tag == f"fauna-nest-test:src-{nti.source_key(repo)}"
    assert len(tag.rsplit("-", 1)[1]) == nti.KEY_HEX


def test_no_git_answers_the_unkeyed_tag_rather_than_raising(tmp_path):
    nti.local_image_tag.cache_clear()
    assert nti.local_image_tag(tmp_path) == nti.UNKEYED_TAG


def test_dockerignore_last_match_wins_with_negation():
    rules = [(False, "docs"), (True, "docs/keep.md"), (False, "*.tmp")]
    assert nti.is_ignored("docs/a.md", rules)
    assert not nti.is_ignored("docs/keep.md", rules)
    assert nti.is_ignored("x.tmp", rules)
    assert not nti.is_ignored("libs/docs.rs", rules)


def test_the_real_dockerignore_keeps_what_the_dockerfile_copies():
    """The repository's own `.dockerignore`, through this matcher: every tree
    the Dockerfile COPYs is an input, the excluded ones are not."""
    from pathlib import Path

    root = Path(__file__).resolve().parents[3]
    rules = nti.dockerignore_rules(root)
    for kept in ("libs/fauna-core/Cargo.toml", "bins/fauna-nest/src/main.rs",
                 "apps/fauna-linux/Cargo.toml", "Dockerfile", "Cargo.lock"):
        assert not nti.is_ignored(kept, rules), kept
    for dropped in ("docs/goal/README.md", "tests/e2e-unified/conftest.py",
                    "apps/fauna-android/build.gradle.kts", ".claude/rules/e2e.md"):
        assert nti.is_ignored(dropped, rules), dropped


# ── One build per key ──────────────────────────────────────────────────────


@pytest.fixture()
def helpers(monkeypatch, tmp_path):
    dh = pytest.importorskip("tests.platform.docker.helpers")
    monkeypatch.setattr(nti, "LOCK_DIR", tmp_path / "locks")
    monkeypatch.setattr(nti, "local_image_tag",
                        lambda root: "fauna-nest-test:src-0123456789abcdef")
    monkeypatch.setattr(nti, "collect_old_tags", lambda keep, **kw: [])
    monkeypatch.delenv(nti.OVERRIDE_ENV, raising=False)
    monkeypatch.setenv("FAUNA_ALLOW_NEST_IMAGE_BUILD", "1")
    return dh


def test_two_concurrent_callers_get_one_build(helpers, monkeypatch, tmp_path):
    """Both find the tag absent; the lock lets one build, and the other — which
    waited — finds the tag and reuses it. Without the lock both would build."""
    state = {"present": False, "inspects": 0, "builds": 0}
    guard = threading.Lock()
    both_checked = threading.Event()

    def fake_run(argv, *a, **kw):
        if "inspect" in argv:
            with guard:
                state["inspects"] += 1
                if state["inspects"] >= 2:
                    both_checked.set()
                present = state["present"]
            return subprocess.CompletedProcess(argv, 0 if present else 1,
                                               "2026-10-20T00:00:00Z" if present else "", "")
        if "rev-parse" in argv:
            return subprocess.CompletedProcess(argv, 0, "f" * 40 + "\n", "")
        if "buildx" in argv:
            # Hold the build until the other caller has made its first check,
            # so it is waiting on the lock rather than arriving after the build.
            assert both_checked.wait(30), "the second caller never checked"
            with guard:
                state["builds"] += 1
                state["present"] = True
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError("unexpected subprocess: " + " ".join(argv))

    monkeypatch.setattr(helpers.subprocess, "run", fake_run)
    results = []
    threads = [threading.Thread(target=lambda: results.append(helpers.docker_build(tmp_path)))
               for _ in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(60)
    assert results == ["fauna-nest-test:src-0123456789abcdef"] * 2
    assert state["builds"] == 1, "a second caller must reuse, never rebuild"


def test_the_build_runs_inside_a_build_slot(helpers, monkeypatch, tmp_path):
    slot = tmp_path.joinpath(*helpers.SLOT_SCRIPT)
    slot.parent.mkdir(exist_ok=True)
    slot.write_text("")
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        if "inspect" in argv:
            return subprocess.CompletedProcess(argv, 1, "", "")
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(helpers.subprocess, "run", fake_run)
    helpers.docker_build(tmp_path)
    build = next(c for c in calls if "buildx" in c)
    assert build[1] == str(slot)
    assert build[build.index("--pool") + 1] == "build"
    assert build[build.index("-t") + 1] == "fauna-nest-test:src-0123456789abcdef"


def test_a_named_image_is_used_announced_and_never_built(helpers, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv(nti.OVERRIDE_ENV, "ghcr.io/faunasocial/nest:dev")
    monkeypatch.setattr(helpers, "_override_announced", False)
    calls = []

    def fake_run(argv, *a, **kw):
        calls.append(list(argv))
        return subprocess.CompletedProcess(
            argv, 0, datetime.datetime.now().isoformat(), "")

    monkeypatch.setattr(helpers.subprocess, "run", fake_run)
    assert helpers.docker_build(tmp_path) == "ghcr.io/faunasocial/nest:dev"
    assert not any("buildx" in c for c in calls)
    assert "not this tree's own build" in capsys.readouterr().err


# ── GC ─────────────────────────────────────────────────────────────────────


def test_gc_keeps_the_newest_the_young_the_used_and_the_kept(monkeypatch):
    now = datetime.datetime(2026, 10, 20, tzinfo=datetime.timezone.utc)

    def stamp(days):
        return (now - datetime.timedelta(days=days)).strftime(
            "%Y-%m-%d %H:%M:%S +0000 UTC")

    ages = {f"src-{i:016x}": 10 + i for i in range(8)}
    ages["src-young"] = 1
    listing = "\n".join(f"{t}\t{stamp(d)}" for t, d in ages.items())
    listing += f"\nlocal\t{stamp(90)}"
    removed_calls = []

    def fake_run(argv, *a, **kw):
        if argv[:3] == ["docker", "image", "ls"]:
            return subprocess.CompletedProcess(argv, 0, listing, "")
        if argv[:2] == ["docker", "ps"]:
            return subprocess.CompletedProcess(
                argv, 0, f"fauna-nest-test:src-{6:016x}\nalpine\n", "")
        if argv[:3] == ["docker", "image", "rm"]:
            removed_calls.append(argv[3])
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError(argv)

    keep = f"fauna-nest-test:src-{7:016x}"  # the oldest, but just built
    removed = nti.collect_old_tags(keep, run=fake_run, now=now)
    # Newest four by creation: src-young, then src-0..2. Used: src-6. Kept: src-7.
    assert sorted(removed) == [f"fauna-nest-test:src-{i:016x}" for i in (3, 4, 5)]
    assert removed == removed_calls
    assert "fauna-nest-test:local" not in removed, "a hand-made tag is never collected"


def test_gc_never_raises_without_docker():
    def no_docker(argv, *a, **kw):
        raise FileNotFoundError("docker")

    assert nti.collect_old_tags("fauna-nest-test:src-x", run=no_docker) == []


def test_gc_constants_are_constants_not_knobs():
    """N and D are code, never an env or flag a run can move."""
    import inspect

    source = inspect.getsource(nti.collect_old_tags)
    assert "environ" not in source and "getenv" not in source
    assert isinstance(nti.GC_KEEP_NEWEST, int) and isinstance(nti.GC_KEEP_DAYS, int)
