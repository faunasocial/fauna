"""Which WS-RPC kinds — or admin-shell element IDs — a test function can reach,
by static call graph.

The *mechanism* half of live's exclusion class (3). The *policy* half — which
kinds count as global-admin-mutating, and which element IDs are the admin
shell's — lives in `nest_surface.py`; this module answers only "which strings
of a registered vocabulary does this test body reach", and answers it for
every test, not just the ones a fixture betrays.

Why a call graph rather than a grep over the test body: the literal is usually
one wrapper away (`ws_rpc_admin_client.force_rotate_dkim` names the kind, the
test names the wrapper; `AdminActions.save_mail_spam` names the button), so a
body-local scan sees a fraction of the reach. The graph is keyed on the **bare
function name**, which merges same-named functions across modules. That is an
over-approximation — it can only ever attribute *more* kinds to a test, never
fewer — and over-approximating is the safe direction here: the cost is a test
needlessly excluded from live, the alternative is a test that mutates a shared
box. The one place the merge is cut is a **module-private** function (`_state`,
`_off`): a bare call to one resolves in its own module, which is what the
leading underscore means (`_is_private`, `build_reach`).

Built **lazily and only when a live run asks**, so the standalone inner loop
pays nothing for it (the nest-mode axis's standing requirement).
"""

from __future__ import annotations

import ast
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple

from helpers.fixture_closure import real_fixture_closure

#: The trees this module reasons about. Every `.py` under them is fair game:
#: tests, helpers, actions, clients and conftest alike, because a wrapper that
#: names a kind is as likely to live in `clients/` as in the test itself.
#:
#: `tests/common/` is the second root because conftest imports its wire helpers
#: from there (`from common.auth import set_tier_caps`), and a helper the graph
#: never parsed reaches nothing: rooted at the e2e tree alone, `test_user`'s
#: call to `set_tier_caps` looked harmless while it lifted a live box's `free`
#: tier for every user on it.
_ROOT = Path(__file__).resolve().parents[1]
_ROOTS = (_ROOT, _ROOT.parent / "common")

#: Modules whose top-level constants are the scan's own POLICY tables — the
#: kinds, routes and IDs class (3) and its siblings match against — never a
#: string the module's functions send. `nest_surface.classify` names those
#: tables, so resolving them (`_module_constants`) would hand every harness
#: unit test that classifies an item the whole vocabulary it classifies by.
_VOCABULARY_MODULES = frozenset({"nest_surface.py", "kind_reach.py"})

#: Functions whose reach is NOT handed to their callers: the STANDALONE-ONLY
#: bodies behind the mail-venue seam. Every reach this graph computes is asked
#: by a non-standalone classification (`classify` returns before any of it in
#: standalone), and these two run only in standalone — the standalone
#: provider's `start_mail_venue` dispatches to them, the docker one starts the
#: image's own bridges instead. Through the bare-name merge of every provider's
#: `start_mail_venue`, a test that CALLS a venue factory by name
#: (`dav_toggle_venue(mail=…)`) was credited with their host-bridge spawn
#: (`fauna.bridges.request_enrollment`, class (5)) and graded `loopback_kind` in
#: docker for an enrollment docker never makes. What a venue sets running in
#: docker is still reached — through the docker method, the admin kinds that
#: keep every venue fixture on `GLOBAL_ADMIN_FIXTURES` included. The bodies
#: keep their own reach, and a test body naming a kind directly still reaches
#: it. Keyed by bare name (a private function's node is `<module>::<name>`).
#: Pinned by `test_nest_mode_axis.py::test_kind_reach_stops_at_the_mail_venue_seam`.
_REACH_BARRIERS = frozenset({"_dedicated_mail_nest_impl", "_dav_toggle_venue_impl"})

#: Directories with no bearing on what a test reaches at runtime.
_SKIP_DIRS = {".pytest_cache", "__pycache__", ".venv", "node_modules"}


def _iter_sources(root: Path):
    for path in sorted(root.rglob("*.py")):
        if any(part in _SKIP_DIRS for part in path.parts):
            continue
        yield path


