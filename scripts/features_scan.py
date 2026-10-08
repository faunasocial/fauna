#!/usr/bin/env python3
"""The static view of the e2e tree the catalog lint needs: node ids, `feature`
markers, and whether a test launches an app driver.

Separate from `features_catalog.py` because that module's job is the *format* — it
reads a directory of pages and a directory of ledger files and knows nothing about
the test tree. This one is the other half of the two-way discoverability the catalog
promises (`docs/goal/architecture/feature-catalog.md` § The marker): the contract is
feature -> test, the marker is test -> feature, and the lint compares them.

Parse-only, stdlib, `ast` — never an import and never a pytest collection. That is
deliberate and inherited: the hand feature matrix's gate (retired 2026-08-27 with
the matrix it derived) learned that a committed artifact derived by pytest
*collection* differs per machine (which apps are installed changes what collects),
so anything a merge gate compares must come from a parse instead (§ Retiring the
hand matrix).
"""

from __future__ import annotations

import ast
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
E2E = REPO / "tests" / "e2e-unified"

#: One home for "which fixture launches an app UI". The set was born in the hand
#: feature matrix's derivation script and moved here before that script retired
#: (2026-08-27, `feature-catalog.md` § Retiring the hand matrix); the argument it
#: made — that a test's app-driving is decided by which fixtures it requests, never
#: by its markers — survives as rule 4 of the catalog lint.
#:
#: These are the DOORS — the fixtures that obtain a driver themselves. Every other
#: app-launching fixture is derived: `ungranted_app(request, app, nest_instance)`,
#: `seeded_media_app(request, app, …)`, a test module's own `two_account_app(app)`,
#: and so on — and `app_launching_fixtures()` finds those by transitive closure over
#: the fixture definitions (`conftest.py` + the module under scan), so a derived
#: fixture never has to be listed by hand. A new DOOR — a fixture that launches a
#: driver without requesting one of these — still belongs here; until it is added,
#: files using it read as nest-side and cannot back a green cell, which is the safe
#: direction to be wrong in.
#: The app markers a test can carry, and the whole vocabulary of them. Pinned BY
#: EQUALITY to `features_catalog.APPS` and to the run's own `conftest._KNOWN_APPS`
#: (tests/scripts/test_features_catalog.py) rather than restated as a third list.
#:
#: ⚠ This is a DIFFERENT question from the doors above, and the two must not be
#: conflated. `APP_LAUNCHING_FIXTURES` answers *does this test drive an app at all* —
#: decided by the fixtures it requests, never by its markers (rule 4). This answers
#: *which columns can it ever speak for* — and only the markers (with a client-set
#: parametrization, `SECOND_APP_FIXTURES` below) know that, because they are exactly
#: what the run's app axis deselects on (`conftest.py`'s `marker_platforms`
#: check: a marked test is dropped when none of its apps was selected, and an unmarked
#: one is kept for every app). A cell's whole-set rule is read per column
#: (`feature-catalog.md` § Cell semantics, "for that app"), so the scope of a column
#: is the citations that could witness THAT column — never all seven apps' witnesses,
#: which no single machine can even collect.
APP_MARKERS = frozenset({"web", "linux", "windows", "macos", "ios", "android", "tui"})

#: The markers are not the app axis's only input: conftest's deselection ALSO keeps a
#: client-parametrized item only when its client was selected (`_parametrized_clients`
#: — a real fixture, parametrized with app names), and on a column outside the set
#: pytest collects a lone `[NOTSET]` placeholder that skips and records nothing. So a
#: test restricted by parametrization (the launch-routing smoke's
#: `_clients(*AUTOSTART_APPS)`, a module's `@pytest.fixture(params=_supported())`) can
#: speak only for its set, exactly as if marked — and read as unmarked it made its
#: cell `unrun`, *a run can close this*, on columns no run could ever stamp
#: (`feature-catalog.md` § Cell semantics). `client_param_sets` reads it by `ast`.
#:
#: The one real-fixture parametrization that does NOT narrow the column: a seat that
#: launches a SECOND app beside the run's own `app`, which still speaks for its
#: column. Pinned by equality to `conftest._REAL_SECOND_APP_FIXTURES`
#: (tests/scripts/test_features_catalog.py).
SECOND_APP_FIXTURES = frozenset({
    "folder_share_owner_app",
    "folder_share_recipient_app",
    "folder_share_stranger_app",
    # The Media read witnesses' member seat (tui external-open / web download).
    "media_member",
    # The shared-and-served WebDAV journey's member seat
    # (`test_webdav_shared_set.py`).
    "served_share_member",
})

