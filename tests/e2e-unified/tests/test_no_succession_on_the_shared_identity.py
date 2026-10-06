"""No test may run the succession ceremony against the SHARED session identity.

``test_user`` is ``scope="session"``: one registered identity backs
``logged_in_app`` and every fixture derived from it, for the whole run. The
identity-succession ceremony is irreversible and, in the same nest transaction,
**revokes every bearer of the predecessor** (`actions/settings.py`
``succeed_identity_with_held_kit``). So a ceremony driven against that shared
identity signs out every *later* test in the run: each one's ``_login_app_as``
patches a session whose secret the nest now refuses, the app routes to the
identity-import flow instead of coming online, and the login seam's connection
barrier burns its full ``ONLINE_BUDGET_S`` before erroring. Nothing in that
failure names the test that caused it.

**This is not hypothetical — it is what happens, measured.** In the 3 347-test
docker sweep of 2026-08-30, ``test_the_successors_conversations_unlock_without_a_user_command``
took ``logged_in_app`` and ran the ceremony at outcome 467. Its own sibling
``test_the_successors_unsent_drafts_come_back`` — the very next barrier-exposed
test — errored at 470, and from outcome 492 on, **every** module that logs in
died: 65-92 % of barrier-exposed outcomes, for the remaining fourteen hours of
the run, never recovering. Modules that never log in interleaved green
throughout, which is what made the pattern read as a load spike for three
sessions of bisection. The cascade also masks real defects: eight tests that
FAIL in a short run were never reached at all, so a sweep's error count is an
undercount of product defects, not merely noise.

**Why a test and not a docstring.** The rule was already written down in three
places before that sweep — ``ungranted_app``'s rationale, ``succeedable_app``'s
docstring ("Running that against the shared user would break every later test in
the run"), and a sibling test's own explanation of why it uses a dedicated actor
— and was violated twice in the same file regardless. A prose invariant with no
mechanical enforcement is one a future session cannot see. The dedicated
fixtures (``ungranted_app``, ``succeedable_app``) already exist and cost nothing:
this test only makes using them non-optional.

**Why only succession, when account deletion would strand the identity too.**
The whole class was swept, not just the instance that bit: succession and
account deletion are the two acts that can end a shared identity, and exactly
one test reaches a deletion while holding the shared one —
``test_pending_actions.py::test_a_queued_account_delete_is_visible_and_cancellable``.
That one is safe **by construction and deliberately**, which is the distinction
worth keeping: the nest holds a queued deletion for a multi-day window and the
test cancels it before it ends, so the account is never at risk inside the
test's lifetime, and it asserts up front that no leftover queued delete
pre-exists it. Succession has no such window — it is immediate and irreversible
the moment the ceremony's confirm lands. So the rule is scoped to the ceremony
on purpose; widening it to deletion would red a legitimate test and teach the
next session to weaken the rule rather than trust it. Re-run the sweep before
assuming that still holds: it is an AST walk over ``tests/`` for the destructive
actions, cross-referenced against each test's fixture params.

The shared-identity fixture set is **derived from conftest.py**, never
hand-listed — the same lesson ``test_dedicated_mail_nest_consumers_rebind.py``
records, where a hand-written list missed a third fixture on the same path. Any
future fixture that hands out the session ``test_user`` is covered the day it is
written.

tier_1: pure source analysis, no nest, no driver, no app.
"""

from __future__ import annotations

import ast
import pathlib

import pytest

pytestmark = [pytest.mark.tier_1]

_E2E_ROOT = pathlib.Path(__file__).resolve().parent.parent
_CONFTEST = _E2E_ROOT / "conftest.py"
_TESTS_DIR = _E2E_ROOT / "tests"
_ACTIONS = _E2E_ROOT / "actions" / "settings.py"

# The session-scoped identity itself. Everything else is derived.
_SESSION_USER = "test_user"

# The one cross-app action that runs the ceremony. Every app reaches the
# ceremony through it, so a module-local helper is only a ceremony because it
# calls this.
_CEREMONY_ACTION = "succeed_identity_with_held_kit"

# A fixture that mints its own actor is NOT handing out the shared identity,
# however it was parameterized — this is what keeps `ungranted_app` and
# `succeedable_app` (both of which take `app`, then log in as a fresh actor)
# out of the derived set.
_MAKES_ITS_OWN_ACTOR = "_make_user"


def _fn_calls(node: ast.AST) -> set[str]:
    """Every simple name called anywhere inside ``node``."""
    names: set[str] = set()
    for sub in ast.walk(node):
        if isinstance(sub, ast.Call):
            fn = sub.func
            name = getattr(fn, "id", None) or getattr(fn, "attr", None)
            if name:
                names.add(name)
    return names