def _is_dunder(name: str) -> bool:
    """``__enter__``-shaped: Python-protocol-mandated, not a domain name.

    Every class that supports ``with`` defines `__enter__`/`__exit__` under
    THAT exact spelling — the language requires it, so the name carries no
    domain signal the way `force_rotate_dkim` or `arm_rpc_hold` does. Merged
    by bare name like everything else, one unrelated class's dunder (a CalDAV
    client's `__enter__`, say) would drag its reach onto every OTHER class's
    `with` block in the tree — not "usually one wrapper away" (the
    over-approximation the module docstring accepts), but EVERY context
    manager, unconditionally. Found via `test_nest_mode_axis.py`'s own
    over-reach guard: `FailingNestArm.__enter__` (`search_journeys.py`) calls
    `arm_rpc_hold` directly, and that one call site's reach, through this
    merge, had reached tests as unrelated as a snapshot file download and
    enabling ActivityPub.
    """
    return name.startswith("__") and name.endswith("__") and len(name) > 4


def _is_private(name: str) -> bool:
    """``_state``-shaped: a module-private helper by Python convention.

    A bare call to such a name resolves in the module that makes it — that is
    what the leading underscore means — so the graph keys these per module
    (`_callees`, `build_reach`) instead of merging them by bare name. The
    merge was measured to be the dominant false positive of the admin-shell
    scan: four test modules each defining their own `_state`, `_step`,
    `_snapshot` or `_off` inherited, through one of those names, the reach of
    a live-provisioning test's `_state` that reads the mail toggle — and a
    `cargo-target` script test read as "drives the admin shell".
    """
    return name.startswith("_") and not _is_dunder(name) and not name.startswith("__")


def _callees(node: ast.AST) -> set[tuple[bool, str]]:
    """Every call inside `node`, as `(is_bare, name)`.

    `foo()` contributes `(True, "foo")`; `client.foo()` contributes
    `(False, "foo")`. Dropping the receiver is what makes the graph name-keyed,
    with the over-approximation documented above — except a dunder call
    (`x.__enter__()`), which contributes nothing: see `_is_dunder`. The
    bare/attribute bit is what lets `build_reach` resolve a bare call to a
    private name inside its own module (`_is_private`) while an attribute call
    to one (`self._state()`, a private method on some class) keeps the merge.
    """
    out: set[tuple[bool, str]] = set()
    for sub in ast.walk(node):
        if isinstance(sub, ast.Call):
            target = sub.func
            if isinstance(target, ast.Name) and not _is_dunder(target.id):
                out.add((True, target.id))
            elif isinstance(target, ast.Attribute) and not _is_dunder(target.attr):
                out.add((False, target.attr))
    return out


def _self_private_calls(node: ast.AST) -> set[str]:
    """Private names `node` calls as `self._x()` / `cls._x()` — a method of the
    caller's own object, never a module-level function of that name. Kept apart
    so `build_reach` merges such a call over private METHODS only: through the
    module-level merge, a fake AT Protocol server's `self._record()` handed
    every SPA-proxy journey the TLS issuance hook a test module's own
    `_record` posts (measured 2026-10-23)."""
    out: set[str] = set()
    for sub in ast.walk(node):
        if (
            isinstance(sub, ast.Call)
            and isinstance(sub.func, ast.Attribute)
            and isinstance(sub.func.value, ast.Name)
            and sub.func.value.id in ("self", "cls")
            and _is_private(sub.func.attr)
        ):
            out.add(sub.func.attr)
    return out


def _method_ids(tree: ast.AST) -> set[int]:
    """`id()` of every function defined directly in a class body."""
    return {
        id(item)
        for cls in _classes(tree)
        for item in cls.body
        if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
    }


def _callee_names(node: ast.AST) -> set[str]:
    """Every function name called anywhere inside `node` — `_callees` without
    the bare/attribute bit, for callers that only need the names."""
    return {name for _, name in _callees(node)}


def _kind_literals(node: ast.AST, vocabulary: frozenset[str]) -> set[str]:
    """Registered kinds named by a string constant inside `node`.

    Membership in `vocabulary` — the closed set `kind.rs` registers — is what
    keeps error codes out. `fauna.bridges.permission_denied` and
    `fauna.admin.conflict` are shaped exactly like kinds and appear in test
    bodies far more often than the kinds do; they are not kinds, they are what
    the nest answers when it refuses one, and a prefix scan that counted them
    would gut live's eligible set for a fictional reason.

    The literal must be the *whole* constant, so a docstring mentioning a kind
    in prose never counts.
    """
    return _strings(node) & vocabulary


def _strings(node: ast.AST) -> set[str]:
    """Every string constant under `node` — what `_kind_literals` and
    `_route_literals` filter, and what the reach graph keeps per node in place
    of the AST (`_graph`), so each selector filters it without a parse."""
    return {
        sub.value for sub in ast.walk(node)
        if isinstance(sub, ast.Constant) and isinstance(sub.value, str)
    }