APP_LAUNCHING_FIXTURES = frozenset({
    "app",
    "logged_in_app",
    "persistent_app",
    "fresh_app",
    "admin_app",
    "folder_share_owner_app",
    "folder_share_recipient_app",
    "folder_share_stranger_app",
})


#: The other way a test module drives an app: it constructs drivers ITSELF, through
#: one of the harness's driver factories, instead of requesting a fixture. Read
#: module-wide, exactly like the playwright rule — a module that imports the
#: factory is a module whose fixtures launch apps (`pin_env`, `launch_harness`,
#: `tray_app`, the account-switcher suites, the seat rounds). `(module, name)`
#: pairs of `from <module> import <name>`.
DRIVER_FACTORY_IMPORTS = frozenset({
    ("drivers", "create_driver"),
    ("common.launch_harness", "make_launch_harness"),
    ("helpers.sync_seats", "start_seats"),
    ("helpers.sync_seats", "make_seat"),
    ("helpers.room_seats", "launch_seat"),
    # The room journeys' shared fixture: three launched seats, adopted by
    # import (a module that imports it drives the app under test).
    ("helpers.room_seats", "room_seats"),
    # Its two-seat twin (a founder and a member, no witness).
    ("helpers.room_seats", "room_seat_pair"),
    ("helpers.skew_client", "launch_on_home"),
    ("helpers.skew_client", "launch_build"),
    # The directory-driven custody journeys' shared launcher (the feeder and
    # audit-floor suites).
    ("helpers.directory_launch", "launch_app_with_directory"),
    # The tui sealed-store journeys' shared launcher (the create/unlock arc and
    # the re-key arc). Lifted out of the first journey's test module 2026-09-26:
    # a driver obtained through a SIBLING TEST MODULE's helper is a door this
    # closure cannot see, and the re-key journey read as launching no app.
    ("helpers.tui_headless_store", "headless_launch"),
})

#: The module form of the same imports — `from helpers import sync_seats`, then
#: `sync_seats.start_seats(...)` (`test_filesync_seats.py`) — drives an app
#: exactly as the name import does.
_DRIVER_FACTORY_MODULES = frozenset(
    module for module, _ in DRIVER_FACTORY_IMPORTS if "." in module)


def _is_fixture_decorator(dec: ast.AST) -> bool:
    target = dec.func if isinstance(dec, ast.Call) else dec
    if isinstance(target, ast.Attribute):
        return target.attr == "fixture"
    return isinstance(target, ast.Name) and target.id == "fixture"


def _is_autouse_fixture(dec: ast.AST) -> bool:
    if not (isinstance(dec, ast.Call) and _is_fixture_decorator(dec)):
        return False
    return any(kw.arg == "autouse" and isinstance(kw.value, ast.Constant) and kw.value.value is True
               for kw in dec.keywords)


def fixture_definitions(tree: ast.AST) -> dict:
    """`{fixture name: the argument names it requests}` for every `@pytest.fixture`
    (or bare `@fixture`) function in `tree`, at any nesting level."""
    out: dict = {}
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        if any(_is_fixture_decorator(d) for d in node.decorator_list):
            args = list(node.args.args) + list(node.args.kwonlyargs)
            out[node.name] = {a.arg for a in args}
    return out


def app_launching_fixtures(*definition_maps: dict, doors: frozenset = APP_LAUNCHING_FIXTURES) -> frozenset:
    """The transitive closure: `doors`, plus every fixture that requests (directly
    or through other fixtures) one of them. Iterates to a fixed point, so the order
    of definitions never matters."""
    launching = set(doors)
    definitions: dict = {}
    for defs in definition_maps:
        definitions.update(defs)
    changed = True
    while changed:
        changed = False
        for name, requested in definitions.items():
            if name not in launching and requested & launching:
                launching.add(name)
                changed = True
    return frozenset(launching)


def conftest_fixtures(root: Path) -> dict:
    """The fixture definitions of `root/conftest.py`, or nothing when there is none
    (the lint's own tests build trees with no conftest at all)."""
    conftest = root / "conftest.py"
    if not conftest.exists():
        return {}
    source = conftest.read_text(encoding="utf-8", errors="replace")
    return fixture_definitions(ast.parse(source, filename=str(conftest)))