def _module(path: pathlib.Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8", errors="replace"))


def _is_fixture(node: ast.FunctionDef) -> bool:
    for dec in node.decorator_list:
        target = dec.func if isinstance(dec, ast.Call) else dec
        if getattr(target, "attr", None) == "fixture":
            return True
    return False


def _shared_identity_fixtures() -> set[str]:
    """Fixtures that hand a test the SHARED session identity.

    Derived, not listed: a fixture is shared if it logs an app in as
    ``test_user`` (directly, by taking it as a parameter), or if it takes a
    shared fixture and hands it on. A fixture that calls ``_make_user`` re-points
    the app at a dedicated actor and drops out of the set however it was reached.
    """
    fixtures: dict[str, tuple[set[str], set[str]]] = {}
    for node in _module(_CONFTEST).body:
        if isinstance(node, ast.FunctionDef) and _is_fixture(node):
            params = {a.arg for a in node.args.args}
            fixtures[node.name] = (params, _fn_calls(node))

    shared = {_SESSION_USER}
    changed = True
    while changed:
        changed = False
        for name, (params, calls) in fixtures.items():
            if name in shared or _MAKES_ITS_OWN_ACTOR in calls:
                continue
            if params & shared:
                shared.add(name)
                changed = True
    return shared


def _ceremony_names(tree: ast.Module) -> set[str]:
    """``_CEREMONY_ACTION`` plus this module's own helpers that reach it.

    One level of indirection is what the real code uses (``_run_succession``);
    the closure below keeps that honest without hard-coding the helper's name.
    """
    names = {_CEREMONY_ACTION}
    helpers = {
        node.name: _fn_calls(node)
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and not node.name.startswith("test_")
    }
    changed = True
    while changed:
        changed = False
        for helper, calls in helpers.items():
            if helper not in names and calls & names:
                names.add(helper)
                changed = True
    return names


def _violations() -> list[str]:
    shared = _shared_identity_fixtures()
    found: list[str] = []
    for path in sorted(_TESTS_DIR.glob("test_*.py")):
        try:
            tree = _module(path)
        except SyntaxError:
            continue
        ceremonies = _ceremony_names(tree)
        for node in tree.body:
            if not isinstance(node, ast.FunctionDef):
                continue
            if not node.name.startswith("test_"):
                continue
            params = {a.arg for a in node.args.args}
            taken = sorted(params & shared)
            run = sorted(_fn_calls(node) & ceremonies)
            if taken and run:
                found.append(
                    f"{path.name}::{node.name} takes {taken} (the SHARED session "
                    f"identity) and runs {run}"
                )
    return found


def test_the_ceremony_never_runs_against_the_shared_session_identity():
    """The authoring rule, checked at collection time on every app."""
    violations = _violations()
    assert not violations, (
        "these tests succeed the identity every LATER test in the run logs in "
        "as, which signs the whole run out — every subsequent module then burns "
        "the 60 s connection barrier and ERRORs, and the failure names none of "
        "this:\n  " + "\n  ".join(violations) + "\n\n"
        "Use a DEDICATED actor instead: `ungranted_app` (app only) or "
        "`succeedable_app` (app plus that actor's own credentials, for a test "
        "that must name the identity to a nest-side read). Both already exist "
        "and cost no extra nest."
    )


def test_the_derivation_actually_finds_the_shared_fixtures():
    """The scan above is worthless if its derived set is empty or too narrow.

    Pins the two ends: the session identity itself, and `logged_in_app` — the
    fixture that hands it to most of the suite. Without this, a refactor that
    renamed either would leave the rule silently passing on every file.
    """
    shared = _shared_identity_fixtures()
    assert _SESSION_USER in shared
    assert "logged_in_app" in shared, (
        f"`logged_in_app` must derive as a shared-identity fixture; got {sorted(shared)}"
    )
    # And the dedicated ones must NOT, or the rule would refuse the correct fix.
    assert "ungranted_app" not in shared
    assert "succeedable_app" not in shared


def test_the_ceremony_action_is_where_this_rule_thinks_it_is():
    """A rename of the action would make the scan vacuous — fail instead."""
    src = _ACTIONS.read_text(encoding="utf-8")
    assert f"def {_CEREMONY_ACTION}(" in src, (
        f"{_ACTIONS.name} no longer defines `{_CEREMONY_ACTION}`; this rule "
        "scans for it by name and would pass vacuously. Update _CEREMONY_ACTION."
    )
    assert "refuse_ceremony_on_shared_identity" in src, (
        f"the runtime backstop is gone from {_ACTIONS.name}: the source scan "
        "above cannot see a test that points the app at the shared identity "
        "through a raw `set_state` patch, which is the case that guard covers."
    )


class _FakeDriver:
    """The one thing the guard reads, and a way to make the read fail."""

    def __init__(self, actor=None, raises=False):
        self._actor = actor
        self._raises = raises

    def get_state(self, path):
        if self._raises:
            raise RuntimeError("no agent")
        return self._actor if path == "session.actor_id" else None


def test_the_runtime_backstop_refuses_a_shared_actor_and_fails_open():
    """The backstop: refuse a registered shared actor, never anything else.

    Fails open on purpose — an app publishing no session block, or a read that
    raises, must not have its ceremony refused. A guard that could block a
    legitimate ceremony on a missing observable would be worse than the bug it
    exists to catch, because the source scan above already covers the shape
    that actually gets written.
    """
    from helpers import shared_identity

    before = shared_identity.shared_actors()
    shared_identity._forget_all_for_test()
    try:
        shared_identity.remember_shared_actor("AB" * 32)

        with pytest.raises(shared_identity.SharedIdentityCeremony) as exc:
            shared_identity.refuse_ceremony_on_shared_identity(
                _FakeDriver("ab" * 32), ceremony="succeed_identity_with_held_kit"
            )
        # The message has to carry the fix, not just the refusal: whoever hits
        # this is mid-ceremony and has no reason to know the fixture exists.
        assert "ungranted_app" in str(exc.value)

        # A dedicated actor, an app that publishes nothing, and a driver whose
        # read raises all proceed.
        for driver in (_FakeDriver("cd" * 32), _FakeDriver(None),
                       _FakeDriver(raises=True)):
            shared_identity.refuse_ceremony_on_shared_identity(
                driver, ceremony="succeed_identity_with_held_kit"
            )
    finally:
        shared_identity._forget_all_for_test()
        for actor in before:
            shared_identity.remember_shared_actor(actor)