def _subscript_keys(node: ast.AST) -> set:
    """Every constant a mapping under `node` is subscripted by in LOAD context
    — what `_key_reads` filters (its note says why only a load counts), kept
    per node by the reach graph like `_strings`."""
    out: set = set()
    for sub in ast.walk(node):
        if not isinstance(sub, ast.Subscript) or not isinstance(sub.ctx, ast.Load):
            continue
        if isinstance(sub.slice, ast.Constant):
            out.add(sub.slice.value)
    return out


def _key_reads(node: ast.AST, keys: frozenset[str]) -> set[str]:
    """Contract keys READ off a mapping under `node` — `handle["peer_url"]`.

    Deliberately narrower than `_kind_literals`, and the narrowing is the whole
    point. A kind name is a rare string that means one thing wherever it
    appears, so matching any constant is safe. A contract key is a short common
    word that appears on BOTH sides of the mapping: the provider that *writes*
    `{"peer_url": ...}` names it too, and since this graph is keyed on the bare
    function name, one write inside a method as common as `start` merges into
    every caller of every same-named method and floods the reach (measured: a
    constant scan for `peer_url` selected 3428 of ~4200 test functions — the
    whole suite).

    A subscript in **load** context is the act being classified: taking the
    value out of a handle to hand it somewhere. A write (`Dict` literal,
    `setdefault`, a subscript in store context) is the harness furnishing the
    handle, which every test does transitively and which classifies nothing.
    """
    return _subscript_keys(node) & keys


def _module_constants(tree: ast.AST) -> dict[str, tuple[set[str], set]]:
    """What each top-level assignment of a module holds, by its target name —
    its `_strings` and its `_subscript_keys`, which every selector filters.

    A test reaches a string it keeps in a module constant as surely as one in
    its body: a `@pytest.mark.parametrize("kind,payload", _PUT_CASES)` table,
    an `ARM_BUTTON = "admin-…"` element-ID name. `ast.walk(fn)` sees the bare
    name only, so each function also takes the literals of every top-level
    assignment of its OWN module that it names (`build_reach`) — a name is
    resolved where Python resolves a global, never across modules.
    """
    out: dict[str, tuple[set[str], set]] = {}
    for stmt in getattr(tree, "body", ()):
        if isinstance(stmt, ast.Assign):
            targets, value = stmt.targets, stmt.value
        elif isinstance(stmt, ast.AnnAssign) and stmt.value is not None:
            targets, value = [stmt.target], stmt.value
        else:
            continue
        strings, reads = _strings(value), _subscript_keys(value)
        if not strings and not reads:
            continue
        for target in targets:
            if isinstance(target, ast.Name):
                held = out.setdefault(target.id, (set(), set()))
                held[0].update(strings)
                held[1].update(reads)
    return out


def _module_aliases(tree: ast.AST) -> dict[str, str]:
    """A module's second names for functions: `_advance = advance_mail_clock`
    and `from m import seeded_outbound as _seeded_outbound`, alias → the name
    it stands for. A bare call to the alias is a call to that name
    (`build_reach`); without the edge a test calling a hook only through its
    module's alias reached nothing, and the docker axis collected it against a
    release image that carries no hook."""
    out: dict[str, str] = {}
    for stmt in getattr(tree, "body", ()):
        if isinstance(stmt, ast.ImportFrom):
            for alias in stmt.names:
                if alias.asname and alias.asname != alias.name:
                    out[alias.asname] = alias.name
        elif (
            isinstance(stmt, ast.Assign)
            and isinstance(stmt.value, ast.Name)
            and len(stmt.targets) == 1
            and isinstance(stmt.targets[0], ast.Name)
        ):
            out[stmt.targets[0].id] = stmt.value.id
    return out


def _imported_privates(tree: ast.AST, roots) -> dict[str, str]:
    """A module's private names imported from another module of the trees, by
    the name they were defined under → that module's graph node
    (`<path>::<name>`). `from tests.api.test_public_folder_fetch import _record`
    is THAT module's `_record`, not every module's: resolved by the merge, a
    share-link journey inherited the TLS issuance hook another module's
    `_record` posts. An import the trees do not hold keeps the merge."""
    out: dict[str, str] = {}
    for stmt in getattr(tree, "body", ()):
        if not isinstance(stmt, ast.ImportFrom) or stmt.level or not stmt.module:
            continue
        rel = stmt.module.replace(".", "/") + ".py"
        source = next(
            (c for base in roots for c in (base / rel, base.parent / rel) if c.is_file()),
            None,
        )
        if source is None:
            continue
        for alias in stmt.names:
            if _is_private(alias.name):
                out[alias.name] = f"{source}::{alias.name}"
    return out


