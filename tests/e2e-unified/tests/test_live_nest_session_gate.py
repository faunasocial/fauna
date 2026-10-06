"""tier_1: the `live_nest` session gate — a session composed ENTIRELY of
`live_nest`-marked tests must not build/start the local `fauna-nest`.

The regression this pins (found live during the first tri-machine multiseat
run, 2026-07-18): `_driver_cache` used to take `nest_instance` + `test_user`
as EAGER fixture params, so every `app`-using test — including live-remote
tests that sign into example.com and never touch a local nest — paid a full
`fauna-nest` build (~15-25 min cold, per machine) for a nest that was then
launched-and-ignored. The fix resolves them lazily and skips them entirely
when every selected test carries `live_nest`.

Four surfaces, each pinned here without spawning pytest or building anything:

  1. `_live_nest_session` — the session-wide predicate (all selected items
     carry the marker; an empty selection is NOT live).
  2. `_driver_cache`'s signature — no eager `nest_instance`/`test_user`
     params; re-adding one silently reverts the whole fix (the closure drag
     is invisible in review), so the signature itself is the contract.
  3. `_session_uses_local_nest` — the autouse primary-mail-domain pin's
     closure scan. Dropping the eager params removes `nest_instance` from
     plain-`app` tests' fixture closures, so the scan must count the
     driver-cache consumers (`app`/`persistent_app`/`fresh_app`) explicitly
     or it would silently stop pinning the primary domain for exactly the
     sessions that had it before.
  4. `_is_real_fixture` / `_real_fixture_closure` — the same scan must NOT
     count a directly-parametrized argname that merely shares a fixture's
     name. `@pytest.mark.parametrize("app", [...])` puts `app` in
     `fixturenames` too, and reading it as a fixture request wedged the whole
     tier_1 `test_app_surface_declarations.py` suite behind a cold nest build
     (2026-08-24). Indirect params must stay counted — the real fixture runs.
"""

import inspect
from types import SimpleNamespace

import pytest

import conftest

pytestmark = pytest.mark.tier_1


class _Item:
    """A collected-item stand-in: markers by name + a fixture closure."""

    def __init__(self, markers=(), fixturenames=()):
        self._markers = set(markers)
        self.fixturenames = tuple(fixturenames)

    def get_closest_marker(self, name):
        return SimpleNamespace(name=name) if name in self._markers else None


def _request(items):
    return SimpleNamespace(session=SimpleNamespace(items=list(items)))


# ── 1. the session predicate ─────────────────────────────────────────────────


def test_all_marked_items_make_a_live_session():
    req = _request([_Item(markers={"live_nest", "tier_3"}) for _ in range(3)])
    assert conftest._live_nest_session(req) is True


def test_one_unmarked_item_disables_live():
    req = _request([
        _Item(markers={"live_nest"}),
        _Item(markers={"tier_2"}),  # a mixed selection keeps the local nest
    ])
    assert conftest._live_nest_session(req) is False


def test_empty_selection_is_not_live():
    assert conftest._live_nest_session(_request([])) is False


# ── 2. no eager nest params on the driver cache ──────────────────────────────


def _fixture_params(fixture_obj):
    """Parameter names of a @pytest.fixture-decorated function, across pytest
    versions (>=8.4 wraps in FixtureFunctionDefinition; older returns the
    function itself)."""
    fn = getattr(fixture_obj, "_get_wrapped_function", None)
    fn = fn() if callable(fn) else inspect.unwrap(fixture_obj)
    return set(inspect.signature(fn).parameters)


def test_driver_cache_declares_no_eager_nest_fixtures():
    params = _fixture_params(conftest._driver_cache)
    assert "nest_instance" not in params and "test_user" not in params, (
        f"_driver_cache eagerly declares {params & {'nest_instance', 'test_user'}} "
        "— that drags a full local fauna-nest build into EVERY app-using "
        "session, including live_nest sessions that never touch it. Resolve "
        "them lazily (request.getfixturevalue) in the non-live branch instead."
    )


# ── 3. the primary-mail-domain pin's closure scan ────────────────────────────


def test_local_nest_scan_counts_driver_cache_consumers():
    # Plain-`app` tests no longer carry nest_instance in their closure, but a
    # NON-live session of them still launches clients against the local nest —
    # the pin must keep firing for them.
    assert conftest._session_uses_local_nest([_Item(fixturenames=("app",))])
    assert conftest._session_uses_local_nest(
        [_Item(fixturenames=("persistent_app",))]
    )
    assert conftest._session_uses_local_nest(
        [_Item(fixturenames=("request", "nest_instance"))]
    )


def test_local_nest_scan_ignores_nestless_sessions():
    # tier_1 / self-contained-docker sessions must stay nest-free (a docker-only
    # CI runner has no rust toolchain — see the self_contained_docker marker).
    assert not conftest._session_uses_local_nest(
        [_Item(fixturenames=("tmp_path", "monkeypatch"))]
    )
    assert not conftest._session_uses_local_nest([])


