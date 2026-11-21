"""A refusal raised while collection is filtered reaches the user as itself.

pytest calls `pytest_collection_finish` from a `finally` around
`pytest_collection_modifyitems`, so a refusal there (the `--release-candidate`
dirty-tree check in `_apply_feature_axis`) used to be followed by the finish hook
running over the UNFILTERED items — the `-m` deselection never ran — and the
drafts-window collision check's error replaced the real one: on the 0.1.7
candidate run, part 2 exited 4 under a message that named the wrong cause
(release-table.md § Release-candidate run). `conftest._CollectionRefusalNote`
notes the refusal; the finish hook then does nothing.
"""

from __future__ import annotations

import types

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)


class _Config:
    def __init__(self):
        self.stash = pytest.Stash()
        self.option = types.SimpleNamespace(collectonly=False)


def _drive_wrapper(config, raise_exc):
    """Run the wrapper around an inner hook that raises `raise_exc` (or returns)."""
    gen = conftest._CollectionRefusalNote().pytest_collection_modifyitems(config, [])
    next(gen)
    if raise_exc is None:
        with pytest.raises(StopIteration):
            gen.send(None)
        return None
    with pytest.raises(type(raise_exc)) as caught:
        gen.throw(raise_exc)
    return caught.value


def test_a_refused_collection_is_noted_and_re_raised_unchanged():
    config = _Config()
    refusal = pytest.UsageError("--release-candidate refused: the working tree is not clean")
    assert _drive_wrapper(config, refusal) is refusal
    assert config.stash[conftest._COLLECTION_REFUSED] is refusal


def test_a_collection_that_passes_notes_nothing():
    config = _Config()
    _drive_wrapper(config, None)
    assert conftest._COLLECTION_REFUSED not in config.stash


def test_after_a_refusal_the_finish_hook_does_not_mask_it(monkeypatch):
    """The dirty-tree refusal stays the run's answer: the finish hook runs no
    drafts check (which would raise its own error over the unfiltered items),
    warms nothing and takes no lane."""
    called = []

    def drafts_check(session):
        called.append("drafts")
        raise pytest.UsageError("drafts_autosave_window_ms re-times the drafts debounce")

    monkeypatch.setattr(conftest, "_refuse_drafts_window_collision", drafts_check)
    monkeypatch.setattr(conftest, "_phase", lambda *a, **k: called.append("phase"))
    config = _Config()
    _drive_wrapper(config, pytest.UsageError("the working tree is not clean"))
    session = types.SimpleNamespace(config=config, items=[object()] * 15041)
    assert conftest.pytest_collection_finish(session) is None
    assert called == []


def test_without_a_refusal_the_finish_hook_still_runs_the_drafts_check(monkeypatch):
    """The early return is the refusal's alone: an ordinary collection still meets
    the drafts check."""
    def drafts_check(session):
        raise pytest.UsageError("drafts collision")

    monkeypatch.setattr(conftest, "_refuse_drafts_window_collision", drafts_check)
    monkeypatch.setattr(conftest, "_phase", lambda *a, **k: None)
    session = types.SimpleNamespace(config=_Config(), items=[])
    with pytest.raises(pytest.UsageError, match="drafts collision"):
        conftest.pytest_collection_finish(session)


def test_after_a_collection_error_the_finish_hook_warms_nothing(monkeypatch):
    """A module that failed to import ends the run before its first test
    (pytest's run loop interrupts on a collection error), so the finish hook
    warms nothing and takes no lane: the error is the run's answer, in seconds.
    Measured on Windows 2026-11-17: one module-level `import pwd` let a tier_1
    run spend 85 minutes in the prebuild's slot queue and builds, then exit 2
    with no test run — the gate's whole night."""
    called = []
    monkeypatch.setattr(
        conftest, "_refuse_drafts_window_collision", lambda s: called.append("drafts")
    )
    monkeypatch.setattr(conftest, "_prewarm_tier_1_run", lambda s: called.append("prewarm"))
    monkeypatch.setattr(conftest, "_admit_tier_1_run", lambda: called.append("admit"))
    monkeypatch.setattr(conftest, "_phase", lambda *a, **k: None)
    config = _Config()
    config.option.continue_on_collection_errors = False
    session = types.SimpleNamespace(config=config, items=[object()] * 10827, testsfailed=1)
    assert conftest.pytest_collection_finish(session) is None
    assert called == []


def test_with_continue_on_collection_errors_the_finish_hook_still_runs(monkeypatch):
    """`--continue-on-collection-errors` runs the tests that did import, so their
    prebuild still happens: the early return is the interrupted run's alone."""
    def drafts_check(session):
        raise pytest.UsageError("drafts collision")

    monkeypatch.setattr(conftest, "_refuse_drafts_window_collision", drafts_check)
    monkeypatch.setattr(conftest, "_phase", lambda *a, **k: None)
    config = _Config()
    config.option.continue_on_collection_errors = True
    session = types.SimpleNamespace(config=config, items=[], testsfailed=1)
    with pytest.raises(pytest.UsageError, match="drafts collision"):
        conftest.pytest_collection_finish(session)


def test_the_note_is_registered_as_a_plugin_by_configure(request):
    """A module attribute of the same hook name would be shadowed by conftest's own
    `pytest_collection_modifyitems`, so the note is a plugin `pytest_configure`
    registers — read off the live run hosting this test."""
    plugin = request.config.pluginmanager.get_plugin(conftest._COLLECTION_REFUSAL_PLUGIN)
    assert isinstance(plugin, conftest._CollectionRefusalNote)