@dataclass
class TestFacts:
    """One `test_*` function, as the tree spells it."""

    node_id: str            # repo-relative: `tests/e2e-unified/tests/test_x.py::test_y`
    path: Path
    line: int
    features: tuple = ()    # the slugs its `@pytest.mark.feature(...)` names
    drives_app: bool = False
    #: The app columns this test can ever witness — its `@pytest.mark.<app>` marks,
    #: module, class and function level folded together, intersected with any
    #: client-set parametrization it carries (`client_param_sets`). EMPTY means
    #: unrestricted, which means *every* column: such a test is parametrized over
    #: whichever apps the run selected. Never a hand list; see `APP_MARKERS`.
    apps: tuple = ()
    #: Its marks and its client-set parametrization share no app, so no run can ever
    #: select it. `apps` cannot say *no column*, so it keeps the marks and this flag
    #: is what a pin reads to make the contradiction loud.
    contradictory: bool = False

    @property
    def rel(self) -> str:
        """The repo-relative file path, taken from the node id rather than recomputed.

        A caller may be linting a fixture tree that is not this repo (the lint's own
        tests build a whole miniature world in `tmp_path`), so a `relative_to(REPO)`
        here would raise on exactly the inputs that prove the rules work.
        """
        return self.node_id.split("::", 1)[0]


def _mark_calls(node: ast.AST, name: str) -> list:
    """Every `@pytest.mark.<name>(...)` / `pytest.mark.<name>(...)` call in `node`."""
    found = []
    for sub in ast.walk(node):
        if not isinstance(sub, ast.Call):
            continue
        func = sub.func
        if isinstance(func, ast.Attribute) and func.attr == name:
            owner = func.value
            if isinstance(owner, ast.Attribute) and owner.attr == "mark":
                found.append(sub)
    return found


def _feature_slugs(node: ast.AST) -> list:
    slugs = []
    for call in _mark_calls(node, "feature"):
        for arg in call.args:
            if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
                slugs.append(arg.value)
    return slugs


def _app_marks(node: ast.AST) -> set:
    """Every `pytest.mark.<app>` in `node` — bare attribute or call, either spelling.

    Unlike `_mark_calls` this must see the BARE form, because that is how an app
    marker is almost always written (`@pytest.mark.windows`, no parentheses).
    """
    found = set()
    for sub in ast.walk(node):
        target = sub.func if isinstance(sub, ast.Call) else sub
        if (isinstance(target, ast.Attribute)
                and target.attr in APP_MARKERS
                and isinstance(target.value, ast.Attribute)
                and target.value.attr == "mark"):
            found.add(target.attr)
    return found


def _module_scope(tree: ast.Module) -> tuple:
    """`(constants, functions)` bound at module level — the names a client set may be
    spelled through (`AUTOSTART_APPS = ("windows",)`, `def _clients(*want): …`)."""
    constants: dict = {}
    functions: dict = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 \
                and isinstance(node.targets[0], ast.Name):
            constants[node.targets[0].id] = node.value
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) \
                and node.value is not None:
            constants[node.target.id] = node.value
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            functions[node.name] = node
    return constants, functions


