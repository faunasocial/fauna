#!/usr/bin/env python3
"""Flavor-diff seam witness over generated FFI binding trees (e2e-automation-surface-gating.md
point 15).

The per-recipe witnesses (`_android-ffi-bindgen`, windows'/apple's copies of the
same shape) assert seam-absence by grepping the generated bindings for the
`*ForTest` NAMING CONVENTION. Seams that are not test-named —
`setProviderBaseUrls`, `resolvedNestDialUrl`, `callMachineMethod`,
`FfiChildAgentSpawner`, … — are invisible to that grep: their only guard is the
cfg gate itself, and a regression that widens one block's gate ships a
socket-redirect (or process-exec) primitive with no witness firing.

This script is the generalized witness. The difference between the two flavors'
generated binding faces, (test − production), is by construction exactly the
gated seam surface, so three asserts pin it:

  * **test-tree presence** — every KNOWN_SEAMS entry is declared in the
    test-flavored tree. Proves the floor list names real, still-testable seams:
    a renamed/deleted seam fails HERE, at the moment the test flavor
    regenerates, instead of leaving the witness watching a name that no longer
    exists (the silent-rot failure a hand-kept absence list cannot see).
  * **production-tree absence** — no KNOWN_SEAMS entry and no `*ForTest`-named
    declaration appears in the production-flavored tree. A dead cfg gate makes
    the symbol appear in BOTH trees — it vanishes from the difference set, and
    this is the assert that catches it.
  * **difference coverage** (runs when both trees are given) — every
    declaration present in the test tree but not the production tree is
    accounted for: test-named by convention (`*ForTest` suffix or `test*`
    prefix), a KNOWN_SEAMS entry, a member/companion of a KNOWN_SEAMS type or
    of a test-named type, or a declaration that is no entry point at all —
    uniffi's `FfiConverter*` lowering objects and pure-data types (records,
    enums and their variants), each present only because a function or object
    in the same difference takes or returns it, which is checked there.
    An unaccounted name is a NEW seam that has not stated its witness: add it
    to KNOWN_SEAMS with a one-line reason (this automates the forward-watch
    "any new non-test-named seam must state its witness").

Each flavored bindgen run passes only its OWN freshly generated tree, so the
first two asserts are evaluated exactly when their tree is fresh — the
conjunction across the two runs IS the difference-set assert, with no
stale-sibling-tree caveat. The two-tree coverage assert runs
whenever the sibling flavor's tree is also on disk (android's per-buildType
staging keeps both at once) AND that tree is of the same vintage as the sources
the current one was generated from.

That vintage precondition is load-bearing and is enforced by the CALLER, not
here: this script compares two symbol SETS and cannot tell "present only in the
test tree because a cfg gate leaked it" from "present only in the test tree
because the production tree predates the symbol's deletion". The two are the
same shape, and it answers both the same way — with a failure whose text tells
you to floor-list the name. Measured 2026-08-16: a `src/debug` tree staged three
days earlier reported `FfiCueVisibilityGates` and `setIncludeEncryptedMetadata`,
product symbols retired the day after that staging, as undeclared seams. Only
the caller knows how old the trees are, so `_android-ffi-bindgen` probes the
sibling with `build-if-stale.py --check` against the same sources and withholds
it (loudly) when it is stale.

Names are compared normalized — underscores stripped, lowercased — so ONE floor
list written in Rust snake_case covers every language UniFFI generates
(Kotlin/Swift camelCase, C# PascalCase). Only declarations are extracted, never
raw text: generated KDoc carries seam names in prose (e.g. `providerBaseUrl`'s
docs reference `set_provider_base_urls`), so a substring grep over the
production tree would false-red on documentation.

Languages: `kotlin` (android), `csharp` (windows), and `swift` (apple) are all
implemented and red-verified on their own machines — each leg's entry is
added to DECL_PATTERNS and red-verified before wiring its recipe: shipping an
untested regex is worse than the explicit absence.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# ── The floor list ───────────────────────────────────────────────────────────
# Every test-helpers-gated UniFFI export whose name does NOT follow the
# `*_for_test` / `test_*` naming convention. Rust snake_case (types in
# CamelCase, as declared); matching is underscore-stripped + case-insensitive.
#
# This list is hand-seeded but MACHINE-CHECKED in both directions on every
# flavored bindgen run (presence in test, absence in production), so it cannot
# rot silently — which is what distinguishes it from the hand-kept
# feature→methods map this mechanism was chosen over. Adding a seam here is
# the "state its witness" step the forward-watch requires of every new
# non-test-named seam.
KNOWN_SEAMS: list[dict[str, str]] = [
    # -- the dial family: socket-redirect primitives (fauna_launch_machine::dial)
    {
        "name": "set_provider_base_urls",
        "kind": "method",
        "why": "installs the provider base-URL override map at runtime; mirrors "
        "its 'nest' entry into the process-global launch dial seam",
    },
    {
        "name": "resolved_nest_dial_url",
        "kind": "method",
        "why": "reads the launch dial seam back (harness observability of the "
        "socket-redirect state)",
    },
    # -- the generic bridge doors: name-dispatched calls into the machine
    {
        "name": "call_machine_method",
        "kind": "method",
        "why": "name-dispatched automation door into OnboardingMachine (the "
        "native E2E bridge's generic verb)",
    },
    {
        "name": "call_machine_method_with_result",
        "kind": "method",
        "why": "call_machine_method with a JSON result channel",
    },
    # (call_machine_free_method is cfg-gated the same way but is NOT
    # uniffi::export'ed — app-side Rust calls it directly — so it never appears
    # in any binding face and is out of this witness's scope; the shared-Rust
    # visibility gates + strings-grep witnesses cover it.)
    {
        "name": "set_dns_creds",
        "kind": "method",
        "why": "installs DNS-provider credentials without the wizard UI path",
    },
    {
        "name": "create_mls_group",
        "kind": "method",
        "why": "MLS group bootstrap bypassing welcome/key-package distribution "
        "(fixture setup; found by this witness's own discovery assert, "
        "2026-08-13 — the name predates the *_for_test convention)",
    },
    # -- process-execution redirect (fauna-ffi sync_agent_provisioning)
    {
        "name": "FfiChildAgentSpawner",
        "kind": "type",
        "why": "spawner whose spawn_agent() execs the path read from "
        "FAUNA_E2E_SYNC_AGENT_BIN — a process-execution redirect",
    },
    # -- process-wide fake-clock overrides (testing.md convention 14: a
    # staleness proof moves the clock, never sleeps out the real window).
    # Found by this witness's own discovery assert on the windows (csharp)
    # leg, 2026-08-22 — neither is *ForTest-named, and both are process-wide
    # with no auto-reset (a leaked offset silently lapses the next real pass).
    {
        "name": "set_delegation_clock_offset_secs",
        "kind": "method",
        "why": "fakes the wall clock fauna_atproto_settings_machine::delegation_clock "
        "reads when minting an atproto delegation cert",
    },
    {
        "name": "set_backup_audit_clock_offset_secs",
        "kind": "method",
        "why": "fakes the wall clock fauna_client_backup::audit_clock reads for the "
        "backup_audit_run_now agent command's staleness proof",
    },
    # -- desktop agent test-only pull-pass poke (fauna-ffi sync_agent_provisioning)
    {
        "name": "custodian_run_pass_now",
        "kind": "method",
        "why": "runs ONE custodian pull pass on this device's hosted replica on "
        "demand for the custodian_pull_run_now agent command; the FFI face of "
        "fauna_client_sync::agent::SyncAgentProvisioner::custodian_run_pass_now, "
        "itself test-only (testing.md convention 15)",
    },
    {
        "name": "FfiCustodianPassReport",
        "kind": "type",
        "why": "the reply record custodian_run_pass_now returns; test-only twin "
        "of fauna_ipc::sync::CustodianPassReport",
    },
    # Found by this witness's discovery assert on the first android RELEASE run
    # since they landed (2026-10-06): the per-tree asserts never see a
    # non-test-named seam, and the pair check only runs on a paired build.
    # Each is called from all the apps' agents and e2e verbs, so the
    # `*_for_test` rename is a seven-app change; the floor entry is the witness.
    # -- the message-banner log (fauna-conversations notification.rs): the
    # harness's observation of which banners a pass fired
    {
        "name": "banner_pass_started",
        "kind": "method",
        "why": "opens a banner pass in the process-wide fired-banner log the "
        "harness reads back",
    },
    {
        "name": "banner_pass_completed",
        "kind": "method",
        "why": "closes a banner pass in the fired-banner log",
    },
    {
        "name": "record_fired_banner",
        "kind": "method",
        "why": "appends a fired banner to the harness's fired-banner log",
    },
    {
        "name": "message_banners_json_text",
        "kind": "method",
        "why": "reads the fired-banner log back as JSON for the harness",
    },
    # -- generic automation doors
    {
        "name": "call_machine_method_async",
        "kind": "method",
        "why": "async twin of call_machine_method: the name-dispatched "
        "automation door into OnboardingMachine",
    },
    {
        "name": "inject_inbound_from_test_json",
        "kind": "method",
        "why": "injects a synthetic inbound conversation message into "
        "ConversationsManager, bypassing the transport",
    },
    {
        "name": "device_set_state_json",
        "kind": "method",
        "why": "a real plane-content read: the account runtime's per-device "
        "set state as JSON",
    },
    # -- process-wide fake clocks (testing.md convention 14), same class as
    # the delegation/backup-audit pair above
    {
        "name": "set_trust_clock_offset_secs",
        "kind": "method",
        "why": "fakes the wall clock the nest-trust facet reads",
    },
    {
        "name": "offline_share_advance_clock",
        "kind": "method",
        "why": "fakes the offline-share ceremony clock",
    },
    # -- offline-share pump instrumentation (fauna-ffi offline_share.rs)
    {
        "name": "offline_share_hold_serves",
        "kind": "method",
        "why": "parks this seat's next share serve unanswered, so a journey "
        "can cut a transfer part-way",
    },
    {
        "name": "share_serve_tally",
        "kind": "method",
        "why": "reads the process-wide share-serve tally back",
    },
    {
        "name": "offline_share_probe_set",
        "kind": "method",
        "why": "dials a peer as this seat, claims a share set in the admission "
        "exchange and reports what the peer serves, without acting on it",
    },
]

# ── Declaration extraction ───────────────────────────────────────────────────
# Per-language: file glob, method-declaration regex, type-declaration regex.
# Regexes anchor on declaration keywords so prose in generated doc comments
# never matches. Generated code is uniformly machine-formatted, which is what
# makes the indentation-based member attribution below reliable.
DECL_PATTERNS = {
    "kotlin": {
        "glob": "**/*.kt",
        # `fun name(` with optional visibility/modifiers; uniffi backticks
        # keyword-colliding names (fun `open`(...)).
        "method": re.compile(
            r"^(\s*)(?:(?:public|internal|private|protected|open|override|"
            r"suspend|actual|inline|operator)\s+)*fun\s+`?([A-Za-z0-9_]+)`?\s*\("
        ),
        "type": re.compile(
            r"^(\s*)(?:(?:public|internal|private|protected|open|abstract|"
            r"sealed|data|enum|annotation|value)\s+)*"
            r"(?:class|interface|object)\s+`?([A-Za-z0-9_]+)`?"
        ),
        # Comment lines — never carry declarations; skipping them keeps a
        # commented-out `fun` (KDoc example code) from registering.
        "comment": re.compile(r"^\s*(?://|\*|/\*)"),
        # Pure-data types: uniffi records (`data class`), enums (`enum class`)
        # and enums-with-fields / errors (`sealed class`, whose variants are
        # nested inside it). A matching line is also a `type` match; this only
        # marks it as data (see `is_accounted_type`). Optional per language:
        # without it every type is treated as possibly callable.
        "data_type": re.compile(
            r"^\s*(?:(?:public|internal|private|protected)\s+)*"
            r"(?:data|enum|sealed)\s+class\s"
        ),
    },
    "swift": {
        "glob": "**/*.swift",
        # uniffi-bindgen-swift emits a method three ways, all real: a bare
        # `func name(` with NO visibility keyword at all for a PROTOCOL
        # REQUIREMENT (only ever seen inside a `public protocol …Protocol { }`
        # body — this witness only ever scans machine-generated bindgen
        # output, so a bare match is safe here); `open func name(` for the
        # concrete class's implementation of that requirement; and
        # `public func name(` for a free function (the clock-offset pair).
        # `public static func` is the one observed two-modifier combo.
        # uniffi backticks keyword-colliding names (`func \`open\`(…)`), same
        # convention as the kotlin entry above.
        "method": re.compile(
            r"^(\s*)(?:(?:public|open|fileprivate|private|internal|static|"
            r"final|override|mutating|nonisolated)\s+)*"
            r"func\s+`?([A-Za-z0-9_]+)`?\s*(?:<[^>(]*>)?\s*\("
        ),
        "type": re.compile(
            r"^(\s*)(?:(?:public|open|fileprivate|private|internal|final)\s+)*"
            r"(?:class|protocol|struct|enum)\s+`?([A-Za-z0-9_]+)`?"
        ),
        # `///` doc comments (uniffi's KDoc-equivalent prose — carries seam
        # names too), plain `//`, and `/*`/`*` block-comment lines.
        "comment": re.compile(r"^\s*(?:///|//|\*|/\*)"),
    },
    "csharp": {
        "glob": "**/*.cs",
        # uniffi-bindgen-cs has no unambiguous declaration keyword (kotlin's
        # `fun` has no C# equivalent), so a declaration is recognized by SHAPE:
        # an optional run of modifiers, then a TWO-token `ReturnType Name(`
        # pair at line start. A bare call site (`Foo(x);`), a dotted call
        # (`obj.Foo(x);`), and an assignment (`var x = Foo(y);`) are all ONE
        # identifier immediately before `(` or `=` — never `Type Name(` — so
        # they don't match. The negative lookahead excludes the one class of
        # two-token false positive this shape allows: a control-flow/statement
        # keyword directly before a call inside a method body
        # (`return Foo(x);`, `await Bar(y);`), which would otherwise misread
        # as a declaration of method `Foo`/`Bar`. Constructors and finalizers
        # (`public Foo(...)`, `~Foo()`) are exactly ONE token before `(` and
        # so never match the method pattern either — by design, not by
        # exclusion (this witness only cares about named methods, never
        # ctors/dtors).
        "method": re.compile(
            r"^(\s*)(?:(?:public|internal|private|protected|static|abstract|"
            r"override|virtual|sealed|async|new|unsafe|partial|readonly)\s+)*"
            r"(?!(?:return|if|while|foreach|using|lock|switch|throw|yield|"
            r"catch|else|await|typeof|checked|unchecked|fixed|for|do|try|"
            r"finally|case|base|this)\b)"
            r"[A-Za-z_][A-Za-z0-9_.]*(?:<[^()]*>)?\??(?:\[\])?\s+"
            r"([A-Za-z_][A-Za-z0-9_]*)\s*\("
        ),
        "type": re.compile(
            r"^(\s*)(?:(?:public|internal|private|protected|static|abstract|"
            r"sealed|partial|readonly)\s+)*"
            r"(?:class|interface|struct|enum|record)\s+([A-Za-z_][A-Za-z0-9_]*)"
        ),
        # `///` XML doc comments (uniffi's KDoc-equivalent prose — carries seam
        # names too, e.g. "unlike [`FfiChildAgentSpawner`]"), plain `//`, and
        # `/*`/`*` block-comment lines.
        "comment": re.compile(r"^\s*(?:///|//|\*|/\*)"),
    },
}


# A doc block can CLOSE on the same line as the declaration it documents:
#
#     */ fun `setDelegationClockOffsetSecs`(`offsetSecs`: kotlin.Long)
#
# The comment patterns above anchor on a leading `*`, so such a line reads as
# pure comment and the declaration on it is NEVER SEEN. That is a false-GREEN on
# an absence witness — the one direction this script exists to prevent — because
# a leaked seam sitting on such a line is invisible to the production-absence and
# difference-coverage asserts alike.
#
# Measured 2026-08-22 on android's staged debug tree: **718** declarations sat
# behind a same-line `*/`, against a witness whose green baseline was 809
# test-face methods. It was seeing roughly half the surface it reported on. The
# shape appears whenever uniffi's output is NOT run through ktlint — the bindgen
# only warns when ktlint is absent ("Unable to auto-format … No such file or
# directory") and carries on — so how much of the face this witness could see
# depended on whether a formatter happened to be installed on the machine.
#
# So blank the comment half instead of dropping the line, PRESERVING COLUMNS: the
# scan uses leading-whitespace width to decide which type encloses a method, and
# cutting the prefix would reindent a member to column 0 and orphan it. Greedy
# `.*` so a line closing several blocks keeps only what follows the LAST `*/`.
BLOCK_COMMENT_HEAD = re.compile(r"^.*\*/")


def norm(name: str) -> str:
    return name.replace("_", "").lower()


def is_test_named(normed: str) -> bool:
    """The naming-convention classes the suffix witnesses already cover."""
    return normed.endswith("fortest") or normed.startswith("test")


def extract_decls(trees: list[Path], lang: str) -> dict[str, set[str]]:
    """Scan one flavor's generated binding tree(s).

    A flavor may stage generated code under more than one root (android:
    `<java>/uniffi` + `<java>/com/fauna/ffi`, per uniffi.toml's package_name
    mapping) — pass each root; hand-written app code (e.g. the debug source
    set's TestAgent) must NOT be under any of them, since the witness is about
    the generated binding FACE, not what app code does with it.

    Returns {"methods", "types", "members_of", "top_level", "data_types"},
    every name normalized. "members_of" maps each method name to the set of
    enclosing-type names it was declared inside (indentation-based: a method
    indented deeper than the most recent type declaration belongs to it —
    reliable on machine-formatted generated code); "top_level" holds the method
    names declared at least once OUTSIDE any type. Membership only ever
    ACCOUNTS for a diff-only member of a floor-listed or test-named type, and
    never excuses a name that also has a top-level declaration. "data_types"
    holds the type names whose every declaration is pure data (the language's
    `data_type` pattern, or nested inside such a type — an enum's variants).
    """
    pats = DECL_PATTERNS[lang]
    methods: set[str] = set()
    types: set[str] = set()
    members_of: dict[str, set[str]] = {}
    top_level: set[str] = set()
    data_decls: set[str] = set()
    callable_decls: set[str] = set()
    data_pat = pats.get("data_type")
    for f in sorted(f for tree in trees for f in tree.glob(pats["glob"])):
        # (indent, normalized name, is pure data)
        type_stack: list[tuple[int, str, bool]] = []
        for raw in f.read_text(encoding="utf-8", errors="replace").splitlines():
            line = (
                BLOCK_COMMENT_HEAD.sub(lambda m: " " * len(m.group(0)), raw)
                if "*/" in raw
                else raw
            )
            if pats["comment"].match(line):
                continue
            tm = pats["type"].match(line)
            if tm:
                indent, name = len(tm.group(1)), norm(tm.group(2))
                types.add(name)
                while type_stack and type_stack[-1][0] >= indent:
                    type_stack.pop()
                is_data = bool(data_pat and data_pat.match(line)) or any(
                    d for _, _, d in type_stack
                )
                (data_decls if is_data else callable_decls).add(name)
                type_stack.append((indent, name, is_data))
                continue
            mm = pats["method"].match(line)
            if mm:
                indent, name = len(mm.group(1)), norm(mm.group(2))
                methods.add(name)
                enclosing = {t for i, t, _ in type_stack if i < indent}
                if enclosing:
                    members_of.setdefault(name, set()).update(enclosing)
                else:
                    top_level.add(name)
    return {
        "methods": methods,
        "types": types,
        "members_of": members_of,
        "top_level": top_level,
        "data_types": data_decls - callable_decls,
    }


def floor_normed() -> tuple[set[str], set[str]]:
    """(normalized floor method names, normalized floor type names)."""
    ms = {norm(e["name"]) for e in KNOWN_SEAMS if e["kind"] == "method"}
    ts = {norm(e["name"]) for e in KNOWN_SEAMS if e["kind"] == "type"}
    return ms, ts


def covered_by_floor_type(name: str, member_types: set[str], floor_types: set[str]) -> bool:
    """A diff-only name is accounted for by a floor TYPE entry when it is that
    type itself, one of uniffi's generated companions (TInterface / TImpl /
    FfiConverter*T lowering plumbing, or C#'s `I`+T companion interface — same
    object-type triple, opposite affix: kotlin suffixes, uniffi-bindgen-cs
    prefixes), or a member declared inside it."""
    for t in floor_types:
        if name in (t, t + "interface", t + "impl", "i" + t):
            return True
        if name.startswith("fficonverter") and name.endswith(t):
            return True
    return bool(member_types & floor_types)


def is_test_named_type(name: str) -> bool:
    """A test-named object type or one of its kotlin companions
    (`FooForTestInterface` / `FooForTestImpl`): `is_test_named` reads only the
    end of the name, so without this the companion of a `*ForTest` type reads
    as an unwitnessed seam."""
    return is_test_named(name) or any(
        name.endswith(s) and is_test_named(name[: -len(s)]) for s in ("interface", "impl")
    )


def is_uniffi_scaffolding(name: str) -> bool:
    """uniffi's `FfiConverter*` lowering objects. A converter has no entry point
    of its own: it is generated because some type or function in the face uses
    the type it lowers (`FfiConverterMapStringULong` for a `HashMap<String, u64>`
    field), and that type or function sits in the same difference set and is
    held to the witness there. So the converter never owes one itself."""
    return name.startswith("fficonverter")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--lang", required=True, choices=sorted(DECL_PATTERNS))
    ap.add_argument(
        "--test-tree",
        type=Path,
        action="append",
        help="test-flavored generated tree root (repeatable — a flavor may "
        "stage under several roots)",
    )
    ap.add_argument(
        "--production-tree",
        type=Path,
        action="append",
        help="production-flavored generated tree root (repeatable)",
    )
    args = ap.parse_args()
    if not args.test_tree and not args.production_tree:
        ap.error("pass --test-tree and/or --production-tree")
    for label, trees in (("test", args.test_tree), ("production", args.production_tree)):
        for tree in trees or []:
            if not tree.is_dir():
                # Strict on purpose: a typo'd path scanning nothing would be a
                # vacuously green witness.
                print(f"ERROR: {label} tree {tree} is not a directory", file=sys.stderr)
                return 2

    floor_methods, floor_types = floor_normed()
    failures: list[str] = []

    test_label = ", ".join(str(t) for t in args.test_tree or [])
    prod_label = ", ".join(str(t) for t in args.production_tree or [])
    test_decls = prod_decls = None
    if args.test_tree:
        test_decls = extract_decls(args.test_tree, args.lang)
        all_test = test_decls["methods"] | test_decls["types"]
        # test-tree presence: the floor list names real, still-testable seams.
        for entry in KNOWN_SEAMS:
            n = norm(entry["name"])
            pool = test_decls["types"] if entry["kind"] == "type" else test_decls["methods"]
            if n not in pool:
                failures.append(
                    f"floor seam `{entry['name']}` ({entry['kind']}) is NOT declared in the "
                    f"test-flavored tree {test_label} — renamed, deleted, or no longer "
                    f"exported. Update KNOWN_SEAMS in {Path(__file__).name} to match "
                    f"reality (a floor list watching a dead name is no witness)."
                )
        if not any(m.endswith("fortest") for m in all_test):
            failures.append(
                f"test-flavored tree {test_label} declares NO *ForTest method at all — "
                f"did test-helpers drop off the test flavor's feature list?"
            )

    if args.production_tree:
        prod_decls = extract_decls(args.production_tree, args.lang)
        all_prod = prod_decls["methods"] | prod_decls["types"]
        # production-tree absence: a dead gate ships the seam into this tree.
        for entry in KNOWN_SEAMS:
            n = norm(entry["name"])
            if n in all_prod:
                failures.append(
                    f"floor seam `{entry['name']}` IS declared in the PRODUCTION-flavored "
                    f"tree {prod_label} — a cfg gate is dead (e2e-automation-surface-gating.md "
                    f"point 15: the production artifact must carry no way to reach this)."
                )
        dead_fortests = sorted(m for m in all_prod if m.endswith("fortest"))
        if dead_fortests:
            failures.append(
                f"production-flavored tree {prod_label} declares *ForTest "
                f"names: {', '.join(dead_fortests)}"
            )

    if test_decls is not None and prod_decls is not None:
        # difference coverage: (test − production) must be fully accounted for.
        diff_methods = test_decls["methods"] - prod_decls["methods"]
        diff_types = test_decls["types"] - prod_decls["types"]
        unaccounted = []
        for name in sorted(diff_methods):
            if is_test_named(name) or name in floor_methods:
                continue
            # A generic member name (`json()`, `observe()`) is classified by the
            # type that declares it — never by the name alone, and never when
            # the same name is also declared at top level.
            members = (
                set()
                if name in test_decls["top_level"]
                else test_decls["members_of"].get(name, set())
            )
            if covered_by_floor_type(name, members, floor_types):
                continue
            if any(is_test_named_type(t) for t in members):
                continue
            unaccounted.append(name + "()")
        for name in sorted(diff_types):
            if is_test_named_type(name) or covered_by_floor_type(name, set(), floor_types):
                continue
            if is_uniffi_scaffolding(name):
                continue
            # A pure-data type (record, enum, enum variant) carries no entry
            # point: it is in the difference only because a function or object
            # there takes or returns it, and that one is held to the witness.
            # Methods a record or enum exports are methods, checked above.
            if name in test_decls["data_types"]:
                continue
            unaccounted.append(name)
        if unaccounted:
            failures.append(
                "declarations present ONLY in the test-flavored tree but neither "
                "test-named nor floor-listed — new seam(s) that have not stated their "
                f"witness: {', '.join(unaccounted)}. Add each to KNOWN_SEAMS in "
                f"{Path(__file__).name} with a one-line reason (or fix its name to the "
                "*_for_test convention)."
            )

    if failures:
        print(
            "FFI seam-diff witness FAILED (e2e-automation-surface-gating.md, convention 15):",
            file=sys.stderr,
        )
        for f in failures:
            print(f"  ✗ {f}", file=sys.stderr)
        return 1

    ran = []
    if test_decls is not None:
        ran.append(f"floor∈test ({len(test_decls['methods'])} methods scanned)")
    if prod_decls is not None:
        ran.append(f"floor∉production ({len(prod_decls['methods'])} methods scanned)")
    if test_decls is not None and prod_decls is not None:
        ran.append("difference fully accounted")
    print(f"FFI seam-diff witness ({args.lang}): {'; '.join(ran)} ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