def _names_loaded(fn: ast.AST) -> set[str]:
    """Every bare name `fn` reads — its decorators included, which is where a
    `parametrize` table is named — less the names it binds itself, which
    shadow a module constant of the same name."""
    loaded: set[str] = set()
    bound: set[str] = set()
    for sub in ast.walk(fn):
        if isinstance(sub, ast.Name):
            (loaded if isinstance(sub.ctx, ast.Load) else bound).add(sub.id)
        elif isinstance(sub, ast.arg):
            bound.add(sub.arg)
    return loaded - bound


def _functions(tree: ast.AST):
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield node


#: The methods a call to a class runs on the caller's behalf: construction,
#: and the `with` protocol a constructed object is almost always entered by.
_CLASS_PROTOCOL = frozenset({
    "__init__", "__post_init__", "__enter__", "__exit__", "__aenter__", "__aexit__",
})


def _classes(tree: ast.AST):
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef):
            yield node


def _protocol_methods(cls: ast.ClassDef):
    for node in cls.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and (
            node.name in _CLASS_PROTOCOL
        ):
            yield node


def _route_literals(node: ast.AST, prefix: str) -> set[str]:
    """String constants under `node` that start with `prefix`.

    A prefix rather than a closed vocabulary, because the caller builds the URL:
    `f"{port_base_url(port)}/api/v1/test/web-paywall/expired-token"` is a
    JoinedStr whose constant piece is the path alone, and a path-parameterized
    route (`.../channel/{id}/refuse`) never appears whole anywhere. The prefix
    `/api/v1/test/` is safe where a bare kind prefix was not: nest serves nothing
    else under it, and there is no "error code shaped like a route" to admit.
    """
    return _with_prefix(_strings(node), prefix)


def _with_prefix(strings, prefix: str) -> set[str]:
    return {s for s in strings if s.startswith(prefix)}


#: How many source files the reach graph has parsed in this process. The graph
#: is selector-independent, so it is parsed once whatever vocabularies,
#: prefixes and key sets are asked of it (`_graph`); a test pins that by this
#: count. A whole-tree parse is the dominant cost of a docker/live
#: classification — 34 MB of source, measured at 30–80 s on a loaded Windows host,
#: where one parse per selector put the first classifying test within 72 s of
#: the 900 s timeout that ends the whole run.
_graph_parses = 0


def graph_parse_count() -> int:
    """Source files the reach graph has parsed so far in this process."""
    return _graph_parses


class _Graph(NamedTuple):
    """The selector-independent half of `build_reach`, built once per process.

    The ASTs are not kept — holding every tree is ~890 MB — only what every
    selector filters: each node's string constants (`_strings`) and its
    subscript-load keys (`_subscript_keys`), the module constants it loads
    folded in, plus the resolved call edges.
    """

    strings: dict[str, frozenset[str]]
    reads: dict[str, frozenset]
    calls: dict[str, frozenset[str]]


def build_reach(
    vocabulary: frozenset[str] | None = None,
    root: Path | None = None,
    *,
    prefix: str | None = None,
    keys: frozenset[str] | None = None,
    keyed: bool = False,
) -> dict[str, frozenset[str]]:
    """Map every function name in the e2e and common trees to the strings it can reach.

    `keyed=True` answers by graph node instead of by bare name — a private
    function as `<path>::<name>` — for a caller that knows which module's
    function it means (`routes_reached_by_own_code`).

    `reach[f] = named directly in f  ∪  ⋃ reach[g] for every g that f calls`,
    computed to a fixpoint so a name three wrappers deep still lands on the test.

    Exactly one of `vocabulary` (closed-set membership — WS-RPC kinds),
    `prefix` (route paths) or `keys` (contract keys read off a handle) selects
    what counts. The graph itself is the same walk every way, which is the
    point: a second scanner would drift from this one, and every caller depends
    on the same "usually one wrapper away" fact. It is built once per process
    (`_graph`); a selector only filters each node's summary and runs the
    fixpoint.
    """
    selectors = [vocabulary is not None, prefix is not None, keys is not None]
    if sum(selectors) != 1:
        raise ValueError("pass exactly one of vocabulary=, prefix= or keys=")
    graph = _graph((root,)) if root is not None else _default_graph()

    if prefix is not None:
        reach = {
            key: _with_prefix(strings, prefix) for key, strings in graph.strings.items()
        }
    elif keys is not None:
        reach = {key: set(reads & keys) for key, reads in graph.reads.items()}
    else:
        reach = {key: set(strings & vocabulary) for key, strings in graph.strings.items()}

    grew = True
    while grew:
        grew = False
        for key, callees in graph.calls.items():
            before = len(reach[key])
            for callee in callees:
                if (callee in reach and callee != key
                        and callee.rsplit("::", 1)[-1] not in _REACH_BARRIERS):
                    reach[key] |= reach[callee]
            if len(reach[key]) != before:
                grew = True

    if keyed:
        return {key: frozenset(kinds) for key, kinds in reach.items()}

    # The lookup view is by bare name: a test function is public, and a fixture
    # may be private (`_session_primary_mail_domain`) — for those the answer is
    # the union over every module's node, the merge the caller already expects.
    out: dict[str, frozenset[str]] = {}
    for key, kinds in reach.items():
        name = key.rsplit("::", 1)[-1] if "::" in key else key
        out[name] = frozenset(out.get(name, frozenset()) | kinds)
    return out