# ── 4. a parametrized argname is not a fixture request ───────────────────────
#
# `item.fixturenames` carries DIRECTLY-parametrized argnames beside real
# fixture requests, so a bare `name in fixturenames` scan reads
# `@pytest.mark.parametrize("app", [...])` as "this test launches an
# application". Measured 2026-08-24: that collision made the session-scoped
# autouse `_session_primary_mail_domain` resolve `nest_instance` at the first
# test of `tests/test_app_surface_declarations.py`, so a pure tier_1 suite —
# convention 7's own skip-taxonomy proofs — sat behind a cold `fauna-nest`
# build and never ran. The scan must key on the fixturedef, not the name.


class _ParamItem(_Item):
    """An item whose `name` is satisfied by DIRECT parametrization.

    Mirrors pytest's own shape: the argname is in `fixturenames`, and
    `name2fixturedefs` holds a pseudo-fixture whose function lives in pytest's
    module rather than in our conftest.
    """

    def __init__(self, name, other_fixtures=()):
        super().__init__(fixturenames=(*other_fixtures, name))
        pseudo = SimpleNamespace(
            func=SimpleNamespace(__module__="_pytest.python"), baseid=""
        )
        self.callspec = SimpleNamespace(params={name: "tui"})
        self._fixtureinfo = SimpleNamespace(
            name2fixturedefs={
                name: [pseudo],
                # pytest's BUILT-IN fixtures live in `_pytest.*` too, and are
                # real: they must survive the scan (a 3653-item survey caught a
                # module-only discriminator dropping all of them).
                "tmp_path": [
                    SimpleNamespace(
                        func=SimpleNamespace(__module__="_pytest.tmpdir"),
                        baseid="",
                    )
                ],
                "monkeypatch": [
                    SimpleNamespace(
                        func=SimpleNamespace(__module__="_pytest.monkeypatch"),
                        baseid="",
                    )
                ],
            }
        )


class _IndirectItem(_Item):
    """An item whose `name` is parametrized INDIRECTLY — the real fixture runs.

    `indirect=True` puts the argname in `callspec.params` exactly like a direct
    param, which is why `callspec` is the wrong discriminator; the fixturedef
    still points at our own code, and the build must stay counted.
    """

    def __init__(self, name, module="conftest"):
        super().__init__(fixturenames=(name,))
        real = SimpleNamespace(
            func=SimpleNamespace(__module__=module), baseid="tests/conftest.py"
        )
        self.callspec = SimpleNamespace(params={name: "tui"})
        self._fixtureinfo = SimpleNamespace(name2fixturedefs={name: [real]})


def test_a_parametrized_app_argname_does_not_imply_the_local_nest():
    # The exact wedge: test_app_name_resolves_every_app[tui].
    assert not conftest._session_uses_local_nest([_ParamItem("app")])
    assert not conftest._session_uses_local_nest(
        [_ParamItem("app", other_fixtures=("tmp_path",))]
    )
    # …and it stays false for every name the scan watches.
    for name in conftest._LOCAL_NEST_FIXTURE_USERS:
        assert not conftest._session_uses_local_nest([_ParamItem(name)]), name


def test_an_indirectly_parametrized_fixture_still_implies_the_local_nest():
    # indirect=True runs the real fixture, so dropping it here would put a
    # genuine nest build back inside a test's own 900 s clock.
    assert conftest._session_uses_local_nest([_IndirectItem("app")])


def test_a_real_fixture_request_is_unaffected_by_the_discriminator():
    # No _fixtureinfo at all (the fakes above, and any shape we don't know)
    # answers "real" — a spurious prebuild is waste, a missing one is a bound
    # inversion, so the fallback must stay conservative.
    assert conftest._session_uses_local_nest([_Item(fixturenames=("app",))])
    assert conftest._is_real_fixture(_Item(fixturenames=("app",)), "app")


def test_pytest_builtin_fixtures_are_never_mistaken_for_parametrized_names():
    # `tmp_path` / `monkeypatch` / `tmp_path_factory` are REAL fixtures that
    # also live in `_pytest.*`, so a discriminator keyed on the module alone
    # drops them from every closure in the suite — measured across 3653
    # collected items while building this fix. They are absent from
    # `callspec.params`, which is the half that keeps them.
    item = _ParamItem("app", other_fixtures=("tmp_path", "monkeypatch"))
    assert conftest._is_real_fixture(item, "tmp_path")
    assert conftest._is_real_fixture(item, "monkeypatch")
    assert not conftest._is_real_fixture(item, "app")