def _app_set(expr: ast.AST, scope: tuple, local: dict | None = None,
             depth: int = 0) -> frozenset | None:
    """The finite set of apps `expr` can evaluate to, or None when the parse cannot
    name one — and None always means *unrestricted*, the status quo, never a guess.

    Reads the idioms the tree spells: literal app-name lists, module constants,
    `*STARRED` constants, a filtering comprehension (`[c for c in want if …]` — a
    filter only ever shrinks, so its source bounds it), and a call to a module
    function whose single `return` is one of those, with its `*want` bound to the
    call's arguments (`_clients(*AUTOSTART_APPS)`, `_supported()`). The run-time
    intersection with `get_available_apps()` is the machine's, not the test's, so a
    bound on the set is exactly the columns the test can ever speak for.
    """
    if depth > 8:
        return None
    local = local or {}
    constants, functions = scope
    if isinstance(expr, ast.Constant):
        return frozenset({expr.value}) if expr.value in APP_MARKERS else None
    if isinstance(expr, (ast.List, ast.Tuple, ast.Set)):
        parts = [_app_set(e, scope, local, depth + 1) for e in expr.elts]
        return None if any(p is None for p in parts) else frozenset().union(*parts)
    if isinstance(expr, ast.Starred):
        return _app_set(expr.value, scope, local, depth + 1)
    if isinstance(expr, ast.Name):
        if expr.id in local:
            return local[expr.id]
        if expr.id in constants:
            return _app_set(constants[expr.id], scope, None, depth + 1)
        return None
    if isinstance(expr, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
        if len(expr.generators) != 1:
            return None
        gen = expr.generators[0]
        if not (isinstance(gen.target, ast.Name) and isinstance(expr.elt, ast.Name)
                and expr.elt.id == gen.target.id):
            return None
        return _app_set(gen.iter, scope, local, depth + 1)
    if isinstance(expr, ast.Call) and isinstance(expr.func, ast.Name) \
            and expr.func.id in functions and not expr.keywords:
        fn = functions[expr.func.id].args
        if fn.posonlyargs or fn.args or fn.kwonlyargs:
            return None
        bound: dict = {}
        if fn.vararg is not None:
            parts = [_app_set(a, scope, local, depth + 1) for a in expr.args]
            if any(p is None for p in parts):
                return None
            bound[fn.vararg.arg] = frozenset().union(*parts)
        elif expr.args:
            return None
        returns = [n for n in ast.walk(functions[expr.func.id]) if isinstance(n, ast.Return)]
        if len(returns) != 1 or returns[0].value is None:
            return None
        return _app_set(returns[0].value, scope, bound, depth + 1)
    return None


def _indirect(call: ast.Call, name: str) -> bool:
    for kw in call.keywords:
        if kw.arg != "indirect":
            continue
        if isinstance(kw.value, ast.Constant):
            return kw.value.value is True
        if isinstance(kw.value, (ast.List, ast.Tuple)):
            return any(isinstance(e, ast.Constant) and e.value == name for e in kw.value.elts)
    return False


def client_param_sets(node: ast.AST, scope: tuple) -> list:
    """Every client set an `@pytest.mark.parametrize` in `node` restricts the test to.

    Only the shape conftest's `_parametrized_clients` deselects on counts: ONE
    argname, parametrized `indirect` (a real fixture runs — a direct parametrization
    is pytest's pseudo-fixture, a plain value however app-like its strings), not a
    `SECOND_APP_FIXTURES` seat, over a set the parse can name.
    """
    sets = []
    for call in _mark_calls(node, "parametrize"):
        if len(call.args) < 2 or not isinstance(call.args[0], ast.Constant) \
                or not isinstance(call.args[0].value, str):
            continue
        name = call.args[0].value.strip()
        if "," in name or name in SECOND_APP_FIXTURES or not _indirect(call, name):
            continue
        found = _app_set(call.args[1], scope)
        if found is not None:
            sets.append(found)
    return sets


def parametrized_names(node: ast.AST) -> set:
    """Every argname an `@pytest.mark.parametrize` in `node` supplies — direct or
    indirect, and each of a comma-separated list. A test that parametrizes a fixture
    itself overrides that fixture's own `params=`, so those never narrow it."""
    names: set = set()
    for call in _mark_calls(node, "parametrize"):
        if call.args and isinstance(call.args[0], ast.Constant) \
                and isinstance(call.args[0].value, str):
            names |= {n.strip() for n in call.args[0].value.split(",") if n.strip()}
        elif call.args and isinstance(call.args[0], (ast.List, ast.Tuple)):
            names |= {e.value for e in call.args[0].elts
                      if isinstance(e, ast.Constant) and isinstance(e.value, str)}
    return names


def fixture_param_sets(tree: ast.Module, scope: tuple) -> dict:
    """`{fixture name: client set}` for every module-level `@pytest.fixture(params=…)`
    whose params the parse can name — the other spelling of the same restriction."""
    out: dict = {}
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) \
                or node.name in SECOND_APP_FIXTURES:
            continue
        for dec in node.decorator_list:
            if not (isinstance(dec, ast.Call) and _is_fixture_decorator(dec)):
                continue
            for kw in dec.keywords:
                if kw.arg == "params":
                    found = _app_set(kw.value, scope)
                    if found is not None:
                        out[node.name] = found
    return out