def _default_graph() -> _Graph:
    """The graph of the e2e and common trees, parsed once per process. An
    explicit `root` (a pin's scratch tree, rewritten between calls) is parsed
    afresh every time instead."""
    return _cached_graph(_ROOTS)


@lru_cache(maxsize=2)
def _cached_graph(roots: tuple[Path, ...]) -> _Graph:
    return _graph(roots)


def _graph(roots: tuple[Path, ...]) -> _Graph:
    """Parse `roots` and keep the selector-independent graph (`_Graph`)."""
    global _graph_parses
    # Graph keys: a public function is its bare name (merged across modules —
    # the over-approximation above); a private one (`_is_private`) is keyed
    # `<module>::<name>`, so each module's `_state` is its own node. A bare call
    # to a private name is resolved to the caller's own module when that
    # module defines it (what the leading underscore means in Python); a bare
    # call the module does not define (a private name imported from elsewhere,
    # rare) and every attribute call (`self._state()`, a private method on an
    # object from anywhere) keep the merge, over every module's node of that
    # name — still the safe direction.
    strings: dict[str, set[str]] = {}  # node → `_strings`, its constants folded in
    reads: dict[str, set] = {}  # node → `_subscript_keys`, its constants folded in
    # node → module → the calls that node's definition(s) in that module make. A
    # public name is one merged node across modules, and each definition's
    # private calls must resolve in ITS OWN module: keeping only the first
    # module's namespace let `observe` in one helper (calling its own `_open`)
    # resolve against another module and merge with every module's `_open`.
    raw_calls: dict[str, dict[str, set[tuple[bool, str]]]] = {}
    private_nodes: dict[str, set[str]] = {}  # bare private name → its nodes
    imported: dict[str, dict[str, str]] = {}  # module → `_imported_privates`
    private_methods: dict[str, set[str]] = {}  # bare private name → method nodes
    self_calls: dict[str, set[str]] = {}  # node → private names called on self/cls
    other_calls: dict[str, set[str]] = {}  # node → private names called on anything else
    for path in (path for r in roots for path in _iter_sources(r)):
        _graph_parses += 1
        try:
            tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        except SyntaxError:
            # A file the interpreter could not parse cannot contribute reach.
            # Skipping it is not a silent hole: it cannot run either.
            continue
        module = str(path)
        constants = {} if path.name in _VOCABULARY_MODULES else _module_constants(tree)
        aliases = _module_aliases(tree)
        imported[module] = _imported_privates(tree, roots)
        methods = _method_ids(tree)

        def unaliased(callees: set[tuple[bool, str]]) -> set[tuple[bool, str]]:
            # A bare call to a module alias is a bare call to what it names;
            # chains (`a = b`, `b = c`) resolve to the end, cycles stop.
            out = set()
            for is_bare, name in callees:
                seen = {name}
                while is_bare and name in aliases and aliases[name] not in seen:
                    name = aliases[name]
                    seen.add(name)
                out.add((is_bare, name))
            return out

        def summarize(key: str, fn: ast.AST) -> set[tuple[bool, str]]:
            # What any selector can find in `fn` — its own constants and keyed
            # reads, and those of every module constant it names — and the
            # calls it makes.
            own_strings = strings.setdefault(key, set())
            own_reads = reads.setdefault(key, set())
            own_strings |= _strings(fn)
            own_reads |= _subscript_keys(fn)
            for name in _names_loaded(fn) & constants.keys():
                held_strings, held_reads = constants[name]
                own_strings |= held_strings
                own_reads |= held_reads
            callees = unaliased(_callees(fn))
            on_self = _self_private_calls(fn)
            self_calls.setdefault(key, set()).update(on_self)
            other_calls.setdefault(key, set()).update(
                name for is_bare, name in callees
                if not is_bare and _is_private(name) and name not in on_self
            )
            return callees

        for fn in _functions(tree):
            key = f"{module}::{fn.name}" if _is_private(fn.name) else fn.name
            if _is_private(fn.name):
                private_nodes.setdefault(fn.name, set()).add(key)
                if id(fn) in methods:
                    private_methods.setdefault(fn.name, set()).add(key)
            callees = summarize(key, fn)
            raw_calls.setdefault(key, {}).setdefault(module, set()).update(callees)
        # A class is a node too, keyed by its own name: `FailingNestArm(port)`
        # is a call to the class, and what that call sets running is the
        # class's OWN protocol methods (`_CLASS_PROTOCOL`). `_callees` drops
        # dunder calls because merging every `__enter__` by bare name would
        # drag one class's reach onto every `with` block in the tree; keying
        # the dunders under their class restores the one edge that merge
        # threw away. Measured 2026-10-05: three search journeys armed the
        # `/api/v1/test/rpc-hold/` hook through `FailingNestArm.__enter__`,
        # class (6) never saw it, and a live run recorded them as failed.
        for cls in _classes(tree):
            key = f"{module}::{cls.name}" if _is_private(cls.name) else cls.name
            if _is_private(cls.name):
                private_nodes.setdefault(cls.name, set()).add(key)
            slot = raw_calls.setdefault(key, {}).setdefault(module, set())
            for method in _protocol_methods(cls):
                slot.update(summarize(key, method))
            strings.setdefault(key, set())
            reads.setdefault(key, set())

    calls: dict[str, frozenset[str]] = {}
    for key, by_module in raw_calls.items():
        resolved: set[str] = set()
        for module, callees in by_module.items():
            for is_bare, name in callees:
                if not _is_private(name):
                    resolved.add(name)
                    continue
                own = f"{module}::{name}"
                source = imported.get(module, {}).get(name)
                if is_bare and own in strings:
                    resolved.add(own)
                elif is_bare and source in strings:
                    resolved.add(source)
                elif (
                    not is_bare
                    and name in self_calls.get(key, ())
                    and name not in other_calls.get(key, ())
                    and name in private_methods
                ):
                    resolved |= private_methods[name]
                else:
                    resolved |= private_nodes.get(name, set())
        calls[key] = frozenset(resolved)

    return _Graph(
        strings={key: frozenset(found) for key, found in strings.items()},
        reads={key: frozenset(found) for key, found in reads.items()},
        calls=calls,
    )