def test_prebuild_closure_drops_parametrized_argnames_only():
    assert conftest._real_fixture_closure(_ParamItem("app")) == set()
    assert conftest._real_fixture_closure(
        _ParamItem("app", other_fixtures=("tmp_path", "monkeypatch"))
    ) == {"tmp_path", "monkeypatch"}
    assert conftest._real_fixture_closure(_IndirectItem("app")) == {"app"}


# ── 5. no NEW caller may key on a bare name ──────────────────────────────────
#
# The unit tests above pin the two functions that were wrong. They cannot catch
# the way this bug actually returns: a *new* caller scanning `fixturenames` by
# name, exactly as the two old ones did. That is the whole failure class here —
# "a proof that silently does not run" — so the guard has to be structural.
#
# Every closure read in `conftest.py` and `helpers/` must go through
# `fixture_closure`, or be listed below with a reason.

#: (file, function) -> why this read is safe without the discriminator.
_BARE_CLOSURE_READS_ALLOWED = {
    ("conftest.py", "_serialize_live_box"): (
        "reads `request.fixturenames`, not a collected item's — and the name it "
        "looks for (`nest_mode`) is parametrized INDIRECTLY, so its real "
        "fixture runs and the discriminator would answer True anyway."
    ),
    ("conftest.py", "pytest_generate_tests"): (
        "`metafunc.fixturenames` at GENERATE time: parametrization has not "
        "happened yet, so there is no callspec and no pseudo-fixture to "
        "mistake for a request."
    ),
}


def _closure_reads(path):
    """(function name, lineno) for every `fixturenames` read in `path`.

    Both spellings count: the attribute (`item.fixturenames`) and the guarded
    form the original callers used (`getattr(item, "fixturenames", ())`) —
    scanning for only one of them is how a pin like this quietly goes blind.
    """
    import ast

    tree = ast.parse(path.read_text(encoding="utf-8"))
    found, stack = [], []

    class _V(ast.NodeVisitor):
        def visit_FunctionDef(self, node):
            stack.append(node.name)
            self.generic_visit(node)
            stack.pop()

        visit_AsyncFunctionDef = visit_FunctionDef

        def visit_Attribute(self, node):
            if node.attr == "fixturenames":
                found.append((stack[-1] if stack else "<module>", node.lineno))
            self.generic_visit(node)

        def visit_Call(self, node):
            if (
                isinstance(node.func, ast.Name)
                and node.func.id == "getattr"
                and len(node.args) >= 2
                and isinstance(node.args[1], ast.Constant)
                and node.args[1].value == "fixturenames"
            ):
                found.append((stack[-1] if stack else "<module>", node.lineno))
            self.generic_visit(node)

    _V().visit(tree)
    return found


def test_every_closure_read_goes_through_the_discriminator():
    import ast
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    sources = [root / "conftest.py", *sorted((root / "helpers").glob("*.py"))]

    offenders = []
    for path in sources:
        if path.name == "fixture_closure.py":
            continue  # the discriminator itself
        tree = ast.parse(path.read_text(encoding="utf-8"))
        funcs = {
            n.name: n
            for n in ast.walk(tree)
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        for fn_name, lineno in _closure_reads(path):
            if (path.name, fn_name) in _BARE_CLOSURE_READS_ALLOWED:
                continue
            body = ast.unparse(funcs[fn_name]) if fn_name in funcs else ""
            if "is_real_fixture" in body or "real_fixture_closure" in body:
                continue
            offenders.append(f"{path.name}:{lineno} in {fn_name}()")

    assert not offenders, (
        "These read a collected item's fixture closure by NAME:\n  "
        + "\n  ".join(offenders)
        + "\n\n`item.fixturenames` also contains every DIRECTLY-parametrized "
        "argname, so `@pytest.mark.parametrize(\"app\", [...])` reads as a test "
        "that launches an application. That collision made the whole `--tier 1` "
        "sweep resolve `nest_instance` — a full fauna-nest build for a selection "
        "that is nest-free by definition — and wedged "
        "tests/test_app_surface_declarations.py outright.\n"
        "Use helpers.fixture_closure.real_fixture_closure(item) / "
        "is_real_fixture(item, name), or add an entry to "
        "_BARE_CLOSURE_READS_ALLOWED saying why this read is safe."
    )


def test_the_allowlist_does_not_outlive_its_reads():
    """A stale exemption is a hole nobody is looking at."""
    from pathlib import Path

    root = Path(__file__).resolve().parents[1]
    live = {
        (path.name, fn_name)
        for path in [root / "conftest.py", *sorted((root / "helpers").glob("*.py"))]
        for fn_name, _ in _closure_reads(path)
    }
    stale = set(_BARE_CLOSURE_READS_ALLOWED) - live
    assert not stale, (
        f"_BARE_CLOSURE_READS_ALLOWED exempts reads that no longer exist: "
        f"{sorted(stale)} — drop the entries."
    )