def _module_facts(tree: ast.AST) -> tuple:
    """`(drives_browser, fixture definitions)` from ONE walk of the module.

    This answers, in a single pass, the three module-level questions `scan_file`
    used to ask with three separate full walks:

      * **Playwright anywhere in the module** — the web app is driven without the
        `app` fixture in the `platform/` suites, so a fixture scan alone would
        miss it;
      * **`from drivers import create_driver`** (or another
        `DRIVER_FACTORY_IMPORTS` pair) anywhere in the module — top level or
        inside a fixture body. Both of those mean the module drives an app, so
        they are ORed into one flag here rather than kept apart; nothing
        downstream ever distinguished them.
      * **`{fixture name: the argument names it requests}`** for every
        `@pytest.fixture` (or bare `@fixture`) function, at any nesting level.

    Why one walk. `ast.walk` over this tree's 757 test modules is the whole cost
    of the catalog lint: three walks per module was 5.2 M `iter_child_nodes`
    calls, ~72 % of a 5.77 s gate on Windows (2026-09-05). The three questions read
    disjoint node types and none of them can affect another's answer, so a
    single pass is the same answer for a third of the traversal.
    """
    browser = False
    fixtures: dict = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            if not browser:
                if (node.module or "").split(".")[0] == "playwright":
                    browser = True
                elif node.module and any(
                        (node.module, alias.name) in DRIVER_FACTORY_IMPORTS
                        or f"{node.module}.{alias.name}" in _DRIVER_FACTORY_MODULES
                        for alias in node.names):
                    browser = True
        elif isinstance(node, ast.Import):
            if not browser and any(a.name.split(".")[0] == "playwright" for a in node.names):
                browser = True
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if any(_is_fixture_decorator(d) for d in node.decorator_list):
                args = list(node.args.args) + list(node.args.kwonlyargs)
                fixtures[node.name] = {a.arg for a in args}
    return browser, fixtures


def _class_autouse_drives_app(cls: ast.ClassDef, launching: frozenset) -> bool:
    """A class whose `autouse=True` fixture requests an app door drives an app in
    every one of its test methods (`class TestDeviceCards: @pytest.fixture(autouse=True)
    def setup(self, logged_in_app, …)`), even though the methods request only `self`."""
    for node in cls.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        if any(_is_autouse_fixture(d) for d in node.decorator_list):
            args = {a.arg for a in list(node.args.args) + list(node.args.kwonlyargs)}
            if args & launching:
                return True
    return False