@lru_cache(maxsize=8)
def _cached_reach(vocabulary: frozenset[str]) -> dict[str, frozenset[str]]:
    return build_reach(vocabulary)


def kinds_reached_by(name: str, vocabulary: frozenset[str]) -> frozenset[str]:
    """Kinds the test function `name` can reach. Empty when it reaches none.

    The graph is built on the first call and cached for the run — one parse of
    the tree per session, and none at all unless a live run asks.
    """
    return _cached_reach(vocabulary).get(name, frozenset())


def element_ids_reached_by(name: str, vocabulary: frozenset[str]) -> frozenset[str]:
    """Element IDs in `vocabulary` the test function `name` can reach.

    The same graph and the same closed-set match as `kinds_reached_by`, over a
    second registered vocabulary: ui.yaml's element IDs are to the app what
    `kind.rs`'s kinds are to the wire — every one the harness can name is in
    the spec, and nothing else is shaped like one. It exists as its own name
    so a reader of `nest_surface` sees which door is being scanned; the cache
    is shared, keyed on the vocabulary, so a live run builds the graph once per
    vocabulary and a standalone run never does.
    """
    return _cached_reach(vocabulary).get(name, frozenset())


@lru_cache(maxsize=4)
def _cached_route_reach(prefix: str) -> dict[str, frozenset[str]]:
    return build_reach(prefix=prefix)


def routes_reached_by(name: str, prefix: str) -> frozenset[str]:
    """Routes under `prefix` the test function `name` can reach.

    Same graph, same cost model as `kinds_reached_by`: built on the first call
    that asks and cached for the run, so a standalone run never builds it.
    """
    return _cached_route_reach(prefix).get(name, frozenset())


@lru_cache(maxsize=4)
def _cached_key_reach(keys: frozenset[str]) -> dict[str, frozenset[str]]:
    return build_reach(keys=keys)


def keys_read_by(name: str, keys: frozenset[str]) -> frozenset[str]:
    """Contract keys the test function `name` reads off a handle.

    Same graph, same cost model as `kinds_reached_by`: built on the first call
    that asks, so a standalone run never builds it.
    """
    return _cached_key_reach(keys).get(name, frozenset())


