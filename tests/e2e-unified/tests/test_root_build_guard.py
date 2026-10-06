"""Proofs for the sudo-run guard that keeps root-owned artifacts out of the checkout.

The mechanism under test is `helpers/root_build_guard.py`, wired into the macOS
installer suite (`tests/platform/macos/test_installer.py`): the `.pkg` fixture
refuses to build while running as root, and a session-scoped guard walks the
checkout's build trees before and after the run.

**Red-on-regression against the way this actually broke.** The refusal shipped
2026-06-28 and still let ~3.8G of root-owned release artifacts accumulate,
because it had no test of its own and nothing checked the outcome it was
supposed to produce. Seven weeks later a human found them and removed them with
`sudo` by hand. So these proofs cover both halves separately:

  * the *decision* — refusing must depend on BOTH being root and needing to
    build. A refusal keyed on `is_root` alone would break the intended flow
    (build unprivileged, then `sudo pytest` against the fresh cache), and a
    version keyed on the cache alone would not refuse at all.
  * the *outcome* — `root_owned_paths` must actually find a root-owned file.
    This is the half that was missing entirely, and it is the one that fires no
    matter which code path did the writing.

Everything here is pure logic and a `tmp_path` tree (tier_1), so it runs on
every machine — the guard is edited from any checkout, and a proof that only
ran under a real `sudo` on macOS would be no proof at all.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

E2E_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(E2E_ROOT))

import helpers.root_build_guard as guard  # noqa: E402
from helpers.root_build_guard import (  # noqa: E402
    GUARDED_TREES,
    MAX_REPORTED,
    refuse_root_build,
    root_droppings_message,
    root_owned_paths,
)


# ── the decision ───────────────────────────────────────────────────────────

def test_refuses_only_when_root_and_the_build_would_actually_run():
    """Root + stale cache is the one case that must refuse."""
    assert refuse_root_build(is_root=True, cache_is_fresh=False) is True


def test_root_with_a_fresh_cache_is_allowed_through():
    """The intended flow: build unprivileged, then `sudo pytest` the install legs.

    A refusal keyed on `is_root` alone would make the sudo-gated tests
    unrunnable, which is how a guard gets deleted instead of fixed.
    """
    assert refuse_root_build(is_root=True, cache_is_fresh=True) is False


@pytest.mark.parametrize("cache_is_fresh", [True, False])
def test_a_normal_user_is_never_refused(cache_is_fresh):
    """Only root can create the problem, so only root is ever stopped."""
    assert refuse_root_build(is_root=False, cache_is_fresh=cache_is_fresh) is False


# ── the outcome ────────────────────────────────────────────────────────────

def _tree(root: Path, rel: str, *, name: str = "artifact.o") -> Path:
    d = root / rel
    d.mkdir(parents=True, exist_ok=True)
    f = d / name
    f.write_text("x")
    return f


#: The invoking (non-root) user every un-faked path is owned by.
_USER_UID = 1000


def _fake_ownership(monkeypatch, root_owned: set[str]) -> None:
    """Every path owned by an ordinary user, except `root_owned` (uid 0).

    Faking BOTH directions, not only the root one, keeps the proof honest on
    every box: on Windows `st_uid` is always 0, so leaving the rest real would
    read the whole tree as root-owned. The POSIX switch is forced on for the
    same reason -- the walk's logic is what is under test, on every machine.
    """
    owned = {os.path.abspath(p) for p in root_owned}
    real_lstat = os.lstat

    def fake_lstat(path, *args, **kwargs):
        st = real_lstat(path, *args, **kwargs)
        uid = 0 if os.path.abspath(path) in owned else _USER_UID
        return os.stat_result(tuple([st.st_mode, st.st_ino, st.st_dev, st.st_nlink, uid] + list(st)[5:]))

    monkeypatch.setattr(guard, "HAS_POSIX_OWNERSHIP", True)
    monkeypatch.setattr(os, "lstat", fake_lstat)


def test_a_clean_checkout_reports_nothing(tmp_path, monkeypatch):
    _tree(tmp_path, "target/release")
    _tree(tmp_path, "apps/fauna-apple/.build")
    _fake_ownership(monkeypatch, set())
    assert root_owned_paths(str(tmp_path)) == []


def test_a_box_without_posix_ownership_reports_nothing(tmp_path, monkeypatch):
    """Windows: `st_uid` is 0 for EVERY file, and there is no root to blame.

    Measured 2026-09-22, the first whole-directory tier_1 run on Windows: the
    unguarded walk reported every file of a clean checkout as root-owned.
    """
    _tree(tmp_path, "target/release")
    real_lstat = os.lstat

    def all_uid_zero(path, *args, **kwargs):
        st = real_lstat(path, *args, **kwargs)
        return os.stat_result(tuple([st.st_mode, st.st_ino, st.st_dev, st.st_nlink, 0] + list(st)[5:]))

    monkeypatch.setattr(os, "lstat", all_uid_zero)
    monkeypatch.setattr(guard, "HAS_POSIX_OWNERSHIP", False)
    assert root_owned_paths(str(tmp_path)) == []
    monkeypatch.setattr(guard, "HAS_POSIX_OWNERSHIP", True)
    assert root_owned_paths(str(tmp_path)), "the fake must make the POSIX walk fire"


def test_the_posix_switch_matches_the_platform():
    assert guard.HAS_POSIX_OWNERSHIP is (sys.platform != "win32")


def test_missing_build_trees_are_not_an_error(tmp_path):
    """A checkout that has never been built has no `target/` at all."""
    assert root_owned_paths(str(tmp_path)) == []


#: Spelled out rather than read from `GUARDED_TREES`, deliberately. Parametrizing
#: over the constant under test makes deleting a tree delete its own test case —
#: measured: that mutant survived, and this list is what kills it. (Same trap as
#: the `OpaqueDriver` note in `test_app_surface_declarations.py`.)
EXPECTED_TREES = ("target", os.path.join("apps", "fauna-apple", ".build"))


def test_the_guard_covers_every_tree_a_root_build_writes_into():
    """cargo's `target/` and SwiftPM's `.build` — both, or the guard has a hole.

    `build.sh` runs `just mac-app release`, which is cargo *and* xcodebuild, so
    a guard watching only `target/` would report a clean checkout while SwiftPM
    droppings sat next to it.
    """
    assert set(GUARDED_TREES) == set(EXPECTED_TREES), (
        "GUARDED_TREES changed. If a build tree was added or moved, update "
        f"EXPECTED_TREES here in the same commit: {GUARDED_TREES}"
    )


@pytest.mark.parametrize("tree", EXPECTED_TREES)
def test_a_root_owned_file_in_any_guarded_tree_is_found(tmp_path, tree, monkeypatch):
    """Every tree the guard claims to cover must actually be walked.

    `st_uid` is faked rather than requiring a real root-owned file, so this
    proof runs unprivileged on every machine — the guard's own logic is what is
    under test, not the OS's ownership bookkeeping.
    """
    target = _tree(tmp_path, os.path.join(tree, "sub"))
    _fake_ownership(monkeypatch, {str(target)})
    found = root_owned_paths(str(tmp_path))
    assert found == [os.path.relpath(str(target), str(tmp_path))], (
        f"a root-owned file under {tree!r} was not reported: {found}"
    )


def test_the_walk_is_bounded(tmp_path, monkeypatch):
    """3.8G of droppings must not become 3.8G of failure output."""
    made = [_tree(tmp_path, "target/release", name=f"a{i}.o") for i in range(MAX_REPORTED + 5)]
    _fake_ownership(monkeypatch, {str(p) for p in made})
    assert len(root_owned_paths(str(tmp_path))) == MAX_REPORTED


def test_the_failure_message_tells_a_human_how_to_get_unstuck(tmp_path):
    """A guard whose message does not name the fix just moves the confusion."""
    msg = root_droppings_message(str(tmp_path), ["target/release/fauna-sync"], when="after the run")
    assert "target/release/fauna-sync" in msg
    assert "after the run" in msg
    assert "-user root -delete" in msg, "the message must carry the removal command"
    assert "fauna-apple-ffi" in msg, "the out-of-checkout cache must be named too"