def scan_file(path: Path, repo: Path = REPO, launching: frozenset | None = None) -> list:
    """`launching` is the app-launching fixture set the tree's conftest yields
    (`scan_tree` computes it once); the module's own fixture definitions are folded
    in here, so a test-local `@pytest.fixture def two_account_app(app)` counts too."""
    source = path.read_text(encoding="utf-8", errors="replace")
    tree = ast.parse(source, filename=str(path))
    rel = path.relative_to(repo).as_posix()
    browser, own_fixtures = _module_facts(tree)
    launching = app_launching_fixtures(
        own_fixtures, doors=launching or APP_LAUNCHING_FIXTURES)

    scope = _module_scope(tree)
    param_fixtures = fixture_param_sets(tree, scope)

    module_slugs: list = []
    module_apps: set = set()
    module_params: list = []
    module_overrides: set = set()
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "pytestmark":
                    module_slugs.extend(_feature_slugs(node.value))
                    module_apps |= _app_marks(node.value)
                    module_params.extend(client_param_sets(node.value, scope))
                    module_overrides |= parametrized_names(node.value)

    def requested_param_sets(requested: set, overridden: set) -> list:
        """The client sets of the params fixtures `requested` reaches, directly or
        through the module's own fixtures (`seeded(sync_app)`) — bar the ones the
        test parametrizes itself, whose own set `client_param_sets` already read."""
        seen: set = set()
        todo = list(requested)
        while todo:
            name = todo.pop()
            if name in seen:
                continue
            seen.add(name)
            todo.extend(own_fixtures.get(name, ()))
        return [param_fixtures[n] for n in sorted((seen - overridden) & set(param_fixtures))]

    facts: list = []

    def visit(fn, prefix: str, class_drives: bool = False,
              inherited_apps: frozenset = frozenset(),
              inherited_params: tuple = (),
              inherited_overrides: frozenset = frozenset()) -> None:
        slugs = list(module_slugs)
        apps = set(module_apps) | set(inherited_apps)
        params = list(module_params) + list(inherited_params)
        overridden = set(module_overrides) | set(inherited_overrides)
        for dec in fn.decorator_list:
            slugs.extend(_feature_slugs(dec))
            apps |= _app_marks(dec)
            params.extend(client_param_sets(dec, scope))
            overridden |= parametrized_names(dec)
        args = list(fn.args.args) + list(fn.args.kwonlyargs)
        requested = {a.arg for a in args}
        params.extend(requested_param_sets(requested, overridden))
        fixtures = requested & launching
        # Marks and a client-set parametrization both narrow — conftest keeps the
        # item only where they agree (`params & marker_platforms`).
        columns = set(apps)
        for restricted in params:
            columns = set(restricted) if not columns else columns & restricted
        contradictory = bool(params) and not columns
        facts.append(TestFacts(
            node_id=f"{rel}::{prefix}{fn.name}", path=path, line=fn.lineno,
            features=tuple(dict.fromkeys(slugs)),
            drives_app=bool(fixtures) or browser or class_drives,
            apps=tuple(sorted(apps if contradictory else columns)),
            contradictory=contradictory))

    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test"):
            visit(node, "")
        elif isinstance(node, ast.ClassDef):
            class_drives = _class_autouse_drives_app(node, launching)
            # A class carries its marks to every method (`@pytest.mark.windows` on
            # `class TestFullJourneyInstalledApp` is the whole windows installer
            # journey's only app marker), and so does a class-body `pytestmark`.
            class_apps = set()
            class_params: list = []
            class_overrides: set = set()
            for dec in node.decorator_list:
                class_apps |= _app_marks(dec)
                class_params.extend(client_param_sets(dec, scope))
                class_overrides |= parametrized_names(dec)
            for sub in node.body:
                if isinstance(sub, ast.Assign) and any(
                        isinstance(t, ast.Name) and t.id == "pytestmark" for t in sub.targets):
                    class_apps |= _app_marks(sub.value)
                    class_params.extend(client_param_sets(sub.value, scope))
                    class_overrides |= parametrized_names(sub.value)
            for sub in node.body:
                if isinstance(sub, (ast.FunctionDef, ast.AsyncFunctionDef)) and sub.name.startswith("test"):
                    visit(sub, f"{node.name}::", class_drives, frozenset(class_apps),
                          tuple(class_params), frozenset(class_overrides))
    return facts


#: `scan_tree` answers for `(root, repo)`, keyed on a fingerprint of the files it
#: read. `features_lint.lint()` and `features_catalog.scan_app_marks()` — reached
#: from the same lint through rule 5's render — each ask for the whole tree, so
#: the gate parsed and walked all 757 test modules TWICE per run. They are the
#: same question, so the second one is free.
_SCAN_CACHE: dict = {}


def _tree_fingerprint(root: Path) -> tuple:
    """(path, mtime_ns, size) for every file `scan_tree` would read.

    The cache is keyed on this rather than on `(root, repo)` alone because a
    caller may legitimately scan one path twice with different content between
    the two — the catalog tests build a fixture world under `tmp_path`, and a
    path-only key would hand the second call the first one's answer. 13 ms over
    757 files on Windows, against the 2.9 s scan it decides to skip.
    """
    return tuple((p.as_posix(), st.st_mtime_ns, st.st_size)
                 for p in sorted(root.rglob("test_*.py"))
                 for st in (p.stat(),))


def scan_tree(root: Path = E2E, repo: Path = REPO) -> dict:
    """`{node id: TestFacts}` for every `test_*.py` under `root`."""
    out: dict = {}
    if not root.exists():
        return out
    conftest = root / "conftest.py"
    conftest_stamp = None
    if conftest.exists():
        st = conftest.stat()
        conftest_stamp = (st.st_mtime_ns, st.st_size)
    key = (root.as_posix(), repo.as_posix(), conftest_stamp, _tree_fingerprint(root))
    cached = _SCAN_CACHE.get(key)
    if cached is not None:
        return cached

    launching = app_launching_fixtures(conftest_fixtures(root))
    for path in sorted(root.rglob("test_*.py")):
        for facts in scan_file(path, repo=repo, launching=launching):
            out[facts.node_id] = facts
    _SCAN_CACHE[key] = out
    return out