def test_function_name(item) -> str:
    """The bare function name behind a pytest item.

    `originalname` is the un-parametrized name when pytest has one; otherwise
    strip the `[...]` parameter suffix ourselves.
    """
    name = getattr(item, "originalname", None)
    if name:
        return name
    return getattr(item, "name", "").split("[", 1)[0]


# ── Own-code scans ───────────────────────────────────────────────────────────
#
# Everything above follows the whole tree's call graph, merged by bare name,
# because a kind or an admin element ID is a rare string that means one thing
# wherever it appears: over-reach there costs a test needlessly excluded. The
# questions below are about COMMON words — a `proc` read, an `unclaimed=True`,
# a `getfixturevalue("self_signed_nest")` — and the merged graph floods on them
# (measured 2026-10-05: a capability-key scan through it reached 5,709 of 6,633
# test functions, through `stop`, `close` and `launch` methods that every driver
# defines). So these scans read only the code that is the test's OWN: its
# function, the module-level fixtures of its module it requests, the methods of
# its class it requests (an autouse `setup`), and every module-private function
# those call by bare name — the one edge whose target the leading underscore
# pins (`_is_private`). A shared helper is reached only by being NAMED in a
# derived table (`capability_helpers`), never by a merged name.


@lru_cache(maxsize=None)
def _parsed(path: str) -> ast.Module | None:
    try:
        return ast.parse(Path(path).read_text(encoding="utf-8", errors="replace"))
    except (OSError, SyntaxError):
        return None


def _top_level_functions(tree: ast.Module) -> dict[str, ast.AST]:
    return {
        node.name: node for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    }


def own_code(item) -> list[ast.AST]:
    """The function nodes that are `item`'s own code (see the section note).

    Empty for an item with no source file — the in-process fakes the mode-axis
    pins build — so every scan below answers "nothing" for them, the same
    default `classify` gives an unknown test function.
    """
    path = getattr(item, "path", None) or getattr(item, "fspath", None)
    tree = _parsed(str(path)) if path else None
    if tree is None:
        return []
    module_fns = _top_level_functions(tree)
    cls = getattr(item, "cls", None)
    class_fns: dict[str, ast.AST] = {}
    if cls is not None:
        for node in tree.body:
            if isinstance(node, ast.ClassDef) and node.name == cls.__name__:
                class_fns = _top_level_functions(node)  # its methods
    name = test_function_name(item)
    roots = [class_fns.get(name) or module_fns.get(name)]
    requested = real_fixture_closure(item)
    roots += [class_fns[f] for f in sorted(requested & class_fns.keys()) if f != name]
    roots += [module_fns[f] for f in sorted(requested & module_fns.keys()) if f != name]

    out: list[ast.AST] = []
    seen: set[int] = set()
    stack = [fn for fn in roots if fn is not None]
    while stack:
        fn = stack.pop()
        if id(fn) in seen:
            continue
        seen.add(id(fn))
        out.append(fn)
        for is_bare, callee in _callees(fn):
            if is_bare and _is_private(callee) and callee in module_fns:
                stack.append(module_fns[callee])
    return out


def routes_reached_by_own_code(item, prefix: str) -> frozenset[str]:
    """`routes_reached_by` over every function of `item`'s own code — the test,
    the module fixtures it requests and the private helpers those call — each
    followed through the merged graph. A route prefix is a rare string (the
    `_route_literals` note), so the merge's over-reach costs nothing here, and a
    hook a module fixture posts (`serve_nest` holding the custody pump) is the
    test's as surely as one its body posts."""
    path = getattr(item, "path", None) or getattr(item, "fspath", None)
    module = str(Path(str(path)).resolve()) if path else ""
    reach = _cached_keyed_route_reach(prefix)
    out: set[str] = set(routes_reached_by(test_function_name(item), prefix))
    for fn in own_code(item):
        key = f"{module}::{fn.name}" if _is_private(fn.name) else fn.name
        out |= reach.get(key, frozenset())
    return frozenset(out)


@lru_cache(maxsize=4)
def _cached_keyed_route_reach(prefix: str) -> dict[str, frozenset[str]]:
    return build_reach(prefix=prefix, keyed=True)


def _shared_sources():
    """The shared-helper trees: everything a test imports a helper FROM."""
    for root in (_ROOT / "helpers", _ROOT / "clients", _ROOT / "actions", _ROOT.parent / "common"):
        yield from _iter_sources(root)


@lru_cache(maxsize=4)
def capability_helpers(keys: frozenset[str]) -> dict[str, frozenset[str]]:
    """Shared module-level PUBLIC functions → the contract keys in `keys` they read.

    Derived, never listed: a function in `helpers/`, `clients/`, `actions/` or
    `tests/common/` belongs here when its own body — or a module-private helper
    it calls bare, or another function already in the table — subscripts a
    handle with one of `keys` (`stop_nest` reads `nest["proc"]`,
    `segment_dir` reads `nest["db_path"]`). Methods never qualify: a method
    name is merged by every class that defines it, which is the flood this
    section exists to avoid.
    """
    modules = []
    for path in _shared_sources():
        tree = _parsed(str(path))
        if tree is not None:
            modules.append(_top_level_functions(tree))

    table: dict[str, set[str]] = {}

    # A public name several modules define (`observe`, `write`) is merged in the
    # table, so an ATTRIBUTE call to it (`f.write(...)` on a file) inside a
    # helper cannot be told from a call to the capability-reading one — that
    # taints every helper that writes a file (measured 2026-11-06: the store
    # reviewer journey deselected as "reads db_path"). Such a call does not
    # propagate; a BARE call resolves in its own module first, not by the
    # merged name.
    defined_in: dict[str, int] = {}
    for fns in modules:
        for name in fns:
            defined_in[name] = defined_in.get(name, 0) + 1
    ambiguous = {name for name, n in defined_in.items() if n > 1}

    def reads(fn, fns, seen):
        if id(fn) in seen:
            return set()
        seen.add(id(fn))
        out = set(_key_reads(fn, keys))
        for is_bare, callee in _callees(fn):
            if is_bare and callee in fns:
                out |= reads(fns[callee], fns, seen)
            elif callee in table and (is_bare or callee not in ambiguous):
                out |= table[callee]
        return out

    grew = True
    while grew:
        grew = False
        for fns in modules:
            for name, fn in fns.items():
                if _is_private(name):
                    continue
                found = reads(fn, fns, set())
                if found - table.get(name, set()):
                    table.setdefault(name, set()).update(found)
                    grew = True
    return {name: frozenset(found) for name, found in table.items()}


def capability_keys_read_by_own_code(item, keys: frozenset[str]) -> frozenset[str]:
    """Keys in `keys` the test's own code reads off a nest handle — directly,
    or by calling a shared helper `capability_helpers` derived as reading one."""
    helpers = capability_helpers(keys)
    out: set[str] = set()
    for fn in own_code(item):
        out |= _key_reads(fn, keys)
        for _, callee in _callees(fn):
            out |= helpers.get(callee, frozenset())
    return frozenset(out)


def lazy_fixtures_requested_by_own_code(item) -> frozenset[str]:
    """Fixtures the test's own code requests LAZILY —
    `request.getfixturevalue("self_signed_nest")` with a literal name.

    pytest's closure never lists these, so every closure-keyed rule is blind
    to them. Callers decide which rules may read them: one requested inside a
    branch (`if builds_local_nest(mode)`) is a fixture the run may never
    request, so a rule whose fixtures are mode-conditional must not.
    """
    out: set[str] = set()
    for fn in own_code(item):
        for sub in ast.walk(fn):
            if (
                isinstance(sub, ast.Call)
                and isinstance(sub.func, ast.Attribute)
                and sub.func.attr == "getfixturevalue"
                and sub.args
                and isinstance(sub.args[0], ast.Constant)
                and isinstance(sub.args[0].value, str)
            ):
                out.add(sub.args[0].value)
    return frozenset(out)


def start_options_passed_by_own_code(
    item, options: frozenset[str], entry_points: frozenset[str]
) -> frozenset[str]:
    """Start options in `options` the test's own code passes, as a TRUTHY
    literal keyword, to a nest-start entry point in `entry_points`
    (`provider.start(..., unclaimed=True)`). A falsy literal asks for the
    default, the rule `_OptionAwareProvider._refuse_unsupported` applies."""
    out: set[str] = set()
    for fn in own_code(item):
        for sub in ast.walk(fn):
            if not isinstance(sub, ast.Call):
                continue
            target = sub.func
            callee = (
                target.id if isinstance(target, ast.Name)
                else target.attr if isinstance(target, ast.Attribute) else None
            )
            if callee not in entry_points:
                continue
            for kw in sub.keywords:
                if (
                    kw.arg in options
                    and isinstance(kw.value, ast.Constant)
                    and kw.value.value
                ):
                    out.add(kw.arg)
    return frozenset(out)
