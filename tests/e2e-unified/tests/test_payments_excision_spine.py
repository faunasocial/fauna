"""The gated-feature registry's compile-time excision spine.

`docs/goal/architecture/dynamic-features.md` § The cargo feature spine + § What
"completely compiled away" means + § The App-Store escape hatch.

Named for `payments` because that was the first member to get a cargo feature
(2026-08-10); it covers **every** member's spine, and `zaps` joined the same day
as the first SUBSET member. The filename is kept deliberately rather than
generalized: it is cited from `dynamic-features.md` and from planning notes that
this module's authors cannot edit, so a rename would strand those references.

The charter's promise is that the day Apple blocks an update over a member, a
**known, perpetually-green** build flavor ships the app with the feature — and
the very ability to turn it on — compiled away. A flavor nobody builds is a
flavor that has rotted by the time it is needed, so these pins hold the
structural half in place; `just ffi-store-safe-check` holds the artifact half
(and runs in the merge-gate check tier).

**The subset edge is a second, independent property.** `zaps = ["payments"]`
means excising the superset excises the subset, while excising the subset alone
stays possible. Manifest text can only prove the first half; the recipe's Damus
column (payments on, zaps off) is what proves the second, and without it `zaps`
could be an alias of `payments` with every pin here still green.

Pure text analysis of the manifests and the justfile — no build, no driver.
The sibling module `test_ffi_flavor_split.py` pins convention 15's automation
seams the same way and is the template this follows; the two share the "a
consumer never names the feature on a dep line" rule (b), because they are the
same mechanism applied to two different kinds of surface.

**The failure mode this exists to catch, measured rather than imagined.** The
first store-safe `libfauna_ffi.so` built perfectly green and still contained
all six `fauna.payments.*` kind strings: `payments` had been made a DEFAULT
feature of `fauna-protocol`, and 17 crates depend on that crate with default
features on, so cargo's additive unification switched it back on no matter what
the flavor root asked for. Depth is the whole issue — hence
`test_payments_is_not_a_default_feature_of_the_deep_wire_crate` below.
"""

import functools
import importlib.util
import re
import tomllib
from pathlib import Path

import pytest
import yaml

from helpers.manifest_seams import shipping_dep_lines as _shipping_dep_lines

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

#: The justfile with `\`-continuations joined, so a `for pat in …` list that wraps
#: reads as one line. Two of the store-safe recipes wrap today; a scanner that
#: missed their tail would silently watch half a pattern list.
_JUSTFILE_TEXT = re.sub(r"\\\n\s*", " ", _JUSTFILE.read_text(encoding="utf-8"))

_UI_YAML = _REPO / "tests/e2e-unified/ui.yaml"
_UI_IDS_SWIFT = _REPO / "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/UiIds.swift"


@functools.lru_cache(maxsize=1)
def _gated_catalog() -> dict:
    """ui.yaml's `gated_features:` block — the one id→feature source of truth.

    Owner: `dynamic-features.md` § Which element IDs belong to a gated feature.
    Read here rather than re-listed so this module cannot become the seventh copy
    of a prefix list.

    ⚠ Both the cache and the loader choice are load-bearing for the merge path,
    and each was worth more than every other line in this file put together
    (profiled 2026-09-05: this gate was the whole gate phase at
    8.07 s once the runner made everything else concurrent). Seven tests call
    this, and it re-parsed the ~300 KB `ui.yaml` for each of them — **9.8 s of a
    17.3 s profiled run, over half the gate** — through PyYAML's pure-Python
    loader while the libyaml one sits installed beside it. `lru_cache` makes it
    one parse, `CSafeLoader` makes that parse ~13x cheaper, and the returned dict
    is read-only by every caller (a mutation would leak across tests, which is
    why nothing here may start mutating it). Same finding, same fix, as
    `lint-ui-actual.py` and `lint_winui_datatemplate_names.py` — this is the
    third gate in the family to be caught doing it.
    """
    loader = getattr(yaml, "CSafeLoader", yaml.SafeLoader)
    parsed = yaml.load(_UI_YAML.read_text(encoding="utf-8"), Loader=loader)
    return parsed.get("gated_features") or {}


def _gated_ids(feature: str) -> set[str]:
    """Every declared element id the named member paints, resolved by prefix.

    The id universe is read from the generator's own PYTHON target — the one
    surface that is never gated (the harness's copy ships in no artifact) — rather
    than re-derived from ui.yaml here. That makes "declared" mean exactly what the
    generator means by it, so this pin cannot drift from the thing it measures, and
    a prose mention of an id somewhere in ui.yaml cannot fake one into existence.
    """
    prefixes = tuple(_gated_catalog()[feature]["id_prefixes"])
    table = (_REPO / "tests/e2e-unified/generated/ui_ids.py").read_text(encoding="utf-8")
    return {m for m in re.findall(r'^[A-Z0-9_]+ = "([^"]+)"$', table, re.M) if m.startswith(prefixes)}


def _paint_tokens(prefixes: tuple[str, ...], case: str) -> tuple[str, ...]:
    """Both spellings by which an app source can PAINT one of these ids.

    ⚠ Since the element-id constant adoption there are two — the string literal
    and the generated constant `Ids.<member>` — and a source-shaped excision pin
    that watches only the literal STOPS COVERING an app on the day that app
    adopts, silently and with every assertion still green. Measured on apple the
    day this helper was written: `Views/SubscriptionSettingsView.swift` paints
    payments ids and holds not one literal, so both apple pins were reading it as
    carrying nothing. It happens to hold the condition — luck, not coverage, and
    luck is what a pin exists to replace. android's leg is in flight, so its pin
    is fixed here before adoption lands rather than after.

    Both spellings stay PREFIX-shaped (`Ids.subscriptionProvider` matches every
    member derived from a `subscription-provider-*` id), so a new element is
    covered the day it is added, whichever way the app spells it. The
    id→member-name rule is imported from `scripts/ui_id_names.py` — the module the
    generator and the two ui lints already share for exactly this "recognise a
    constant reference as an implementation" reason — so it cannot drift from
    what is emitted.
    """
    spec = importlib.util.spec_from_file_location(
        "ui_id_names", _REPO / "scripts" / "ui_id_names.py"
    )
    names = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(names)
    derive = getattr(names, case)
    return prefixes + tuple(f"Ids.{derive(p.rstrip('-'))}" for p in prefixes)


def _witness_id_prefixes() -> set[str]:
    """Element-id prefixes the store-safe recipes' `for pat in …` lines watch.

    Criterion 2's two axes share those lines — kind-string patterns
    (`fauna\\.payments\\.`) and web's wasm face names (`paymentsProvidersSet`) — and
    both are skipped BY SHAPE rather than by a name list, so a new kind family or a
    new face needs no edit here: an element-id prefix is kebab-case and ends on the
    separator, which is what makes it a prefix rather than an id.
    """
    found: set[str] = set()
    for line in _JUSTFILE_TEXT.splitlines():
        if not line.strip().startswith("for pat in "):
            continue
        for token in re.findall(r"'([^']+)'", line):
            if re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*-", token):
                found.add(token)
    return found

# The flavor ROOTS: crates a build flavor is built FROM, where
# `--no-default-features` is a real switch rather than something a unifying
# consumer can undo. These carry `payments` default-ON.
_FLAVOR_ROOTS = ["libs/fauna-ffi", "libs/fauna-wasm"]

# Crates that own a `zaps` surface and therefore declare the subset edge. The
# wire crate holds the edge itself (`zaps = ["payments"]`); the roots restate
# `payments` because their own `zaps` also forwards the client crate.
_SUBSET_OWNERS = ["libs/fauna-ffi", "libs/fauna-wasm"]

# Registry members that have a cargo feature today (`dynamic-features.md`
# § Charter members is the authority for membership itself). Kept as a list
# rather than a hard-coded pair so adding a member is one edit here, not a
# rewrite of the complement assertion.
#
# `p2p-share` joined 2026-08-17 — it was born gated at the nest and at tui, as
# this comment predicted, and the list simply had not been told. That left BOTH
# equality assertions below stale, and a stale list here is invisible until
# the module runs.
_REGISTRY_FEATURES = ["payments", "zaps", "p2p-share"]

# Members a root legitimately does NOT carry, with the reason — never inferred
# from a manifest's silence.
#
# The distinction is the whole point of the equality assertion: an absence read
# off the manifest cannot tell "this root exports no surface for that plane yet"
# from "the feature was dropped and this root now ships the plane
# unconditionally", and the second is exactly the regression the assertion
# exists to catch. Declaring it here costs one line and fails loudly when the
# face lands (see `test_every_declared_member_absence_is_real`).
_ROOT_ABSENCES: dict[str, dict[str, str]] = {
    "libs/fauna-wasm": {
        "p2p-share": "no wasm face for the share leg: the wasm graph has no QUIC "
        "seam (p2p.md § Wormability walk rule 5).",
    },
}


def _expected_members(root: str) -> list[str]:
    """The registry members this root must carry — the charter list minus its
    declared absences. Silence in a manifest is never an answer here."""
    absent = _ROOT_ABSENCES.get(root, {})
    return [m for m in _REGISTRY_FEATURES if m not in absent]


def _manifest(rel: str) -> str:
    return (_REPO / rel / "Cargo.toml").read_text(encoding="utf-8")


def _feature_list(manifest: str, feature: str) -> str | None:
    """The bracketed body of a `[features]` entry, or None if absent."""
    m = re.search(rf"^{re.escape(feature)}\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    return m.group(1) if m else None


# `_shipping_dep_lines` is imported from `helpers.manifest_seams` above — see
# that module's docstring for why (a multi-line `features = [...]` entry used
# to defeat this scan, including the `optional = true` checks below whenever
# that clause landed on a continuation line rather than the dep's own first
# line).


def _recipe_body(name: str) -> str:
    """A recipe's body, resolved through the slot-lock delegation wrapper.

    Since 2026-08-20 (the `{{slot_build}} just _<name>-impl` ruling), every `*-store-safe-check` public recipe is a
    ONE-LINE delegation to its own `_<name>-impl`, which carries the actual
    build columns these witnesses pin. Reading the wrapper's own one-line body
    would see none of them, so a body whose LAST non-blank line is PURELY a
    delegation call resolves through it — generically, by shape, not by a
    per-caller hard-coded `_impl` name (which would just move the coupling
    here instead of removing it). Any preceding lines are kept and the
    delegate's body is appended after them — the `_apple-ffi-flavor`
    split (2026-08-25) keeps a cache-lookup fast path ahead of its own
    delegate call, deliberately outside the slot lease, so this can no longer
    assume the whole body is the one delegation line.
    """
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    body = m.group(1)
    lines = [line for line in body.splitlines() if line.strip()]
    if lines:
        delegate = re.match(r"^\s*(?:\{\{slot_build\}\}|\{\{slot_e2e\}\})?\s*just\s+(_\S+)\s*$", lines[-1])
        if delegate:
            prefix = "\n".join(lines[:-1])
            return (prefix + "\n" if prefix else "") + _recipe_body(delegate.group(1))
    return body


def _recipe_header(name: str) -> str:
    """The recipe's own `name deps...:` line, i.e. its dependency list.

    Same helper as `test_ffi_flavor_split.py`'s: a recipe that takes its FFI
    flavor as a *dependency* (apple's do) carries that half on this line, not in
    the body.
    """
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*$", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(0)


# ── the depth rule: default-on at the roots, opt-in in the deep crates ───────


def test_payments_is_not_a_default_feature_of_the_deep_wire_crate():
    """`fauna-protocol` must NOT put `payments` in `default`.

    This is the pin for the measured failure above. The goal doc's "default-on
    in its owning shared crates" is implementable at a flavor root and NOT at
    this depth: cargo features are additive and unified, so one of the many
    consumers taking fauna-protocol with default features re-enables the plane
    and `--no-default-features` at the root excises nothing. Putting it back in
    `default` here compiles fine, passes every type check, and silently makes
    the escape hatch a no-op — which is exactly why it needs a test rather than
    a comment.
    """
    manifest = _manifest("libs/fauna-protocol")
    default = _feature_list(manifest, "default")
    assert default is not None, "libs/fauna-protocol lost its `default` feature list"
    assert "payments" not in default, (
        "libs/fauna-protocol/Cargo.toml put `payments` back into `default`. Cargo "
        "features are additive and unified, and many crates depend on fauna-protocol "
        "with default features on, so a default-on `payments` HERE is re-enabled by "
        "any of them — the store-safe flavor then builds green and still ships every "
        "`fauna.payments.*` kind string (measured 2026-08-10). Default-on belongs at "
        f"the flavor roots ({', '.join(_FLAVOR_ROOTS)}), opt-in here. "
        "dynamic-features.md § The cargo feature spine."
    )
    assert _feature_list(manifest, "payments") is not None, (
        "libs/fauna-protocol lost its `payments` feature entirely — the "
        "`fauna.payments.*` / `fauna.tips.*` wire planes would then be unconditional "
        "and no flavor could excise them"
    )


# ── the subset edge: `zaps` requires `payments`, declared once ───────────────


@pytest.mark.parametrize("crate", _SUBSET_OWNERS)
def test_zaps_is_declared_a_subset_member_of_payments(crate):
    """`zaps = [... "payments" ...]` — the subset relation, as a cargo feature
    dependency rather than a rule re-derived per surface.

    Two properties ride on it. Excising `payments` must excise `zaps` with it,
    because a zap's only consequences ARE payments-plane surfaces
    (`dynamic-features.md` § Charter members). And excising `zaps` alone must
    stay possible — the Damus flavor, the finer-grained hatch the precedent
    shows a store actually demands.

    Written as a cargo dependency in exactly the crates that own a zap surface,
    so no build can express "zaps without payments" at all: the two flavor roots
    say `payments` outright, the deep crates inherit it from fauna-protocol.
    """
    zaps = _feature_list(_manifest(crate), "zaps")
    assert zaps is not None, (
        f"{crate}/Cargo.toml has no `zaps` feature, so its NIP-57 surfaces are "
        "unconditional and the Damus flavor cannot exist (dynamic-features.md "
        "§ Charter members — the `zaps` row)"
    )
    forwards_payments = "payments" in zaps or "fauna-protocol/zaps" in zaps
    assert forwards_payments, (
        f"{crate}'s `zaps` feature no longer implies `payments` — either directly "
        'or through `fauna-protocol/zaps` (which is itself `zaps = ["payments"]`). '
        "A build could then express zaps-without-payments, which the charter says "
        "is not a representable state: a zap's only consequences are payments-plane "
        f"surfaces. Got: {zaps!r}"
    )


def test_the_subset_edge_lives_in_the_wire_crate_not_only_at_the_roots():
    """The edge itself is declared in fauna-protocol, once.

    A root that spelled `zaps = ["payments", ...]` while the wire crate left
    `zaps = []` would let a deep consumer turn the zap wire plane on without the
    money plane — the relation would hold only where someone remembered to
    restate it, which is the per-surface re-derivation § Charter members
    explicitly rules out.
    """
    zaps = _feature_list(_manifest("libs/fauna-protocol"), "zaps")
    assert zaps is not None, "libs/fauna-protocol lost its `zaps` feature"
    assert "payments" in zaps, (
        'libs/fauna-protocol must declare `zaps = ["payments"]` — that single line '
        "is what makes the subset relation true of every consumer, instead of true "
        f"only where a root happens to restate it. Got: {zaps!r}"
    )


@pytest.mark.parametrize("crate", _SUBSET_OWNERS + ["libs/fauna-protocol", "libs/fauna-client-nostr"])
def test_zaps_is_not_a_default_feature_of_a_deep_crate(crate):
    """Same depth rule as `payments`, and the same measured failure behind it.

    Only the flavor roots may put `zaps` in `default`; a deep crate that did
    would have it switched back on by any consumer taking that crate with
    default features, and the Damus flavor would silently keep every
    `fauna.nostr.zap_signers.*` string while building green.
    """
    if crate in _FLAVOR_ROOTS:
        pytest.skip("flavor roots are where default-on is correct")
    default = _feature_list(_manifest(crate), "default")
    assert default is None or "zaps" not in default, (
        f"{crate}/Cargo.toml puts `zaps` in `default`. At this depth that is "
        "re-enabled by any consumer taking the crate with default features, so "
        "`--no-default-features` at the flavor root excises nothing — the measured "
        "failure recorded in this module's docstring, one member over."
    )


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_zaps_is_default_on_at_every_flavor_root(root):
    """A plain build ships zaps, exactly as it ships payments. Excision is what a
    FLAVOR asks for (`dynamic-features.md` § Goal (b))."""
    default = _feature_list(_manifest(root), "default")
    assert default is not None, f"{root} lost its `default` feature list"
    assert "zaps" in default, (
        f"{root}/Cargo.toml no longer has `zaps` in `default` — a plain build would "
        "silently ship without the NIP-57 trust root, which is a product regression, "
        "not an escape hatch"
    )


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_each_flavor_root_forwards_the_whole_zap_plane_from_its_own_feature(root):
    """Rule (b) again, for the second member: both halves ride ONE switch.

    Forwarding only `fauna-client-nostr/zaps` would drop the RPC face while the
    `fauna.nostr.zap_signers.*` kind strings stayed in the artifact — a
    half-excision that reads green to a symbol-level check and fails the
    artifact witness's Damus column.
    """
    zaps = _feature_list(_manifest(root), "zaps")
    assert zaps is not None, f"{root} has no `zaps` feature"
    assert "fauna-protocol/zaps" in zaps, (
        f"{root}'s `zaps` feature no longer forwards `fauna-protocol/zaps` — the "
        "client face would excise while the wire plane's kind strings stayed in the "
        'artifact (dynamic-features.md § What "completely compiled away" means, '
        "item 2: no wire senders)"
    )
    assert "fauna-client-nostr/zaps" in zaps, (
        f"{root}'s `zaps` feature no longer forwards `fauna-client-nostr/zaps` — the "
        "shared NostrZapSignerClient would stay compiled in"
    )


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_payments_is_default_on_at_every_flavor_root(root):
    """The user-visible posture: a plain build ships payments. Excision is what a
    FLAVOR asks for, never the default (`dynamic-features.md` § Goal (b))."""
    default = _feature_list(_manifest(root), "default")
    assert default is not None, f"{root} lost its `default` feature list"
    assert "payments" in default, (
        f"{root}/Cargo.toml no longer has `payments` in `default` — a plain build "
        "would silently ship without the money plane, which is a product regression, "
        "not an escape hatch (dynamic-features.md § Goal (b): excision is a build "
        "flavor's choice)"
    )


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_the_store_safe_feature_is_the_complement_and_not_a_second_list(root):
    """`default = ["store-safe", "payments"]` is what keeps the escape hatch from
    rotting: a new client surface is added to `store-safe` once and is in
    `default` by construction.

    The alternative — spelling the ~40-entry complement on the recipe's command
    line — rots the first time someone adds a feature and updates only one of
    the two places, and it rots INVISIBLY, since both builds still succeed.

    Parametrized over BOTH client roots 2026-08-10 (with the wasm column of the
    artifact witness). `fauna-wasm`'s `store-safe` is legitimately empty today —
    its `default` happened to contain nothing but registry members, so the bare
    `--no-default-features` spelling resolved identically — but "identical right
    now" is not the property this pin protects. Without the complement, the day
    a non-registry default feature is added to the wasm root it is silently
    dropped from the store-safe web build: a product regression that fails no
    build, since both flavors still compile. Uniform spelling across all three
    roots (priority #1) is also what lets one runbook sentence describe the
    excised flavor instead of one-per-root.
    """
    manifest = _manifest(root)
    default = _feature_list(manifest, "default")
    assert default is not None
    entries = [e.strip().strip('"') for e in default.split(",") if e.strip()]
    expected = _expected_members(root)
    assert sorted(entries) == sorted(["store-safe"] + expected), (
        f'{root}\'s `default` must be exactly ["store-safe"] plus the registry '
        f"members it carries ({expected}) — the split is what makes the excised flavor "
        "`--no-default-features --features store-safe` with no second copy of the "
        "feature list to keep in sync. A new entry here that is NOT a registry member "
        "belongs in `store-safe` instead; a new registry member belongs in BOTH this "
        "list and `_REGISTRY_FEATURES` above (and in dynamic-features.md § Charter "
        "members, which is the authority). If this root genuinely exports no surface "
        f"for a member, declare it in `_ROOT_ABSENCES` with a reason. Got: {entries}"
    )
    store_safe = _feature_list(manifest, "store-safe")
    assert store_safe is not None, (
        f"{root} lost its `store-safe` feature. It may be EMPTY (fauna-wasm's is), but "
        "it must exist: it is what makes `--no-default-features --features store-safe` "
        "the one spelling of the excised flavor at every root."
    )
    for member in _REGISTRY_FEATURES:
        assert member not in store_safe, (
            f"{root}'s `store-safe` feature forwards `{member}` — the "
            "store-safe flavor would then still ship a controversial-class plane, "
            "defeating the entire hatch"
        )


def _store_safe_closure(root: str) -> dict[str, list[str]]:
    """Every entry `store-safe` reaches through the root's own `[features]` graph,
    each mapped to the chain of local features that reaches it.

    Cargo's semantics, read off the manifest: a bare name that is a local feature
    recurses; `dep:x` enables an optional dependency and nothing else; `x/f` and
    `x?/f` switch on feature `f` of dependency `x`. Unconditional dep-line
    features are rule (b)'s business and are pinned separately below.
    """
    features = tomllib.loads(_manifest(root)).get("features", {})
    reached: dict[str, list[str]] = {}
    stack = [("store-safe", ["store-safe"])]
    while stack:
        name, chain = stack.pop()
        for entry in features.get(name, []):
            if entry in reached:
                continue
            reached[entry] = chain
            if entry in features:
                stack.append((entry, chain + [entry]))
    return reached


@pytest.mark.parametrize(
    "root", _FLAVOR_ROOTS + ["bins/fauna-nest", "apps/fauna-tui", "apps/fauna-linux"]
)
def test_no_registry_member_hides_in_the_store_safe_closure_under_another_name(root):
    """The complement test above reads `store-safe`'s own list for a member's
    NAME, and a member's surface can sit under another feature name. That is not
    hypothetical: until 2026-09-28 fauna-ffi's `store-safe` forwarded
    `offline-share` — the `p2p-share` member's ceremony half — which switches on
    `fauna-client-capabilities/p2p-share`. So the store-safe cdylib kept the
    ceremony's UniFFI face and its id-naming docstrings, with every name check
    here green (`dynamic-features.md` § Which element IDs belong to a gated
    feature, the carrier paragraph).

    What gives a member's surface away is the dependency feature it has to switch
    on, because the deep crates name their gate after the member. Walking the
    closure catches it however many local names sit in between. No build is
    needed; the manifest is the input.
    """
    reached = _store_safe_closure(root)
    offenders = sorted(
        f"{entry}  (via {' -> '.join(chain)})"
        for entry, chain in reached.items()
        if entry in _REGISTRY_FEATURES
        or ("/" in entry and entry.rsplit("/", 1)[1] in _REGISTRY_FEATURES)
    )
    assert not offenders, (
        f"{root}'s `store-safe` closure reaches a registry member's plane, so the "
        "App-Store flavor still compiles that surface in. Move the local feature "
        "that forwards it under the member's own feature (fauna-ffi's "
        "`offline-share` now rides `p2p-share` for exactly this reason): "
        f"{offenders!r}"
    )


# ── rule (b): a consumer forwards from its own feature, never a dep line ─────


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_no_flavor_root_names_the_payments_feature_on_a_dependency_line(root):
    """A dep line naming `fauna-protocol/payments` turns it on unconditionally —
    surviving `--no-default-features`, which is how convention 15's seams reached
    the release Go binding. The root must forward it from its own `payments`
    feature instead."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(root))
        if "fauna-protocol" in line and "payments" in line
    ]
    assert not offenders, (
        f"{root}/Cargo.toml names fauna-protocol's `payments` on a dependency line, "
        "which turns the wire plane on unconditionally regardless of the flavor "
        "(dynamic-features.md § The cargo feature spine, rule (b) — the same rule "
        f"convention 15 states for `test-helpers`). Offending line(s): {offenders!r}"
    )


@pytest.mark.parametrize("root", _FLAVOR_ROOTS)
def test_each_flavor_root_forwards_the_whole_plane_from_its_own_feature(root):
    """Both halves must ride ONE switch: the client crate (the RPC face) and the
    wire plane (the kind strings + types). Forwarding only the crate leaves the
    `fauna.payments.*` strings in the artifact — a half-excision that reads green
    to a symbol-level check and fails the artifact witness."""
    payments = _feature_list(_manifest(root), "payments")
    assert payments is not None, f"{root} has no `payments` feature"
    assert "dep:fauna-client-payments" in payments, (
        f"{root}'s `payments` feature no longer pulls fauna-client-payments as an "
        "optional dep — the crate would be unconditional and its RPC face would ship "
        "in the store-safe flavor"
    )
    assert "fauna-protocol/payments" in payments, (
        f"{root}'s `payments` feature no longer forwards `fauna-protocol/payments` — "
        "the client crate would excise while the wire plane's kind strings stayed in "
        "the artifact (dynamic-features.md § What \"completely compiled away\" means, "
        "item 2: no wire senders)"
    )


def test_the_payments_crates_are_optional_wherever_they_are_consumed():
    """An `optional = true` dep is the only kind cargo can actually leave out. A
    plain dep line under a gated `mod` still compiles the crate into the
    artifact — the module gate hides the API, not the code."""
    for root in _FLAVOR_ROOTS:
        offenders = [
            line
            for line in _shipping_dep_lines(_manifest(root))
            if re.search(r"^\s*fauna-client-payments\s*=", line) and "optional = true" not in line
        ]
        assert not offenders, (
            f"{root}/Cargo.toml takes fauna-client-payments as a NON-optional dep, so "
            "the crate is compiled into every flavor no matter what the module gates "
            f"say. Offending line(s): {offenders!r}"
        )


# ── the artifact witness has to exist, and be two-column ─────────────────────


def test_the_store_safe_artifact_witness_recipe_exists_and_checks_both_columns():
    """Text analysis proves what the manifests SAY; this pins the recipe that
    proves what the BINARY is — and pins it in both directions.

    A one-column "0 occurrences" assertion passes vacuously if the pattern is
    wrong, the build no-ops, or the kinds get renamed. The positive column is
    what makes the negative one mean something, exactly as
    `atproto-bridge-build-e2e` does for the seize seam.
    """
    body = _recipe_body("ffi-store-safe-check")
    assert "--no-default-features --features store-safe" in body, (
        "ffi-store-safe-check no longer builds the store-safe flavor"
    )
    assert "fauna\\.payments\\." in body and "FfiPaymentsClient" in body, (
        "ffi-store-safe-check lost one of its witness patterns — the kind strings and "
        "the UniFFI face are the two things an excised artifact must not contain"
    )
    # The positive column: a bare `cargo build -p fauna-ffi` with no feature
    # flags is the default flavor, and the recipe must assert the plane IS there.
    # `--locked` is required as of 2026-08-10 (this recipe joined _HEAVY_GATES,
    # whose test_gate_recipes_are_locked enforces it everywhere): an unlocked
    # cargo run inside the main checkout rewrites Cargo.lock and wedges every
    # later merge-gate kick on "main checkout is dirty".
    assert re.search(r"cargo build --locked -p fauna-ffi\s*$", body, re.MULTILINE), (
        "ffi-store-safe-check no longer builds the DEFAULT flavor, so its "
        "store-safe assertion is vacuous — a grep matching nothing would pass"
    )
    assert "vacuous" in body, (
        "ffi-store-safe-check lost the comment explaining why the second column "
        "exists; the next session to 'simplify' this recipe will delete the column"
    )
    # Criterion 1 on a LIBRARY root, added 2026-08-18.
    # The axis is easy to read as decoration — "a library paints nothing" — and it
    # is not: UniFFI embeds Rust docstrings in the cdylib metadata, so a doc
    # comment naming a gated element id ships the id in the artifact and in the
    # generated Swift/Kotlin face. It was live when it was found: 9 `post-tip-` +
    # 1 `subscription-claim-` lines in a store-safe `libfauna_ffi.so`, every one a
    # docstring on a deliberately-ungated inert surface. Prefixes are read from
    # the catalog, not re-listed, for the same reason as everything else here.
    #
    # A SUBSET is correct (a prefix no doc comment in this graph names would be
    # vacuous in both columns, exactly as `post-tip-` is for apple), so this pins
    # the two prefixes that were actually leaking and no more.
    #
    # Read from the PATTERN ARRAY, not from the recipe body: the body also
    # *discusses* `subscription-provider-` in the comment explaining why that
    # prefix is deliberately not watched here, and a body-wide substring test
    # reports the prose as coverage — the same "a mention is not an
    # implementation" trap `_paint_tokens` exists for one level down.
    array = re.search(r"^\s*PAY_ID_PATS=\(([^)]*)\)", body, re.MULTILINE)
    assert array, (
        "ffi-store-safe-check no longer defines PAY_ID_PATS, the criterion-1 "
        "element-id pattern array added 2026-08-18"
    )
    ffi_watched = set(re.findall(r"'([^']+)'", array.group(1)))
    assert {"post-tip-", "subscription-claim-"} <= ffi_watched, (
        "ffi-store-safe-check lost its criterion-1 element-id patterns. A UniFFI "
        "cdylib IS a criterion-1 surface even though it paints nothing — docstrings "
        'are part of the artifact (dynamic-features.md § What "completely compiled '
        'away" means, criterion 1: prose included). Without these patterns the '
        "residue fixed on 2026-08-18 comes back invisibly: "
        f"watched={sorted(ffi_watched)!r}"
    )
    catalog_prefixes = set(_gated_catalog()["payments"]["id_prefixes"])
    assert ffi_watched <= catalog_prefixes, (
        "ffi-store-safe-check watches an element-id prefix ui.yaml's "
        "`gated_features.payments` does not declare, so the generator emits those "
        "ids ungated while this witness expects them absent: "
        f"{sorted(ffi_watched - catalog_prefixes)!r}"
    )
    assert "PAY_ID_PATS[@]" in body, (
        "PAY_ID_PATS is defined but never expanded into a column, so the "
        "criterion-1 axis is declared and measured by nothing"
    )
    # The `p2p-share` member's axis (2026-09-28), kept apart from PAY_ID_PATS
    # because the subset check above holds that array to the PAYMENTS catalog.
    # All four catalog prefixes are live in the default cdylib, so all four are
    # watched: a prefix the catalog adds later joins the witness or reds here.
    p2p_array = re.search(r"^\s*P2P_ID_PATS=\(([^)]*)\)", body, re.MULTILINE)
    assert p2p_array, (
        "ffi-store-safe-check no longer defines P2P_ID_PATS, the `p2p-share` "
        "member's element-id axis on the cdylib"
    )
    p2p_watched = set(re.findall(r"'([^']+)'", p2p_array.group(1)))
    assert p2p_watched == set(_gated_catalog()["p2p-share"]["id_prefixes"]), (
        "ffi-store-safe-check's P2P_ID_PATS must be exactly ui.yaml's "
        "`gated_features.p2p-share.id_prefixes` (all four are live in the default "
        f"cdylib): watched={sorted(p2p_watched)!r}"
    )
    for arr in ("P2P_ID_PATS", "P2P_FACE_PATS"):
        assert body.count(f"{arr}[@]") >= 3, (
            f"{arr} must be expanded into all three columns (absent in store-safe "
            "and Damus, present in DEFAULT); fewer expansions leave one column "
            "unmeasured or its absence vacuous"
        )


def test_the_artifact_witness_has_a_damus_column_for_the_subset_edge():
    """The subset relation is only real if some artifact demonstrates it.

    Manifest text proves `zaps` *implies* `payments`; it cannot prove the other
    direction — that `payments` does NOT imply `zaps`. Only a build with
    payments on and zaps off can, and that flavor is the whole point of admitting
    `zaps` as a separate member: the Damus precedent is a store demanding
    exactly it. Without this column, `zaps` could be an alias of `payments` and
    every other pin here would still pass.
    """
    body = _recipe_body("ffi-store-safe-check")
    assert "--no-default-features --features store-safe,payments" in body, (
        "ffi-store-safe-check no longer builds the Damus flavor (payments without "
        "zaps). That column is the only evidence the subset edge is one-directional "
        "— manifest text cannot show that `payments` alone leaves zaps excised."
    )
    assert "fauna\\.nostr\\.zap_signers\\." in body, (
        "ffi-store-safe-check lost the zap kind-string witness pattern"
    )
    assert "FfiNostrZapSignerClient" in body, (
        "ffi-store-safe-check lost the zap UniFFI-face witness pattern"
    )


# ── the WASM flavor root's own column (2026-08-10) ───────────────────────────
#
# The third root was the only one no gate ever BUILT, and that is exactly how it
# came to be declared excisable while `--no-default-features` did not compile at
# all — the defect W1.5 (account-data-plane.md § Workstreams) found by hand. These pins keep the recipe that discharges
# the claim from being quietly weakened; the recipe itself is what keeps the
# FLAVOR honest. A structural pin can never replace it: manifest text is the
# thing that was already green while the build was broken.


def test_the_wasm_root_has_its_own_three_column_artifact_witness():
    """`ffi-store-safe-check` cannot cover this root, so a second witness is not
    duplication — but NOT for the tempting reason that the graphs are disjoint.
    They largely are not: of the 42 fauna-* crates in fauna-wasm's wasm32-only
    dependency block, 40 are also fauna-ffi deps (measured 2026-08-10).

    The load-bearing difference is the CFG SET. A host-target build never
    compiles `#[cfg(target_arch = "wasm32")]` code at all, and fauna-wasm's whole
    WS-RPC face (`src/rpc.rs`, gated at the `mod`) is exactly that — which is
    where the W1 defect actually lived: `rpc.rs` named `fauna_protocol::payments::`
    unconditionally, and no host build in the workspace could have seen it.
    Compounding it, each root resolves the shared crates with its own feature set
    and declares its own payments/zaps forwarding. Neither do
    `wasm-seam-check`/`wasm-chunk-check` cover it: they build the default flavor.
    """
    body = _recipe_body("wasm-store-safe-check")
    # Column 1 — store-safe, spelled with the complement feature like the other
    # two roots (see test_the_store_safe_feature_is_the_complement_and_not_a_second_list).
    assert "--no-default-features --features store-safe" in body, (
        "wasm-store-safe-check no longer builds the store-safe flavor"
    )
    # Column 2 — the Damus flavor, the only thing that can show the subset edge
    # is one-directional (payments does NOT imply zaps).
    assert "--no-default-features --features store-safe,payments" in body, (
        "wasm-store-safe-check lost its Damus column — manifest text cannot show "
        "that `payments` alone leaves zaps excised; only a build can"
    )
    # Column 3 — the default flavor, without which the absences are vacuous.
    assert re.search(
        r"cargo build --locked -p fauna-wasm --target wasm32-unknown-unknown\s*$",
        body,
        re.MULTILINE,
    ), (
        "wasm-store-safe-check no longer builds the DEFAULT flavor, so its "
        "store-safe assertions are vacuous — a grep matching nothing would pass"
    )
    assert "vacuous" in body, (
        "wasm-store-safe-check lost the comment explaining why the third column "
        "exists; the next session to 'simplify' this recipe will delete the column"
    )


def test_the_wasm_witness_greps_both_the_kind_strings_and_the_js_faces():
    """Two axes, because either alone is half a hatch.

    The kind strings are what could reach the wire; the wasm-bindgen export
    names are the callable JS faces — the web twin of the UniFFI classes the ffi
    column greps. Absence of only one leaves either a live kind behind a dead
    face or a callable face over a renamed kind.
    """
    body = _recipe_body("wasm-store-safe-check")
    for pattern in ("fauna\\.payments\\.", "fauna\\.tips\\."):
        assert pattern in body, f"wasm-store-safe-check lost the {pattern!r} kind witness"
    for pattern in ("fauna\\.nostr\\.zap_signers\\.", "nostr\\.zaps\\.total"):
        assert pattern in body, f"wasm-store-safe-check lost the {pattern!r} zap-kind witness"
    for face in ("paymentsProvidersSet", "paymentsClaimsRedeem"):
        assert face in body, f"wasm-store-safe-check lost the {face!r} JS-face witness"
    for face in ("nostrZapSignersList", "nostrZapTotal"):
        assert face in body, f"wasm-store-safe-check lost the {face!r} zap JS-face witness"


def test_the_wasm_witness_inspects_a_wasm_artifact_not_a_host_build():
    """The artifact under test must be the browser one.

    A host-target build of `fauna-wasm` would compile a different cfg set (the
    wasm32-only dependency block drops out entirely), so it could pass while the
    real bundle carried the plane — the vacuity trap one level up. The recipe
    therefore builds `--target wasm32-unknown-unknown` and greps the emitted
    `.wasm` itself.
    """
    body = _recipe_body("wasm-store-safe-check")
    build_lines = [
        line.strip()
        for line in body.splitlines()
        if "cargo build" in line and not line.strip().startswith("#")
    ]
    assert len(build_lines) == 3, (
        f"wasm-store-safe-check must have exactly three build columns, found {build_lines}"
    )
    off_target = [line for line in build_lines if "--target wasm32-unknown-unknown" not in line]
    assert not off_target, (
        "every column of wasm-store-safe-check must build for wasm32-unknown-unknown; "
        f"a host-target column proves nothing about the browser artifact: {off_target}"
    )
    assert "wasm32-unknown-unknown/debug/fauna_wasm.wasm" in body, (
        "wasm-store-safe-check no longer greps the emitted .wasm. Note the recipe "
        "deliberately uses a plain `cargo build` rather than wasm-pack: wasm-pack is "
        "a release build plus wasm-bindgen-cli plus wasm-opt (the ~30-minute "
        "`just web-check` class), and the raw cdylib already carries both witness "
        "axes — which is what makes three columns affordable on the check tier."
    )


# ── the NEST flavor root (2026-08-10, the nest-side excision) ────────────────
#
# Kept as its own section rather than folded into the parametrized client tests
# above, because the plane crates differ by SIDE: a client root forwards the RPC
# face (`fauna-client-payments`, `fauna-client-nostr`), while the nest forwards
# the server-side entitlement engine + provider-webhook waist (`fauna-payments`).
# Parametrizing the client assertions over the nest would have to special-case
# every crate name; a sibling section states the nest's own shape directly.

_NEST = "bins/fauna-nest"


def test_every_declared_member_absence_is_real():
    """A declared absence must expire on its own, or it becomes a blind spot.

    `_ROOT_ABSENCES` says a root carries no feature for a member because it
    exports no surface for that plane yet. The day the face lands, the entry
    stops being true — and a stale entry is strictly worse than no entry at all,
    because the equality assertion above would then *expect* the member to be
    missing and pass while the root shipped the plane ungated. So the entry is
    valid only while the manifest really has no such feature section.
    """
    for root, absences in _ROOT_ABSENCES.items():
        manifest = _manifest(root)
        for member, reason in absences.items():
            assert member in _REGISTRY_FEATURES, (
                f"_ROOT_ABSENCES declares {root} lacks '{member}', which is not a "
                f"registry member at all — fix the spelling or drop the entry."
            )
            assert reason.strip(), f"{root}'s '{member}' absence carries no reason"
            assert _feature_list(manifest, member) is None, (
                f"{root} now DECLARES a `{member}` feature, so its `_ROOT_ABSENCES` "
                f"entry is stale and is now hiding a real check: delete the entry and "
                f"add `{member}` to that root's `default` list. The recorded reason "
                f"was: {reason}"
            )


def test_the_nest_is_a_flavor_root_with_the_registry_members_default_on():
    """A binary crate is a flavor root by definition — `--no-default-features` on
    it is a real switch nothing can unify away. The nest carries the same posture
    as the client roots: a plain build ships the planes, and excision is what a
    FLAVOR asks for (`dynamic-features.md` § Goal (b))."""
    default = _feature_list(_manifest(_NEST), "default")
    assert default is not None, f"{_NEST} lost its `default` feature list"
    entries = [e.strip().strip('"') for e in default.split(",") if e.strip()]
    assert sorted(entries) == sorted(["store-safe"] + _expected_members(_NEST)), (
        f'{_NEST}\'s `default` must be exactly ["store-safe"] plus the registry '
        f"members it carries ({_expected_members(_NEST)}) — same complement split as the client roots, "
        "so the excised nest is `--no-default-features --features store-safe` with no "
        f"second copy of the list to keep in sync. Got: {entries}"
    )
    store_safe = _feature_list(_manifest(_NEST), "store-safe")
    assert store_safe is not None, f"{_NEST} lost its `store-safe` feature"
    for member in _REGISTRY_FEATURES:
        assert member not in store_safe, (
            f"{_NEST}'s `store-safe` feature forwards `{member}` — the store-safe nest "
            "would then still ship a controversial-class plane"
        )


def test_the_nest_names_no_registry_feature_on_a_dependency_line():
    """Rule (b) at the nest. A dep line survives `--no-default-features`, which is
    exactly how the plane would stay in a store-safe nest binary while both
    flavors still built green."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(_NEST))
        if "fauna-protocol" in line
        and any(f'"{m}"' in line for m in _REGISTRY_FEATURES)
    ]
    assert not offenders, (
        f"{_NEST}/Cargo.toml names a registry feature on the fauna-protocol dependency "
        "line, which turns that wire plane on unconditionally regardless of the flavor "
        "(dynamic-features.md § The cargo feature spine, rule (b)). Forward it from the "
        f"nest's own feature instead. Offending line(s): {offenders!r}"
    )


def test_the_nest_forwards_both_halves_of_each_plane_from_its_own_feature():
    """Both halves ride ONE switch, server-side: the engine crate and the wire
    plane. Forwarding only the engine leaves the kind strings in the binary."""
    payments = _feature_list(_manifest(_NEST), "payments")
    assert payments is not None, f"{_NEST} has no `payments` feature"
    assert "dep:fauna-payments" in payments, (
        f"{_NEST}'s `payments` feature no longer pulls fauna-payments as an optional "
        "dep — the entitlement engine would be compiled into every flavor"
    )
    assert "fauna-protocol/payments" in payments, (
        f"{_NEST}'s `payments` feature no longer forwards `fauna-protocol/payments` — "
        "the `fauna.payments.*` kind strings would stay in the store-safe binary"
    )
    zaps = _feature_list(_manifest(_NEST), "zaps")
    assert zaps is not None, f"{_NEST} has no `zaps` feature"
    assert "payments" in zaps, (
        f"{_NEST}'s `zaps` no longer requires `payments` — excising the superset must "
        "excise the subset by construction (dynamic-features.md § Charter members)"
    )
    assert "fauna-protocol/zaps" in zaps, (
        f"{_NEST}'s `zaps` no longer forwards `fauna-protocol/zaps`. Measured when the "
        "nest witness was first run: with the handlers gated but this forward missing, "
        "a store-safe nest still carried the `fauna.nostr.zap_signers.*` kind strings."
    )


def test_the_nest_zaps_feature_has_no_nostr_cargo_edge():
    """`zaps` and `nostr` compose STRUCTURALLY at the nest, by module placement —
    the zap surfaces live inside the already-`nostr`-gated `src/nostr/` subtree,
    so the reachable surface is `nostr AND zaps` with no cargo edge either way.

    A `zaps = [..., "nostr"]` edge would be actively wrong, not merely redundant:
    `zaps` is default-ON, so it would pull fauna-bridge-nostr into the DEFAULT
    nest binary — the same class of accident as a default-features change
    reaching a consumer that opted out. (And `nostr ⇒ zaps` would make the Damus
    flavor — nostr on, zaps off — unexpressible, which is the point of the subset
    member.)
    """
    zaps = _feature_list(_manifest(_NEST), "zaps")
    assert zaps is not None, f"{_NEST} has no `zaps` feature"
    assert "nostr" not in zaps.replace("fauna-protocol/zaps", ""), (
        f"{_NEST}'s `zaps` feature gained a `nostr` edge. That would compile the nostr "
        "bridge into the default nest binary, and it is unnecessary: the zap surfaces "
        "are already inside the `nostr`-gated module subtree."
    )
    nostr = _feature_list(_manifest(_NEST), "nostr")
    assert nostr is not None, f"{_NEST} has no `nostr` feature"
    assert "zaps" not in nostr, (
        f"{_NEST}'s `nostr` feature forwards `zaps` — the Damus flavor (nostr on, zaps "
        "off) would become unexpressible, which is the whole reason zaps is a member."
    )


def test_the_payments_engine_crate_is_optional_at_the_nest():
    """An `optional = true` dep is the only kind cargo can leave out; a plain dep
    line under a gated `mod` still compiles the crate into the binary."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(_NEST))
        if re.search(r"^\s*fauna-payments\s*=", line) and "optional = true" not in line
    ]
    assert not offenders, (
        f"{_NEST}/Cargo.toml takes fauna-payments as a NON-optional dep, so the "
        f"entitlement engine ships in every flavor. Offending line(s): {offenders!r}"
    )


def test_the_windows_nest_service_still_asks_for_the_payments_plane():
    """The shipped-artifact trap, pinned so it cannot be re-sprung silently.

    `apps/fauna-windows/fauna-nest-service` takes `fauna-nest` with
    `default-features = false`. While the nest's own `default` was `[]` that was a
    no-op — and the manifest said so. Making the registry members default-ON
    turned the same line into a silent excision of the payments plane AND the
    `subscriptions` capability token from the shipped `FaunaNest` service exe,
    with both flavors still building green and no test naming it.

    Whether that exe *should* ship payments is a product question; this pin only
    ensures the answer changes deliberately rather than as a side effect of
    someone editing the nest's `default`.
    """
    lines = [
        line
        for line in _shipping_dep_lines(_manifest("apps/fauna-windows/fauna-nest-service"))
        if re.search(r"^\s*fauna-nest\s*=", line)
    ]
    assert len(lines) == 1, f"expected exactly one fauna-nest dep line, got {lines!r}"
    line = lines[0]
    if "default-features = false" in line:
        assert '"payments"' in line, (
            "fauna-nest-service takes fauna-nest with `default-features = false` and no "
            "explicit `payments` feature, so the shipped Windows FaunaNest service exe "
            "silently loses the payments plane and the `subscriptions` capability token "
            'it ships today. Add `features = ["store-safe", "payments"]` — or, if '
            "dropping payments there is intended, make that an explicit product "
            "decision and update this pin."
        )


def test_the_nest_has_its_own_two_column_artifact_witness():
    """The server-side column of the artifact witness.

    Not redundant with the ffi one, and load-bearing for a reason the client side
    does not share: the registry members are default-ON at the nest, so NEITHER
    arm of `nest-lib-test-check` (bare-default, then the all-features union) ever
    compiles the excised nest. This recipe is the only thing that builds it, so
    without it the nest-side escape hatch is executed by no gate at all — the
    compile-gated-but-never-test-gated class.
    """
    body = _recipe_body("nest-store-safe-check")
    assert "--no-default-features --features store-safe" in body, (
        "nest-store-safe-check no longer builds the store-safe nest flavor"
    )
    assert "fauna\\.payments\\." in body and "fauna\\.tips\\." in body, (
        "nest-store-safe-check lost one of its witness patterns"
    )
    # `--locked` required as of 2026-08-10 — see the ffi twin's comment.
    assert re.search(
        r"cargo build --locked -p fauna-nest --bin fauna-nest\s*$", body, re.MULTILINE
    ), (
        "nest-store-safe-check no longer builds the DEFAULT nest flavor, so its "
        "store-safe assertion is vacuous — a grep matching nothing would pass"
    )
    assert "vacuous" in body, (
        "nest-store-safe-check lost the comment explaining why the second column exists"
    )


# ── the APP-SHELL flavor roots (2026-08-14, the tui + linux excision legs) ────
#
# A third section for the same reason the nest got the second: the shape differs.
# An app shell forwards the RPC face like a client root does, but it also
# forwards `fauna-feed/payments` (the tip resolver) and declares only the
# registry members it actually renders — neither shell has a `zaps` surface, so
# the library roots' exact `["store-safe"] + _REGISTRY_FEATURES` equality is the
# wrong assertion here. The complement is checked against what each root
# DECLARES instead.
#
# ⚠ Registering the shells here was itself a gap, found 2026-08-14 while adding
# the linux leg: the tui leg (2026-08-13) shipped `tui-store-safe-check` without
# adding either root to this module, so BOTH shells' manifests and both recipes'
# `--locked` flags were unasserted. Add a shell here in the same commit that
# gives it a store-safe recipe.

_APP_FLAVOR_ROOTS = ["apps/fauna-tui", "apps/fauna-linux"]

# ⚠ Parametrization IDs here are UNDERSCORE-joined on purpose, and must stay
# that way. This predates `conftest._parametrized_clients`'s fix to key on the real fixture behind each `callspec` entry rather than
# the param id string — a direct `@pytest.mark.parametrize` like this one is
# excluded regardless of its ids now, but the underscore convention is kept
# rather than reintroducing hyphenated ids purely to prove the point: the
# natural ids (`[apps/fauna-linux]`, `[linux-fauna-desktop]`) once made these
# tests LOOK like app-driver tests under the OLD id-string matcher — under the
# default `--app tui` set the linux half was silently DESELECTED, and a
# deselected pin is indistinguishable from one that never existed (convention
# 7's silent-no-coverage class — measured here 2026-08-14, where exactly half
# of this section vanished from the default run). These are tier_1 pure-text
# manifest reads; no driver, no app. Keep the ids app-free regardless.
_APP_ROOT_IDS = ["tui_shell", "linux_shell"]


def _declared_registry_members(root: str) -> list[str]:
    """The registry members this root declares a feature for.

    An app shell renders only some of the planes, so its `default` carries only
    the members it actually has a surface for — unlike the library roots, which
    carry every member because they export every face.
    """
    manifest = _manifest(root)
    return [m for m in _REGISTRY_FEATURES if _feature_list(manifest, m) is not None]


@pytest.mark.parametrize("root", _APP_FLAVOR_ROOTS, ids=_APP_ROOT_IDS)
def test_every_app_shell_is_a_flavor_root_with_its_members_default_on(root):
    """An app binary IS a flavor root (`dynamic-features.md` § Platform-family
    surface excision), so the switch belongs on the app crate and a plain build
    still ships the plane — excision is what a FLAVOR asks for."""
    members = _declared_registry_members(root)
    assert members, (
        f"{root} declares no gated-feature-registry member at all. Either it has no "
        "payments surface (in which case it does not belong in _APP_FLAVOR_ROOTS) or "
        "its feature was dropped and the shell now ships the plane unconditionally."
    )
    default = _feature_list(_manifest(root), "default")
    assert default is not None, f"{root} lost its `default` feature list"
    entries = [e.strip().strip('"') for e in default.split(",") if e.strip()]
    assert sorted(entries) == sorted(["store-safe"] + members), (
        f'{root}\'s `default` must be exactly ["store-safe"] plus the registry members '
        f"it declares ({members}) — the split is what makes the excised flavor "
        "`--no-default-features --features store-safe` with no second copy of the "
        "feature list to keep in sync. A new entry that is NOT a registry member "
        f"belongs in `store-safe` instead. Got: {entries}"
    )


@pytest.mark.parametrize("root", _APP_FLAVOR_ROOTS, ids=_APP_ROOT_IDS)
def test_every_app_shell_keeps_a_store_safe_complement(root):
    """The complement may be non-empty: a surface behind a feature that is not a
    registry member belongs in it, or the bare `--no-default-features` spelling
    would silently drop that surface from every store-safe build, a regression
    that fails no build because both flavors still compile. Today it is empty at
    every root."""
    store_safe = _feature_list(_manifest(root), "store-safe")
    assert store_safe is not None, (
        f"{root} lost its `store-safe` feature. It may be EMPTY (fauna-tui's is), but "
        "it must exist: it is what makes `--no-default-features --features store-safe` "
        "the one spelling of the excised flavor at every root."
    )
    for member in _declared_registry_members(root):
        assert member not in store_safe, (
            f"{root}'s `store-safe` feature forwards `{member}` — the store-safe shell "
            "would then still render a controversial-class plane, defeating the hatch"
        )


@pytest.mark.parametrize("root", _APP_FLAVOR_ROOTS, ids=_APP_ROOT_IDS)
def test_no_app_shell_names_a_registry_feature_on_a_dependency_line(root):
    """Rule (b) at the app shells, and this one has already been violated: before
    2026-08-14 `fauna-linux` carried `fauna-feed = { features = ["payments"] }`,
    which turns the tip resolver on unconditionally and survives
    `--no-default-features`. The shell must forward it from its own `payments`
    feature instead."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(root))
        if any(f'"{m}"' in line or f"'{m}'" in line for m in _REGISTRY_FEATURES)
    ]
    assert not offenders, (
        f"{root}/Cargo.toml names a registry feature on a dependency line, which turns "
        "that plane on unconditionally regardless of the flavor (dynamic-features.md "
        f"§ The cargo feature spine, rule (b)). Offending line(s): {offenders!r}"
    )


@pytest.mark.parametrize("root", _APP_FLAVOR_ROOTS, ids=_APP_ROOT_IDS)
def test_every_app_shell_forwards_both_the_rpc_face_and_the_tip_resolver(root):
    """Both halves ride ONE switch. Forwarding only the client crate leaves
    `fauna-feed`'s `resolve_post_tips` compiled in, and the tip render then
    compiles fine against a `PostSummary.tips` that is `None` forever — the
    deliberately ungated inert record that makes a store-safe shell still ship
    every `post-tip-*` id it would have painted. Dead is not absent."""
    payments = _feature_list(_manifest(root), "payments")
    assert payments is not None, f"{root} has no `payments` feature"
    assert "dep:fauna-client-payments" in payments, (
        f"{root}'s `payments` feature no longer pulls fauna-client-payments as an "
        "optional dep — the crate would be unconditional and its RPC face would ship "
        "in the store-safe flavor"
    )
    assert "fauna-feed/payments" in payments, (
        f"{root}'s `payments` feature no longer forwards `fauna-feed/payments` — the "
        "tip resolver would stay compiled in while the shell's own surface excised"
    )


@pytest.mark.parametrize("root", _APP_FLAVOR_ROOTS, ids=_APP_ROOT_IDS)
def test_the_payments_crate_is_optional_at_every_app_shell(root):
    """An `optional = true` dep is the only kind cargo can actually leave out; a
    plain dep line under a gated `mod` still compiles the crate into the
    artifact."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(root))
        if re.search(r"^\s*fauna-client-payments\s*=", line) and "optional = true" not in line
    ]
    assert not offenders, (
        f"{root}/Cargo.toml takes fauna-client-payments as a NON-optional dep, so the "
        "crate is compiled into every flavor no matter what the module gates say. "
        f"Offending line(s): {offenders!r}"
    )


@pytest.mark.parametrize(
    "app,binary",
    [("tui", "fauna-tui"), ("linux", "fauna-desktop")],
    ids=_APP_ROOT_IDS,
)
def test_every_app_shell_has_its_own_two_column_artifact_witness(app, binary):
    """The app-shell column of the witness family, and the ONLY place criterion 1
    ("no UI surface — the feature's element IDs absent") is asserted: the ffi,
    wasm and nest columns all witness a library or server root, where no element
    id lives.

    ⚠ The binary name is checked explicitly because it is the trap this pair of
    recipes can fall into silently: `fauna-linux`'s binary is `fauna-desktop`, so
    a verbatim copy of the tui recipe greps a path that never exists and passes
    vacuously in BOTH columns.
    """
    body = _recipe_body(f"{app}-store-safe-check")
    assert f'BIN="$TARGET_DIR/debug/{binary}"' in body, (
        f"{app}-store-safe-check no longer greps `{binary}`. If the path is wrong the "
        "`strings` calls read an absent file and both columns pass vacuously."
    )
    assert "--no-default-features --features store-safe" in body, (
        f"{app}-store-safe-check no longer builds the store-safe flavor"
    )
    # Criterion 1 (element ids) AND criterion 2 (kind strings), as PREFIXES so a
    # new §4/§5 or tip element is covered the day it is added.
    for pat in ("subscription-provider-", "subscription-claim-", "post-tip-"):
        assert pat in body, f"{app}-store-safe-check lost its '{pat}' element-id pattern"
    assert "fauna\\.payments\\." in body, (
        f"{app}-store-safe-check lost its kind-string pattern"
    )
    # `--locked`: a store-safe build that silently re-resolved the lockfile is not
    # the artifact a store submission would carry.
    assert re.search(
        rf"cargo build --locked -p fauna-{app} --bin {binary}\s*$", body, re.MULTILINE
    ), (
        f"{app}-store-safe-check no longer builds the DEFAULT flavor with --locked, so "
        "its store-safe assertion is vacuous — a grep matching nothing would pass"
    )
    assert "vacuous" in body, (
        f"{app}-store-safe-check lost the comment explaining why the second column exists"
    )


# ── the APPLE app-shell leg (2026-08-15) ─────────────────────────────────────
#
# A fourth section because apple's shape is unlike all three above: the family
# has no cargo app crate at all. Its flavor root is `fauna-ffi` (pinned in the
# library-root section) and its shell half is a **Swift compilation condition**,
# so these pins are recipe- and source-shaped rather than manifest-shaped.
#
# ⚠ THE CONDITION IS NEGATIVE, and deliberately: `swift build` can only ADD a
# condition (`-Xswiftc -D`), never remove one, so a positive `FAUNA_PAYMENTS`
# would have to be declared in Package.swift and could not then be switched off
# from a recipe. The invariant survives inverted and is in fact stronger — a
# plain build ships the plane with no configuration at all, so a new target or a
# stray `swift build` cannot silently excise it. `dynamic-features.md`
# § Platform-family surface excision records the decision.
#
# These are pure text reads, so they run on every machine — which matters here
# more than anywhere else in this module: `apple-store-safe-check` is the only
# apple witness and it needs an Apple toolchain, so without the source pin below
# a macOS-less fleet could add an ungated payments view and see nothing red.

_APPLE_CONDITION = "FAUNA_EXCISE_PAYMENTS"
_FAUNAKIT = _REPO / "apps/fauna-apple/FaunaKit/Sources/FaunaKit"
#: Swift sources both apple targets compile. `Fauna-iOS/` is in scope for the
#: host-visibility pin below because it is exactly as invisible to the macOS
#: artifact witness as an `#if os(iOS)` block inside FaunaKit is.
_APPLE_SWIFT_ROOTS = (_FAUNAKIT, _REPO / "apps/fauna-apple/Fauna-iOS")
#: The rendered payments surface, as element-id PREFIXES (never whole ids, so a
#: new §4/§5 element is covered on the day it is added). Shared by the two
#: source pins below so they cannot drift apart.
#:
#: ⚠ `nostr-zap-signer-` is here because on apple the SUBSET MEMBER rides the same
#: condition as its parent (dynamic-features.md § Which element IDs belong to a
#: gated feature, the Swift row): the store-safe FFI drops the `zaps` cargo feature
#: with `payments`, so a store-safe `FaunaFFI.xcframework` exports no
#: `FfiZapSignerEntry` and every zap-signer surface must carry `FAUNA_EXCISE_PAYMENTS`
#: exactly as a §4/§5 one does. It was NOT here when the designation control's apple
#: leg landed (2026-08-22), and the gap cost a red merge gate six days later: the
#: glue half was caught only by `apple-store-safe-check`, which needs an Apple
#: toolchain — precisely the mac-less-fleet blindness this pin exists to remove.
_APPLE_ID_PREFIXES = ("subscription-provider-", "subscription-claim-", "nostr-zap-signer-")

#: The `p2p-share` member's apple condition, and what it covers. It is a registry
#: member of its own — NOT a subset of `payments` the way `zaps` is — so it takes a
#: condition of its own (dynamic-features.md § Platform-family surface excision: one
#: condition per registry member). Its pins are in the section after the payments
#: ones, and they are LINE-level, for the reason given there.
_APPLE_P2P_CONDITION = "FAUNA_EXCISE_P2P_SHARE"
_APPLE_P2P_ID_PREFIXES = ("offline-share-", "offline-receive-", "share-transfer-", "share-serve-")


def _apple_paint_tokens() -> tuple[str, ...]:
    """Every spelling by which an apple source PAINTS a payments element id.

    ⚠ There are TWO since apple adopted the generated element-id constants
    (2026-08-17, ~2428 sites): the string literal, and the constant reference
    `Ids.subscriptionProviderRow`. A pin watching only the literal silently stopped
    covering the adopted views the moment adoption landed — measured the day this
    was written, `Views/SubscriptionSettingsView.swift` paints payments ids and
    contains **not one literal**, so both apple source pins were reading it as
    carrying nothing. It happens to hold the condition, so nothing leaked; that is
    luck, not coverage, and luck is what a pin exists to replace.

    Stays PREFIX-shaped in both spellings — `Ids.subscriptionProvider` matches every
    member derived from a `subscription-provider-*` id — so a new §4/§5 element is
    covered on the day it is added, whichever way the app spells it. The
    id→member-name rule is imported from `scripts/ui_id_names.py`, the same module
    the generator and the two ui lints use for exactly this "recognise a constant
    reference as an implementation" reason, so it cannot drift from what is emitted.
    """
    return _paint_tokens(_APPLE_ID_PREFIXES, "camel")

#: How a macOS-host `swift build` resolves the platform predicates. Anything not
#: listed (`DEBUG`, `FAUNA_EXCISE_PAYMENTS`, a bare custom flag) is treated as
#: TRUE — see `_compiled_for_host`.
_HOST_TRUTH = {
    "os(macOS)": True,
    "os(iOS)": False,
    "os(watchOS)": False,
    "os(tvOS)": False,
    "os(visionOS)": False,
    "canImport(AppKit)": True,
    "canImport(UIKit)": False,
}


def _strip_outer_parens(tok: str) -> str:
    """`(os(iOS))` → `os(iOS)`, leaving `os(iOS)` alone (a naive `.strip("()")`
    would eat the predicate's own closing paren)."""
    tok = tok.strip()
    while tok.startswith("(") and tok.endswith(")"):
        depth = 0
        for i, ch in enumerate(tok):
            depth += (ch == "(") - (ch == ")")
            if depth == 0 and i < len(tok) - 1:
                return tok  # the leading "(" closed early — not a wrapper
        tok = tok[1:-1].strip()
    return tok


def _host_truth(cond: str) -> bool | None:
    """Three-valued evaluation of a Swift `#if` condition for a macOS-host build:
    `True`/`False` when the PLATFORM predicates settle it, `None` when a token
    this pin does not model (`DEBUG`, `FAUNA_EXCISE_PAYMENTS`, a custom flag)
    leaves it open.

    ⚠ The third value is the whole point, and it is not decoration: collapsing
    unknown to `True` early looks conservative and is not, because `!unknown`
    then reads as a definite `False`. `#if !FAUNA_EXCISE_PAYMENTS` — the exact
    spelling every payments surface in FaunaKit uses — would be classified
    host-INVISIBLE, and the pin below would report every real payments render as
    an offender. Unknown must stay unknown through the negation.
    """
    cond = _strip_outer_parens(cond)
    if "||" in cond:
        parts = [_host_truth(p) for p in cond.split("||")]
        if any(p is True for p in parts):
            return True
        return None if any(p is None for p in parts) else False
    if "&&" in cond:
        parts = [_host_truth(p) for p in cond.split("&&")]
        if any(p is False for p in parts):
            return False
        return None if any(p is None for p in parts) else True
    negated = False
    while cond.startswith("!"):
        negated = not negated
        cond = _strip_outer_parens(cond[1:])
    value = _HOST_TRUTH.get(cond)
    if value is None:
        return None
    return not value if negated else value


def _compiled_for_host(cond: str) -> bool:
    """Would a macOS-host `swift build` compile the branch guarded by `cond`?

    Deliberately conservative: only a condition the platform predicates settle as
    definitely false counts as host-invisible, so this can never invent a finding
    — at worst it misses an exotic spelling, which is the right way for a pin to
    fail.
    """
    return _host_truth(cond) is not False


def _host_invisible_lines(text: str) -> set[int]:
    """1-based line numbers a macOS-host `swift build` never compiles.

    Swift's `#if`/`#elseif`/`#else` chain compiles exactly one branch, so each
    frame carries both "does this branch compile for the host" and "has some
    earlier branch already matched".
    """
    invisible: set[int] = set()
    stack: list[list[bool]] = []  # [branch_compiles_for_host, matched_so_far]
    for number, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("#if "):
            compiles = _compiled_for_host(stripped[4:])
            stack.append([compiles, compiles])
        elif stripped.startswith(("#elseif ", "#elif ")) and stack:
            compiles = (
                False
                if stack[-1][1]
                else _compiled_for_host(stripped.split(" ", 1)[1])
            )
            stack[-1][0] = compiles
            stack[-1][1] = stack[-1][1] or compiles
        elif stripped == "#else" and stack:
            stack[-1][0] = not stack[-1][1]
            stack[-1][1] = True
        elif stripped == "#endif":
            if stack:
                stack.pop()
        elif not all(frame[0] for frame in stack):
            invisible.add(number)
    return invisible


def test_the_apple_xcframework_has_a_store_safe_flavor():
    """`just apple-ffi-store-safe` is the escape hatch's apple artifact recipe —
    the row `dynamic-features.md` § The App-Store escape hatch's table has been
    carrying as **Open** since 2026-08-10."""
    assert '_apple-ffi-flavor "store-safe"' in _recipe_body("apple-ffi-store-safe"), (
        "apple-ffi-store-safe must delegate to the shared impl with the store-safe "
        "flavor; a second copy of the 5-slice body would drift from the production one"
    )
    assert '_apple-ffi-host-flavor "{{config}}" "store-safe"' in _recipe_body(
        "apple-ffi-host-store-safe"
    ), (
        "apple-ffi-host-store-safe must delegate to the shared host impl — it is the "
        "1-slice twin the macOS app and the witness link"
    )
    for impl in ("_apple-ffi-flavor", "_apple-ffi-host-flavor"):
        body = _recipe_body(impl)
        assert '--no-default-features --features "store-safe,file-provider-host"' in body, (
            f"{impl} no longer spells the store-safe flavor as the COMPLEMENT feature. "
            "A hand-listed excision set rots the first time a surface is added to one "
            "list and not the other (dynamic-features.md § The cargo feature spine)."
        )


def test_the_apple_store_safe_app_recipe_excises_both_halves():
    """An excised flavor is the FFI built without the feature **plus** the shell
    built with the matching condition off. Half of it is a build that compiles
    perfectly and ships the plane."""
    body = _recipe_body("mac-store-safe")
    assert "apple-ffi-host-store-safe" in _recipe_header("mac-store-safe"), (
        "mac-store-safe no longer takes the store-safe FFI, so its app would link a "
        "payments-carrying xcframework and carry every kind string with it"
    )
    assert f"-Xswiftc -D{_APPLE_CONDITION}" in body, (
        "mac-store-safe no longer passes the apple family's compile condition, so the "
        "shell renders (and ships the element ids of) a plane the FFI cannot reach"
    )


def test_the_apple_app_shell_has_its_own_two_column_artifact_witness():
    """Criterion 1 ("no UI surface — the feature's element IDs absent") lives in a
    shell, and apple's shells are Swift: no ffi/wasm/nest column can see them.

    ⚠ `post-tip-` is deliberately NOT asserted here, unlike tui and linux: apple
    has no tip surface yet, and a pattern that matches nothing in BOTH columns is
    the vacuity the second column exists to prevent. It joins on the day a tip
    render lands.
    """
    body = _recipe_body("apple-store-safe-check")
    assert "apple-ffi-host-store-safe" in body, (
        "apple-store-safe-check's first column no longer links the store-safe FFI — "
        "the kind-string assertion would then be about the linker's dead-stripping "
        "luck rather than about an artifact that structurally cannot reach the plane"
    )
    assert f"-Xswiftc -D{_APPLE_CONDITION}" in body, (
        "apple-store-safe-check's first column no longer builds the excised shell"
    )
    assert "--target FaunaiOS" in body, (
        "apple-store-safe-check lost its iOS arm. There is no iOS archive recipe yet, "
        "so this typecheck is the only thing keeping the shared shell excisable for "
        "the iOS target between now and the one that lands it. ⚠ Note what it does "
        "NOT do (measured 2026-08-17): SwiftPM builds `--target FaunaiOS` for the "
        "macOS HOST through the CrossPlatformUI shims, so it never compiles an "
        "`#if os(iOS)` block — that blind spot is pinned separately by "
        "test_no_apple_payments_surface_hides_behind_an_ios_only_conditional."
    )
    for pat in ("subscription-provider-", "subscription-claim-"):
        assert pat in body, f"apple-store-safe-check lost its '{pat}' element-id pattern"
    assert "fauna\\.payments\\." in body, "apple-store-safe-check lost its kind-string pattern"
    assert "just apple-ffi-host release" in body, (
        "apple-store-safe-check's second column no longer restores the DEFAULT FFI, so "
        "its store-safe assertion is vacuous — a grep matching nothing would pass"
    )
    # ⚠ RELEASE is load-bearing here, not a preference: a debug Swift build
    # compiles the `#if DEBUG` automation surface, whose `*ForTest` calls only
    # resolve against the test-helpers FFI — which a shipping store-safe
    # xcframework must never carry. Debug + store-safe cannot build at all.
    assert body.count("-c release") >= 3, (
        "apple-store-safe-check no longer builds RELEASE. Debug + store-safe is an "
        "un-buildable pairing (the `#if DEBUG` FaunaKit automation surface needs the "
        "test-helpers FFI flavor), and release is also the artifact a submission carries"
    )
    assert "vacuous" in body, (
        "apple-store-safe-check lost the comment explaining why the second column exists"
    )


def test_every_apple_payments_surface_carries_the_compile_condition():
    """The render trap, caught without an Apple toolchain.

    Gating the API call is the half the compiler does for you — a store-safe
    xcframework exports no `FfiPaymentsClient`, so every call site is a build
    error. The half it cannot see is a SwiftUI view that compiles fine, paints
    nothing, and still ships every `accessibilityIdentifier` it would have
    painted. So: any FaunaKit source carrying a payments element id must also
    carry the condition that removes it.
    """
    tokens = _apple_paint_tokens()
    offenders = []
    carriers = []
    for path in sorted(_FAUNAKIT.rglob("*.swift")):
        text = path.read_text(encoding="utf-8")
        if not any(token in text for token in tokens):
            continue
        carriers.append(path.relative_to(_REPO).as_posix())
        if _APPLE_CONDITION not in text:
            offenders.append(path.relative_to(_REPO).as_posix())
    # Vacuity guard — the failure mode of a grep-shaped pin is matching nothing.
    # If FaunaKit moves, or the ids are renamed, this test would otherwise go
    # permanently green while covering zero sources.
    assert carriers, (
        f"no source under {_FAUNAKIT} carries a payments element id at all, so this "
        "pin is asserting nothing. Either the tree moved or the ids were renamed — "
        "in both cases fix the pin, do not delete it."
    )
    assert not offenders, (
        "these FaunaKit sources render payments element ids with no "
        f"`#if !{_APPLE_CONDITION}` anywhere in the file, so a store-safe apple build "
        "ships every id they paint (dynamic-features.md § What \"completely compiled "
        f"away\" means, criterion 1): {offenders!r}"
    )


def test_the_generated_apple_id_table_puts_every_payments_id_behind_the_condition():
    """The pin above is a whole-FILE check, which is too coarse for a GENERATED table.

    `UiIds.swift` is emitted by `scripts/ui-ids-generate.py`, and the emitter also
    writes an explanatory comment naming `FAUNA_EXCISE_PAYMENTS`. So an emitter
    regression that put the payments constants back in the ordinary `enum Ids`
    body would leave the file still *containing* the condition string, and the
    file-level pin would stay green while the table shipped all 30 ids again —
    the exact escape this row was opened to close.

    This asserts the stronger property the generator actually promises: every
    payments id in that file sits inside a `#if !FAUNA_EXCISE_PAYMENTS` region.

    ⚠ "payments" here means the parent AND its subset member `zaps`, because on
    apple both ride the one condition (dynamic-features.md § Which element IDs
    belong to a gated feature, the Swift row) — the emitter writes them as two
    separate `#if !FAUNA_EXCISE_PAYMENTS extension Ids` blocks, which the depth
    counter below handles as flat siblings. Reading only the parent is how the six
    `nostr-zap-signer-*` ids sat in the ordinary table for six days while this pin
    stayed green.
    """
    text = _UI_IDS_SWIFT.read_text(encoding="utf-8")

    # Region map: True on the lines a store-safe build compiles away. The emitted
    # shape is one flat `#if !COND … #endif` block, so a depth counter is enough —
    # and a *nested* conditional would make this over-report rather than
    # under-report, which is the safe direction for a pin.
    excised: list[bool] = []
    depth = 0
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(f"#if !{_APPLE_CONDITION}"):
            depth += 1
            excised.append(True)
            continue
        if stripped == "#endif" and depth:
            depth -= 1
            excised.append(True)
            continue
        excised.append(depth > 0)

    gated = sorted(_gated_ids("payments") | _gated_ids("zaps"))
    assert gated, (
        "ui.yaml's `gated_features.payments`/`.zaps` resolved to no ids, so this pin "
        "is asserting nothing — fix the catalog, do not delete the pin."
    )

    leaked: list[str] = []
    seen: set[str] = set()
    for line, is_excised in zip(text.splitlines(), excised):
        for element_id in gated:
            if f'"{element_id}"' in line:
                seen.add(element_id)
                if not is_excised:
                    leaked.append(element_id)
    missing = [i for i in gated if i not in seen]
    assert not missing, (
        "these payments/zaps ids are declared in ui.yaml's `gated_features:` block "
        f"but appear nowhere in {_UI_IDS_SWIFT.name}, so the table and the catalog "
        f"have drifted — regenerate with `just ui-ids-generate`: {missing!r}"
    )
    assert not leaked, (
        f"these payments element ids sit OUTSIDE the `#if !{_APPLE_CONDITION}` region "
        f"of {_UI_IDS_SWIFT.name}, so a store-safe apple build carries them "
        "(dynamic-features.md § Which element IDs belong to a gated feature): "
        f"{sorted(set(leaked))!r}"
    )


def test_the_gated_id_catalog_covers_every_prefix_the_witnesses_hand_list():
    """One catalog, or the copies drift — which is how this row started.

    Before 2026-08-17 the payments element-id prefixes were hand-listed in this
    module and again in each `*-store-safe-check` recipe, with nothing tying them
    together; the generator then became a sixth consumer with no list at all and
    shipped every id ungated. `ui.yaml`'s `gated_features:` block is now the one
    source of truth, and this is what stops a seventh copy from being born: a
    prefix a witness watches must be one the catalog declares.

    The converse is deliberately NOT asserted per-witness. `apple-store-safe-check`
    omits `post-tip-` on purpose (apple has no tip surface, and a pattern matching
    nothing in both columns is the vacuity the second column exists to prevent),
    so a witness watching a SUBSET is correct. What must never happen is a witness
    watching something the catalog has never heard of.
    """
    catalog = {p for spec in _gated_catalog().values() for p in spec["id_prefixes"]}
    assert catalog, "ui.yaml declares no `gated_features:` prefixes at all"

    watched = (
        set(_APPLE_ID_PREFIXES)
        | set(_APPLE_P2P_ID_PREFIXES)
        | set(_WEB_ELEMENT_ID_PREFIXES)
        | _witness_id_prefixes()
    )

    orphans = sorted(p for p in watched if p not in catalog)
    assert not orphans, (
        "these element-id prefixes are watched by a store-safe witness but are not "
        "declared in ui.yaml's `gated_features:` block, so the generator emits their "
        "ids ungated while the witness expects them absent: "
        f"{orphans!r}"
    )
    unwatched = sorted(p for p in catalog if p not in watched)
    assert not unwatched, (
        "these `gated_features:` prefixes are watched by NO store-safe witness, so "
        "nothing measures whether an artifact actually drops them — add them to the "
        f"owning family's recipe or drop them from the catalog: {unwatched!r}"
    )


def test_the_host_visibility_scanner_actually_sees_an_ios_only_block():
    """Self-test for the pin below, on a literal snippet rather than the tree.

    The pin's failure mode is a scanner that quietly reports nothing — it would
    then stay green forever while covering zero lines, which is the same vacuity
    the two-column witnesses exist to prevent. This fixes the scanner's semantics
    against a known answer, so a regression in `_host_invisible_lines` fails HERE
    with an obvious cause instead of silently disarming the invariant.
    """
    snippet = (
        "import SwiftUI\n"                       # 1  host-visible
        "#if os(iOS)\n"                          # 2  directive
        "let mobileOnly = 1\n"                   # 3  INVISIBLE to a host build
        "#else\n"                                # 4  directive
        "let desktopOnly = 2\n"                  # 5  host-visible
        "#endif\n"                               # 6  directive
        "#if os(macOS)\n"                        # 7  directive
        "let alsoDesktop = 3\n"                  # 8  host-visible
        "#else\n"                                # 9  directive
        "let notDesktop = 4\n"                   # 10 INVISIBLE (the else of a macOS #if)
        "#endif\n"                               # 11 directive
        "#if DEBUG\n"                            # 12 directive
        "let unknownFlagIsAssumedCompiled = 5\n"  # 13 host-visible (conservative)
        "#endif\n"                               # 14 directive
        "#if !os(macOS)\n"                       # 15 directive
        "let negatedDesktop = 6\n"               # 16 INVISIBLE
        "#endif\n"                               # 17 directive
        "#if os(iOS) || os(watchOS)\n"           # 18 directive
        "let anyMobile = 7\n"                    # 19 INVISIBLE
        "#endif\n"                               # 20 directive
        # ⚠ The regression that made this self-test necessary: an unknown flag
        # NEGATED must stay host-visible. Collapsing unknown to True before the
        # `!` classified `#if !FAUNA_EXCISE_PAYMENTS` — the spelling every apple
        # payments surface uses — as iOS-only, so the pin below reported two real
        # renders as offenders. Measured while writing it, 2026-08-17.
        "#if !FAUNA_EXCISE_PAYMENTS\n"           # 21 directive
        "let paymentsRender = 8\n"               # 22 host-visible
        "#endif\n"                               # 23 directive
    )
    assert _host_invisible_lines(snippet) == {3, 10, 16, 19}, (
        "`_host_invisible_lines` no longer resolves Swift's #if/#else chain the way "
        "a macOS-host `swift build` does, so the iOS-only pin below is not asserting "
        "what it claims. Fix the scanner, never the expectation."
    )


def test_no_apple_payments_surface_hides_behind_an_ios_only_conditional():
    """The blind spot BOTH apple columns share, closed without an Apple toolchain.

    `just apple-store-safe-check` has two arms, and neither can see iOS-only code:
    its artifact arm greps a release **FaunaMacOS** binary, and its iOS arm is
    `swift build --target FaunaiOS`, which SwiftPM builds for the **macOS host**
    through the `CrossPlatformUI.swift` shims (`Package.swift`'s `FaunaiOSLib`
    comment says so outright — a real iOS-triple compile only happens under
    `xcodebuild`). So a payments element id placed inside an `#if os(iOS)` block
    would be invisible to the artifact grep, never compiled by the typecheck, and
    still shipped by an iOS build: the excision would rot with every gate green.

    Measured 2026-08-17 when this pin was written: **zero** offenders, and none of
    the payments-carrying files held an `os(iOS)` conditional at all — so the gap
    was latent, not live, and this keeps it that way. Retire this pin only when
    the iOS artifact witness exists to replace it (`dynamic-features.md` § The
    App-Store escape hatch, the `.xcarchive` row).

    The `p2p-share` member's ids are scanned too (2026-09-26): the blind spot is
    the macOS arm's, not the member's.
    """
    tokens = _apple_paint_tokens() + _paint_tokens(_APPLE_P2P_ID_PREFIXES, "camel")
    offenders: list[str] = []
    carriers: list[str] = []
    for root in _APPLE_SWIFT_ROOTS:
        for path in sorted(root.rglob("*.swift")):
            text = path.read_text(encoding="utf-8")
            if not any(token in text for token in tokens):
                continue
            carriers.append(path.relative_to(_REPO).as_posix())
            invisible = _host_invisible_lines(text)
            if not invisible:
                continue
            for number, line in enumerate(text.splitlines(), start=1):
                if number in invisible and any(token in line for token in tokens):
                    offenders.append(f"{path.relative_to(_REPO).as_posix()}:{number}")
    # Same vacuity guard as the sibling pin: a grep-shaped assertion whose corpus
    # went empty is a test that passes by covering nothing.
    assert carriers, (
        "no apple Swift source carries a payments element id at all, so this pin is "
        f"asserting nothing (looked under {[r.name for r in _APPLE_SWIFT_ROOTS]!r}). "
        "Either the tree moved or the ids were renamed — fix the pin, do not delete it."
    )
    assert not offenders, (
        "these lines paint a payments element id inside a block a macOS-host build "
        "never compiles, so BOTH arms of `just apple-store-safe-check` are blind to "
        "them and an iOS artifact would ship the id regardless of the flavor "
        f"(dynamic-features.md § The feature-matrix test story, item (3)): {offenders!r}. "
        "Either move the render out of the platform conditional, or land the iOS "
        "artifact witness that can see it."
    )


# ── the apple leg of `p2p-share` (2026-09-26) ────────────────────────────────
#
# The second apple condition, `FAUNA_EXCISE_P2P_SHARE`. Every store-safe Swift
# invocation passes it beside `FAUNA_EXCISE_PAYMENTS`: the family has one store-safe
# artifact, but one condition per registry member.
#
# ⚠ WHY THESE PINS ARE LINE-LEVEL, unlike the payments pins' whole-file check. The
# member has two halves, and the compiler guards only one. The share PLANE's FFI face
# (`FfiSharePlaneView`, `FfiSharePlaneListener`, `startSharePlane`) is absent from the
# store-safe xcframework, so an ungated reference to it fails to compile in the mac
# merge-gate check's gate 2. The co-present CEREMONY's face (`offline_share_*`) still
# sits in `fauna-ffi`'s `store-safe` list, so an ungated
# ceremony reference compiles and ships its ids. Neither case needs an Apple
# toolchain to catch here. A file-level check would also be satisfied by the one
# guarded block most of these files already have, which is how an ungated call
# placed beside it would slip through.

#: Tokens naming the member's FFI face in Swift: the plane's types and door, then
#: the ceremony's. `offlineShare` is the camelCase prefix of every generated
#: `offline_share_*` free function, and of the view model's own ceremony state.
_APPLE_P2P_FFI_TOKENS = (
    "FfiSharePlane",
    "startSharePlane",
    "FfiCeremonySeat",
    "OfflineShareView",
    "OfflineSharePanel",
    "CeremonyStatus",
    "PeerCodeParsed",
    "FfiGroupShareViews",
    "offlineShare",
)
#: Every hand-written Swift root a shipped apple app compiles: shared FaunaKit, the
#: iOS shell, and the macOS shell (which holds the plane's start call).
_APPLE_SHELL_ROOTS = _APPLE_SWIFT_ROOTS + (_REPO / "apps/fauna-apple/Fauna-macOS",)


def _swift_code_only(line: str) -> str:
    """`line` minus a trailing `//` comment. A `//` inside a string literal stays."""
    in_string = False
    i = 0
    while i < len(line):
        ch = line[i]
        if in_string and ch == "\\":
            i += 2
            continue
        if ch == '"':
            in_string = not in_string
        elif not in_string and line.startswith("//", i):
            return line[:i]
        i += 1
    return line


def _excised_lines(text: str, condition: str) -> set[int]:
    """1-based line numbers a build defining `condition` compiles away.

    A branch is excised when its guard requires the condition to be ABSENT: an
    `#if`/`#elseif` whose `&&`-conjunction has a `!condition` term
    (`#if DEBUG && !FAUNA_EXCISE_P2P_SHARE` included), or the `#else` of a bare
    `#if condition`. A line is excised when any enclosing branch is, so nesting
    works. A guard with `||` in it is never read as excising, since it could hold
    either way. That keeps this pin conservative: it may report a line that is in
    fact excised, but it cannot pass one that is not.
    """

    def requires_absent(expr: str) -> bool:
        if "||" in expr:
            return False
        terms = [_strip_outer_parens(t) for t in expr.split("&&")]
        return any(re.fullmatch(rf"!\s*{re.escape(condition)}", t) for t in terms)

    excised: set[int] = set()
    stack: list[list] = []  # [this branch excised, the #if's own expression]
    for number, line in enumerate(text.splitlines(), start=1):
        stripped = _swift_code_only(line).strip()
        if stripped.startswith("#if "):
            expr = stripped[4:]
            stack.append([requires_absent(expr), expr])
        elif stripped.startswith(("#elseif ", "#elif ")) and stack:
            stack[-1][0] = requires_absent(stripped.split(" ", 1)[1])
        elif stripped == "#else" and stack:
            stack[-1][0] = _strip_outer_parens(stack[-1][1]) == condition
        elif stripped == "#endif":
            if stack:
                stack.pop()
            continue
        if any(frame[0] for frame in stack):
            excised.add(number)
    return excised


def test_the_p2p_share_region_scanner_actually_sees_a_guarded_block():
    """Self-test for the two pins below, on a literal snippet rather than the tree.

    A region scanner that over-reports makes the pins green while they cover
    nothing: every line reads as excised, so nothing is ever an offender. This
    fixes its answer on the shapes the tree actually uses.
    """
    c = _APPLE_P2P_CONDITION
    snippet = (
        "let shipped = 1\n"                                  # 1  kept
        f"#if !{c}\n"                                        # 2  excised
        "let plane = FfiSharePlaneView()\n"                  # 3  excised
        "#if os(iOS)\n"                                      # 4  excised (nested)
        "let nested = 2\n"                                   # 5  excised
        "#endif\n"                                           # 6  directive
        "#else\n"                                            # 7  kept
        "let stub = 0\n"                                     # 8  kept
        "#endif\n"                                           # 9  directive
        f"#if DEBUG && !{c}\n"                               # 10 excised
        "let testDoor = 3\n"                                 # 11 excised
        "#endif\n"                                           # 12 directive
        f"#if DEBUG || !{c}\n"                               # 13 kept (could hold)
        "let either = 4\n"                                   # 14 kept
        "#endif\n"                                           # 15 directive
        "#if !FAUNA_EXCISE_PAYMENTS\n"                       # 16 kept (other member)
        "let payments = 5\n"                                 # 17 kept
        "#endif\n"                                           # 18 directive
        f"#if {c}\n"                                         # 19 kept
        "let excisedOnly = 6\n"                              # 20 kept
        "#else\n"                                            # 21 excised
        "let defaultOnly = 7  // FfiSharePlaneView\n"        # 22 excised
        "#endif\n"                                           # 23 directive
    )
    assert _excised_lines(snippet, c) == {2, 3, 4, 5, 10, 11, 21, 22}, (
        "`_excised_lines` no longer reads Swift's #if/#else chain the way a build "
        f"defining {c} does, so the p2p-share pins below are not asserting what they "
        "claim. Fix the scanner, never the expectation."
    )
    assert _swift_code_only('let url = "https://x"  // offlineShare') == 'let url = "https://x"  '


def test_every_apple_store_safe_swift_build_passes_the_p2p_share_condition():
    """Both conditions travel together, at every store-safe Swift invocation.

    The family has one store-safe artifact, and its FFI drops `payments` and
    `p2p-share` together. A store-safe build that passed only the payments
    condition would fail to compile the share plane's leg, and would compile and
    ship the ceremony's. Read off the whole justfile rather than a named recipe
    list, so a new store-safe Swift recipe is covered on the day it is added.
    """
    spellings = (f"-Xswiftc -D{_APPLE_CONDITION}", f"-D {_APPLE_CONDITION}")
    invocations = [
        line.strip()
        for line in _JUSTFILE.read_text(encoding="utf-8").splitlines()
        if not line.lstrip().startswith("#") and any(s in line for s in spellings)
    ]
    assert len(invocations) >= 4, (
        "found fewer than the four store-safe Swift invocations this pin was written "
        "against (`mac-store-safe`, both builds in `_apple-store-safe-check-impl`, and "
        f"the iOS archive), so it is covering less than it claims: {invocations!r}"
    )
    missing = [line for line in invocations if _APPLE_P2P_CONDITION not in line]
    assert not missing, (
        f"these store-safe Swift invocations pass {_APPLE_CONDITION} but not "
        f"{_APPLE_P2P_CONDITION}, so the build compiles the share plane's leg against "
        "an FFI that does not export it, and ships the ceremony's ids "
        f"(dynamic-features.md § Platform-family surface excision): {missing!r}"
    )


def test_the_apple_witness_watches_the_p2p_share_prefixes_in_both_columns():
    """Both columns watch the member's ids: the store-safe one for absence, and
    the default one so the absence cannot be a pattern matching nothing."""
    body = _recipe_body("apple-store-safe-check")
    pattern_lines = [line for line in body.splitlines() if line.strip().startswith("for pat in ")]
    assert len(pattern_lines) == 2, (
        "apple-store-safe-check no longer has exactly the two `for pat in` columns "
        f"this pin reads: {pattern_lines!r}"
    )
    for line in pattern_lines:
        for prefix in _APPLE_P2P_ID_PREFIXES:
            assert f"'{prefix}'" in line, (
                f"an apple-store-safe-check column no longer watches '{prefix}', so a "
                "p2p-share render that forgot its condition would ship unseen"
            )


def test_every_apple_p2p_share_line_sits_behind_its_condition():
    """The structural pin for the member's apple leg, with no toolchain needed.

    Every code line (comments stripped) in a shipped apple Swift root that paints a
    `p2p-share` element id, or names the member's FFI face, must sit inside a region
    a build defining `FAUNA_EXCISE_P2P_SHARE` compiles away.

    `Generated/` is skipped: its id table has the pin below, and its i18n table
    (`L.folders.offlineShare…`) is display prose, which no criterion and no family
    gates.
    """
    tokens = _paint_tokens(_APPLE_P2P_ID_PREFIXES, "camel") + _APPLE_P2P_FFI_TOKENS
    seen: set[str] = set()
    offenders: list[str] = []
    for root in _APPLE_SHELL_ROOTS:
        for path in sorted(root.rglob("*.swift")):
            if "Generated" in path.relative_to(root).parts:
                continue
            text = path.read_text(encoding="utf-8")
            if not any(token in text for token in tokens):
                continue
            excised = _excised_lines(text, _APPLE_P2P_CONDITION)
            for number, line in enumerate(text.splitlines(), start=1):
                code = _swift_code_only(line)
                hits = [token for token in tokens if token in code]
                if not hits:
                    continue
                seen.update(hits)
                if number not in excised:
                    offenders.append(f"{path.relative_to(_REPO).as_posix()}:{number}: {line.strip()}")
    # Vacuity guard, per surface: a surface found nowhere was renamed or moved, and a
    # pin watching a spelling nothing uses quietly stops covering it. An id prefix
    # counts under either spelling, since apple paints through the generated
    # constants and the literal lives only in the skipped table.
    surfaces = [(p, _paint_tokens((p,), "camel")) for p in _APPLE_P2P_ID_PREFIXES]
    surfaces += [(t, (t,)) for t in _APPLE_P2P_FFI_TOKENS]
    unseen = [name for name, spellings in surfaces if not seen.intersection(spellings)]
    assert not unseen, (
        "these p2p-share tokens appear in no code line under "
        f"{[r.name for r in _APPLE_SHELL_ROOTS]!r}, so this pin no longer covers the "
        "surface they named. The id, type or door was renamed or removed; fix the "
        f"token list, do not delete the pin: {unseen!r}"
    )
    assert not offenders, (
        f"these apple Swift lines paint a p2p-share element id or name its FFI face "
        f"outside `#if !{_APPLE_P2P_CONDITION}`. A store-safe build either fails to "
        "compile them (the plane) or compiles them and ships the member's ids (the "
        "ceremony); dynamic-features.md § Platform-family surface excision:\n  "
        + "\n  ".join(offenders)
    )


def test_the_generated_apple_id_table_puts_every_p2p_share_id_behind_its_condition():
    """`UiIds.swift` gates the member's ids in an `extension Ids` of their own, so a
    render that forgot its own `#if` is a compile error rather than a shipped id."""
    text = _UI_IDS_SWIFT.read_text(encoding="utf-8")
    excised = _excised_lines(text, _APPLE_P2P_CONDITION)
    gated = sorted(_gated_ids("p2p-share"))
    assert gated, (
        "ui.yaml's `gated_features.p2p-share` resolved to no ids, so this pin is "
        "asserting nothing — fix the catalog, do not delete the pin."
    )
    seen: set[str] = set()
    leaked: list[str] = []
    for number, line in enumerate(text.splitlines(), start=1):
        for element_id in gated:
            if f'"{element_id}"' in line:
                seen.add(element_id)
                if number not in excised:
                    leaked.append(element_id)
    missing = [i for i in gated if i not in seen]
    assert not missing, (
        "these p2p-share ids are declared in ui.yaml's `gated_features:` block but "
        f"appear nowhere in {_UI_IDS_SWIFT.name} — regenerate with "
        f"`just ui-ids-generate`: {missing!r}"
    )
    assert not leaked, (
        f"these p2p-share element ids sit OUTSIDE the `#if !{_APPLE_P2P_CONDITION}` "
        f"region of {_UI_IDS_SWIFT.name}, so a store-safe apple build carries them "
        "(dynamic-features.md § Which element IDs belong to a gated feature): "
        f"{sorted(set(leaked))!r}"
    )


# ── the ANDROID app shell (2026-08-16, the android excision leg) ──────────────
#
# The fourth app shell and the SECOND non-Rust one, and its mechanics are its
# own from top to bottom (`dynamic-features.md` § Platform-family surface
# excision):
#
#   * tui and linux are cargo crates, so their flavor is a manifest fact and the
#     section above reads it. Android's is a Gradle **build type**.
#   * apple's flavor is a command-line compilation condition, and Swift's `#if`
#     removes the render and its FFI reference together. **Kotlin has no inline
#     compile-time exclusion at all**: `if (BuildConfig.PAYMENTS)` is a runtime
#     branch whose body must still typecheck. So android splits in two — the
#     GLUE moves to per-build-type source sets (`src/payments` vs
#     `src/noPayments`), and the RENDER stays shared behind the constant, with
#     R8 folding it out of the artifact.
#
# That split is why this section pins two things the other shells never needed:
# that no shared source names a payments FFI symbol (the glue half, which the
# compiler enforces only when someone actually builds the storeSafe variant),
# and that every shared source painting a payments element id carries the
# constant (the render half, which nothing but the witness can otherwise see).
# Both are pure text reads, so they run on every machine — only one dev machine
# carries an Android SDK and can build an APK at all, and a session without that
# toolchain must still not be able to add an ungated surface silently.

_ANDROID_CONDITION = "BuildConfig.PAYMENTS"
_ANDROID_APP = _REPO / "apps/fauna-android/app"
_ANDROID_GRADLE = _ANDROID_APP / "build.gradle.kts"
_ANDROID_SHARED_SRC = _ANDROID_APP / "src/main/java"
# The shell's kids-excised surfaces (the feed and its tip render, search, every
# bridge, …) live in `src/noKids/java` (`test_kids_excision_spine.py`), which
# `debug`, `release`, `storeSafe` and `foss` all compile — so for the payments
# and p2p-share axes it is as SHARED as `src/main`, and the pins below scan both.
_ANDROID_SHARED_SRCS = (_ANDROID_SHARED_SRC, _ANDROID_APP / "src/noKids/java")
# The two variant source sets — the ONLY android code allowed to name a
# payments FFI symbol, because they are compiled into disjoint build types.
_ANDROID_VARIANT_SRCS = [
    _ANDROID_APP / "src/payments/java",
    _ANDROID_APP / "src/noPayments/java",
]
# TYPES `fauna-ffi` compiles out under `--features store-safe`, as they appear
# in the generated Kotlin face. Derived from the `#[cfg(feature = "payments")]`
# sites in libs/fauna-ffi (payments_client.rs's four records + the client).
#
# Types only, and deliberately so: the excised free FUNCTIONS
# (`paymentsKnownKinds`, `paymentsWebhookUrl`) and the excised METHOD
# (`FfiFeedManager.resolvePostTips`) share their bare names with app-side
# declarations by design — the seam extensions kept the old `ApiClient` method
# names so no call site had to change, and `FeedVM` has its own
# `resolvePostTips`. A bare-name grep flags all of those and proves nothing. The
# qualified/receiver-scoped rules below cover them precisely instead.
#
# `zaps` is a SUBSET member of `payments` (`dynamic-features.md` § Charter
# members) and rides the SAME source-set split — `FfiNostrZapSignerClient` /
# `FfiZapSignerEntry` are the two types this list was missing between
# 2026-08-28 (when the shared glue landed as `ApiClient` members naming them
# directly) and this row's fix, the whole reason `storeSafe` stopped
# compiling with no pin anywhere catching it.
_ANDROID_ABSENT_FFI_TYPES = [
    "FfiPaymentsClient",
    "FfiProviderItem",
    "FfiClaimItem",
    "FfiClaimRedeemReply",
    "FfiClaimMintReply",
    "FfiNostrZapSignerClient",
    "FfiZapSignerEntry",
]
# Excised free functions, matched only where they are QUALIFIED by the generated
# package (which is how shared code would have to reach them — the app-side
# extensions of the same name live in `com.fauna.app.payments`).
_ANDROID_ABSENT_FFI_FUNCTIONS = ["paymentsKnownKinds", "paymentsWebhookUrl"]
_ANDROID_ELEMENT_ID_PREFIXES = (
    "subscription-provider-",
    "subscription-claim-",
    "post-tip-",
    "nostr-zap-signer-",
)


def _android_shared_sources() -> list[Path]:
    """Hand-written Kotlin in the shared source sets (`src/main` + `src/noKids`).

    ⚠ The generated UniFFI face is staged INTO this tree (`just android-ffi`
    writes `com/fauna/ffi/`), and it is gitignored — the repo stopped committing
    generated bindings. Scanning it makes every pin below answer
    a different question depending on whether the machine happens to have built
    the FFI: on a clean checkout the file is absent and the pins pass; on any
    machine that has run `just android-ffi` the *default*-flavor face names
    `FfiPaymentsClient` and friends by construction, and
    `test_no_shared_android_source_names_a_payments_ffi_symbol` fails claiming a
    hand-written leak that does not exist (found 2026-08-16, on the primary dev
    VM). A pin whose
    verdict depends on untracked build output is worse than no pin: it is red for
    a reason nobody can act on, and the next session learns to ignore it.

    The generated face is exactly what the store-safe flavor REGENERATES without
    those symbols, so it is never the thing these pins are about — they ask what
    *app* code names.
    """
    generated = _ANDROID_SHARED_SRC / "com/fauna/ffi"
    return sorted(
        p
        for root in _ANDROID_SHARED_SRCS
        for p in root.rglob("*.kt")
        if generated not in p.parents
    )


def _kotlin_code_only(text: str) -> str:
    """Kotlin source with comments removed.

    Every pin below is about what the COMPILER sees. Kotlin comments never reach
    the dex, so a KDoc naming `FfiProviderItem` or quoting a `post-tip-` id — the
    seam types' own documentation does both, as it must — is not a leak, and a
    pin that flags it would be pure noise the next session learns to suppress.

    (This differs from criterion 2's rule for *Rust* doc comments, which DO ship:
    UniFFI embeds them in the library metadata and propagates them into the
    generated face. That is a property of the bindgen, not of Kotlin.)

    `//` is left alone when preceded by `:` so a `"https://…"` literal does not
    swallow the rest of its line. Nested block comments (a real Kotlin quirk) are
    under-stripped rather than over-stripped, which can only make a pin stricter.
    """
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return re.sub(r"(?<!:)//[^\n]*", "", text)


def test_the_android_ffi_has_a_store_safe_flavor():
    """`just android-ffi-store-safe` is the escape hatch's android FFI recipe —
    part of the row `dynamic-features.md` § The App-Store escape hatch's table
    carried as **Open** for windows/android/web since 2026-08-10."""
    assert "_android-ffi-flavor storeSafe store-safe" in _recipe_body("android-ffi-store-safe"), (
        "android-ffi-store-safe must delegate to the shared flavor impl with the "
        "store-safe axis; a second copy of the 4-ABI body would drift from the "
        "production one"
    )
    body = _recipe_body("_android-ffi-flavor")
    assert "--no-default-features --features store-safe" in body, (
        "_android-ffi-flavor no longer spells the store-safe flavor as the COMPLEMENT "
        "feature. A hand-listed excision set rots the first time a surface is added to "
        "one list and not the other (dynamic-features.md § The cargo feature spine)."
    )


def test_the_android_flavor_marker_covers_the_abi_set():
    """The witness builds one ABI to avoid eight release FFI builds, which makes
    the ABI set part of the flavor identity: an `arm64` run leaves the other
    three triples' `.so`s at whatever flavor built them last, so a later 4-ABI
    run that skipped cargo on a "fresh" stamp would stage three stale-flavor
    libraries beside one current one — the deterministic wrong-flavor false-green
    the recipe's own comment already records for the FEATURE axis."""
    body = _recipe_body("_android-ffi-flavor")
    assert '"$FEATURE_FLAGS:$ABIS:$PROFILE"' in body, (
        "the android FFI flavor marker no longer records the ABI set, so a one-ABI "
        "witness run can leave a later full build trusting three stale .so files"
    )


def test_the_android_store_safe_app_recipe_excises_both_halves():
    """An excised flavor is the FFI built without the feature **plus** the shell
    built with the matching condition off. Half of it is a build that compiles
    perfectly and ships the plane."""
    assert "android-ffi-store-safe" in _recipe_header("android-store-safe"), (
        "android-store-safe no longer takes the store-safe FFI, so its APK would "
        "package a payments-carrying libfauna_ffi.so and carry every kind string with it"
    )
    assert "assembleStoreSafe" in _recipe_body("android-store-safe"), (
        "android-store-safe no longer assembles the storeSafe build type, so the shell "
        "renders (and ships the element ids of) a plane the FFI cannot reach"
    )


def test_the_android_build_type_carries_the_family_compile_condition():
    """android's family condition is a `BuildConfig` constant + R8 stripping.

    The constant must be TRUE in the shipping and debug types and FALSE in
    `storeSafe`, and `storeSafe` must `initWith(release)` rather than hand-copy
    it: an excised artifact has to be the shipping one minus the members and
    nothing else, and a hand-copied block silently drifts on minification or
    proguard files — precisely the axes that decide whether the folded branch is
    actually stripped.
    """
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")
    assert 'create("storeSafe")' in text, (
        "apps/fauna-android lost its `storeSafe` build type — the android leg of the "
        "App-Store escape hatch (dynamic-features.md § The App-Store escape hatch)"
    )
    assert 'initWith(getByName("release"))' in text, (
        "the storeSafe build type no longer initWith's release, so the excised APK can "
        "drift from the shipping one on minification/proguard — the settings that "
        "decide whether R8 strips the folded payments branches at all"
    )
    assert 'buildConfigField("boolean", "PAYMENTS", "false")' in text, (
        "the storeSafe build type no longer sets PAYMENTS=false, so every payments "
        "render stays live and the APK ships the whole surface"
    )
    assert text.count('buildConfigField("boolean", "PAYMENTS", "true")') >= 2, (
        "the debug and release build types must both set PAYMENTS=true — a plain build "
        "ships the plane, which is the posture dynamic-features.md § Goal asks for"
    )


def test_the_android_glue_lives_in_disjoint_variant_source_sets():
    """The source-set split IS android's compile-time exclusion.

    Kotlin cannot remove a reference with a runtime `if`, so the payments glue
    lives in `src/payments` (debug + release) and `src/noPayments` (storeSafe).
    Wiring both into the same build type — or dropping one — would either
    duplicate-declare every extension or leave the storeSafe variant naming
    symbols its bindings lack.
    """
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")

    def _src_dirs(build_type: str) -> str:
        """The `sourceSets { getByName("<type>") { … } }` body, formatting-agnostic.

        Matched on braces rather than on an exact one-liner: the block grew to
        multiple lines the day `src/noAgent` joined it, and a pin that breaks on
        reformatting teaches the next session to loosen it rather than read it.
        """
        m = re.search(rf'getByName\("{build_type}"\)\s*\{{([^}}]*)\}}', text)
        assert m, f"no sourceSets entry for the `{build_type}` build type in {_ANDROID_GRADLE}"
        return m.group(1)

    for build_type, srcs in (
        ("debug", ["src/payments/java"]),
        # Both SHIPPING flavors take the inert automation twin: `src/noAgent`
        # holds the same-signature `TestAgent` stub that `src/main`'s ~10
        # production mentions need in order to compile at all once the real
        # agent stays behind in `src/debug` (convention 15). It could live in
        # `src/release/java` while release was the only shipping flavor; it
        # cannot now, because that directory also holds release's staged UniFFI
        # bindings and storeSafe must not take those.
        ("release", ["src/payments/java", "src/noAgent/java"]),
        ("storeSafe", ["src/noPayments/java", "src/noAgent/java"]),
    ):
        body = _src_dirs(build_type)
        for src in srcs:
            assert f'java.srcDir("{src}")' in body, (
                f"the `{build_type}` build type no longer takes `{src}` as a source dir — "
                "the variant seams (com.fauna.app.payments.PaymentsGlue for the payments "
                "glue, com.fauna.app.testing.TestAgent for the automation twin) are what "
                "keep each flavor compiling without naming symbols its own build lacks"
            )
    # The excised flavor must not reach the built glue, or the twins would
    # duplicate-declare and the whole seam would be decorative.
    assert 'java.srcDir("src/payments/java")' not in _src_dirs("storeSafe"), (
        "the storeSafe build type takes the BUILT payments glue as well as the excised "
        "twin — every extension would be declared twice and the excision is undone"
    )
    for src in _ANDROID_VARIANT_SRCS:
        glue = src / "com/fauna/app/payments/PaymentsGlue.kt"
        assert glue.is_file(), f"missing the payments glue twin at {glue}"
        # `zaps` rides the SAME split as a subset member of `payments` (its own
        # file rather than folded into PaymentsGlue.kt — it maps a different FFI
        # client) — this pair is what the 2026-08-28 regression fixed: before it,
        # this glue lived as `ApiClient` members naming the FFI zap types directly,
        # which a store-safe build's bindings do not export.
        zap_glue = src / "com/fauna/app/payments/ZapSignerGlue.kt"
        assert zap_glue.is_file(), f"missing the zap-signer glue twin at {zap_glue}"

    # The twins must stay signature-compatible: every extension the built half
    # declares needs a same-named twin, or the excised variant stops compiling
    # the moment a caller is added. Names only — a text pin cannot check types,
    # and the storeSafe compile inside the witness is what does.
    def _fun_names(path: Path) -> set[str]:
        text = path.read_text(encoding="utf-8")
        return set(re.findall(r"^(?:suspend )?fun \w+\.(\w+)", text, re.MULTILINE))

    built = _fun_names(_ANDROID_VARIANT_SRCS[0] / "com/fauna/app/payments/PaymentsGlue.kt")
    excised = _fun_names(_ANDROID_VARIANT_SRCS[1] / "com/fauna/app/payments/PaymentsGlue.kt")
    assert built, "the built payments glue declares no extensions at all — the pin is vacuous"
    assert built == excised, (
        "the two payments glue twins declare different extension sets, so one build "
        f"type will not compile: only in built={sorted(built - excised)!r}, only in "
        f"excised={sorted(excised - built)!r}"
    )

    zap_built = _fun_names(_ANDROID_VARIANT_SRCS[0] / "com/fauna/app/payments/ZapSignerGlue.kt")
    zap_excised = _fun_names(_ANDROID_VARIANT_SRCS[1] / "com/fauna/app/payments/ZapSignerGlue.kt")
    assert zap_built, "the built zap-signer glue declares no extensions at all — the pin is vacuous"
    assert zap_built == zap_excised, (
        "the two zap-signer glue twins declare different extension sets, so one build "
        f"type will not compile: only in built={sorted(zap_built - zap_excised)!r}, only in "
        f"excised={sorted(zap_excised - zap_built)!r}"
    )


def test_no_shared_android_source_names_a_payments_ffi_symbol():
    """The glue half, caught without building the storeSafe variant.

    A store-safe APK links a `fauna-ffi` whose Kotlin face has no
    `FfiPaymentsClient` / `FfiProviderItem` / `resolvePostTips`, so any file
    outside the two variant source sets that names one fails to compile in that
    flavor. The compiler does say so — but only when someone builds the variant,
    and that build is the heaviest gate android has. This pin says so on every
    machine, for free.
    """
    offenders = {}
    scanned = 0
    for path in _android_shared_sources():
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        scanned += 1
        hits = [t for t in _ANDROID_ABSENT_FFI_TYPES if re.search(rf"\b{re.escape(t)}\b", code)]
        hits += [
            f"com.fauna.ffi.{f}"
            for f in _ANDROID_ABSENT_FFI_FUNCTIONS
            if re.search(rf"com\.fauna\.ffi\.{re.escape(f)}\b", code)
        ]
        # The tip resolver is a METHOD, so the receiver is what identifies it:
        # `FfiFeedManager` is the only type that has one, and a shared file that
        # both imports it and calls `.resolvePostTips(` is calling the excised
        # member. `FeedVM`'s own same-named function and the seam's
        # `resolvePostTipsIfBuilt` are both correctly ignored.
        if "com.fauna.ffi.FfiFeedManager" in code and re.search(r"\.resolvePostTips\s*\(", code):
            hits.append("FfiFeedManager.resolvePostTips")
        if hits:
            offenders[path.relative_to(_REPO).as_posix()] = hits
    assert scanned, f"no Kotlin sources found under {_ANDROID_SHARED_SRC} — the pin is vacuous"
    assert not offenders, (
        "these SHARED android sources name FFI symbols a store-safe build does not "
        "export, so `just android-store-safe` cannot compile. Move the reference into "
        "app/src/payments/ (with a twin in app/src/noPayments/) and let the shared code "
        f"talk to the seam instead: {offenders!r}"
    )


def test_every_android_payments_render_carries_the_compile_condition():
    """The render trap, caught without an Android toolchain.

    Gating the glue is the half the compiler does for you. The half it cannot
    see is a Compose section that compiles fine — `PostSummary.tips` is a
    deliberately ungated inert record, so it stays null forever — paints
    nothing, and still ships every `Modifier.testTag(...)` it would have
    painted. Dead is not absent. So any shared source carrying a payments
    element id must also carry the constant that removes it.

    This is the same pin apple's `#if !FAUNA_EXCISE_PAYMENTS` source check makes,
    and it matters at least as much here: apple's absence is the Swift compiler's
    doing, android's is R8's, and an optimizer's willingness is not a structural
    guarantee.
    """
    tokens = _paint_tokens(_ANDROID_ELEMENT_ID_PREFIXES, "scream")
    offenders = []
    carriers = []
    for path in _android_shared_sources():
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        if not any(token in code for token in tokens):
            continue
        carriers.append(path.relative_to(_REPO).as_posix())
        if _ANDROID_CONDITION not in code:
            offenders.append(path.relative_to(_REPO).as_posix())
    assert carriers, (
        f"no source under {_ANDROID_SHARED_SRC} carries a payments element id at all, "
        "so this pin is asserting nothing. Either the tree moved or the ids were "
        "renamed — in both cases fix the pin, do not delete it."
    )
    assert not offenders, (
        "these shared android sources render payments element ids with no "
        f"`{_ANDROID_CONDITION}` anywhere in the file, so a store-safe APK ships every "
        'id they paint (dynamic-features.md § What "completely compiled away" means, '
        f"criterion 1): {offenders!r}"
    )


def test_the_android_app_shell_has_its_own_two_column_artifact_witness():
    """Criterion 1 ("no UI surface — the feature's element IDs absent") lives in a
    shell, and android's is Kotlin+R8: no ffi/wasm/nest column can see it, and
    neither can tui's or linux's — the shells render the money plane from
    different code.

    ⚠ Unpacking is load-bearing, not tidiness. An APK is a ZIP: dex string
    constants are deflate-compressed inside it, so `strings -a app.apk` reports
    zero matches for everything and reads exactly like a perfect excision.
    """
    body = _recipe_body("android-store-safe-check")
    assert "_android-ffi-flavor storeSafe store-safe" in body, (
        "android-store-safe-check's first column no longer stages the store-safe FFI — "
        "the kind-string assertion would then be about R8's dead-stripping luck rather "
        "than about an artifact that structurally cannot reach the plane"
    )
    assert "assembleStoreSafe" in body, (
        "android-store-safe-check's first column no longer builds the excised APK"
    )
    assert "unzip" in body and "classes*.dex" in body, (
        "android-store-safe-check no longer UNPACKS the APK before grepping it. A dex "
        "is deflate-compressed inside the archive, so scanning the .apk directly finds "
        "nothing in either column and the whole witness passes vacuously."
    )
    assert "lib/*/*.so" in body, (
        "android-store-safe-check no longer scans the packaged native library, so "
        "criterion 2 (kind strings) is unasserted for the artifact that actually ships"
    )
    for pat in _ANDROID_ELEMENT_ID_PREFIXES:
        assert pat in body, f"android-store-safe-check lost its '{pat}' element-id pattern"
    assert "fauna\\.payments\\." in body, "android-store-safe-check lost its kind-string pattern"
    assert "fauna\\.nostr\\.zap_signers\\." in body, (
        "android-store-safe-check lost its zap-signer kind-string pattern — `zaps` is a "
        "subset member of `payments` and excises with it, so this pattern must be absent "
        "from the store-safe column and present in the release one exactly as the "
        "payments pattern is"
    )
    assert "assembleRelease" in body, (
        "android-store-safe-check's second column no longer builds the DEFAULT APK, so "
        "its store-safe assertion is vacuous — a grep matching nothing would pass, and "
        "so would an unpack that extracted zero files"
    )
    assert "vacuous" in body, (
        "android-store-safe-check lost the comment explaining why the second column exists"
    )


def test_the_android_witness_is_reachable_even_though_it_is_not_gated_yet():
    """The recipe exists and `merge-gate-check.sh` explains why it is not run.

    Unlike its three siblings, this column is not in the merge-gate check — a
    deliberate, recorded exemption, settled 2026-08-18 with the measured number
    the 2026-08-16 note asked for (~10 min of warm build work out of a 2-slot
    pool, on a scope that must include `^libs/` and so fires on nearly every
    pass) — the same reason apple's column stays out of the mac check. The
    witness has run green by hand twice (2026-08-16 pre-id-table, 2026-08-18
    post-adoption). This pin is what stops the exemption decaying into an
    oversight: the script must keep SAYING so, so a later reader finds a
    decision instead of a gap.
    """
    gate_script = _REPO / "scripts/merge-gate-check.sh"
    if not gate_script.exists():
        pytest.skip("merge-gate-check.sh is fleet-only tooling and does not ship")
    script = gate_script.read_text(encoding="utf-8")
    assert "android-store-safe-check" in script, (
        "merge-gate-check.sh no longer mentions android-store-safe-check at all. Either "
        "register it (with its scope constant) or keep the note explaining why not — "
        "silence turns a recorded decision into an invisible coverage gap."
    )
    assert "SCOPE_ANDROID_STORE_SAFE" in script, (
        "the android scope constant is gone. It is what makes the gate registrable "
        "without re-deriving that android's render trap is a Kotlin edit, invisible to "
        "the `.rs` scope the tui and linux columns ride."
    )


# ── android's `p2p-share` leg — its own `P2P_SHARE` build-config field ────────
# `p2p-share` is a registry member of its own and NOT a subset of `payments`, so
# android gives it its own condition (one condition per member): a
# `BuildConfig.P2P_SHARE` field for the RENDER, and a `src/p2pShare` /
# `src/noP2pShare` twin of `com.fauna.app.p2pshare.OfflineShareHost` for the
# GLUE — the payments shape above, one member over. Android renders only the
# member's ceremony half (the offline co-present share on the Folders page);
# the plane half's two prefixes are painted nowhere on android.
_ANDROID_P2P_CONDITION = "BuildConfig.P2P_SHARE"
_ANDROID_P2P_VARIANT_SRCS = [
    _ANDROID_APP / "src/p2pShare/java",
    _ANDROID_APP / "src/noP2pShare/java",
]
_ANDROID_P2P_HOST = "com/fauna/app/p2pshare/OfflineShareHost.kt"
# The ceremony's UniFFI face as the generated Kotlin spells it. `mod
# offline_share` (fauna-ffi) and `group_ceremony_view` (fauna-client-capabilities)
# are feature-gated whole, so a store-safe build's bindings have none of these.
# Word-bounded, as windows' list: the custody and recovery ceremonies own
# unrelated names. The free functions are matched QUALIFIED
# (`com.fauna.ffi.offlineShare…`), which is how shared code would reach them.
_ANDROID_P2P_FACES = (
    "FfiCeremonySeat", "CeremonyStatus", "OfflineSharePanel", "FfiGroupShareViews",
    "FfiPendingGroupShare", "FfiGroupScope", "OfflineShareView", "OfflineShareGates",
    "PeerCodeParsed", "PeerCodeError",
)
_ANDROID_P2P_FUNCTION_PREFIX = r"com\.fauna\.ffi\.offlineShare"


def test_the_android_build_type_carries_the_p2p_share_condition():
    """False on `storeSafe`, true on every other build type (debug, release, foss):
    the plain build ships the member, the excised one folds its render."""
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")
    assert 'buildConfigField("boolean", "P2P_SHARE", "false")' in text, (
        "the storeSafe build type no longer sets P2P_SHARE=false, so the ceremony "
        "render stays live and the APK ships every offline-share id"
    )
    assert text.count('buildConfigField("boolean", "P2P_SHARE", "true")') >= 3, (
        "debug, release and foss must each set P2P_SHARE=true — a missing field is a "
        "compile error in that build type, and a false one drops the ceremony from it"
    )


def test_the_android_p2p_share_glue_lives_in_disjoint_variant_source_sets():
    """The source-set split IS android's compile-time exclusion for the ceremony:
    `storeSafe` takes the inert twin and never the built one, every other build
    type the built one; and the two twins declare the same public surface."""
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")

    def _src_dirs(build_type: str) -> str:
        m = re.search(rf'getByName\("{build_type}"\)\s*\{{([^}}]*)\}}', text)
        assert m, f"no sourceSets entry for the `{build_type}` build type in {_ANDROID_GRADLE}"
        return m.group(1)

    for build_type in ("debug", "release", "foss"):
        body = _src_dirs(build_type)
        assert 'java.srcDir("src/p2pShare/java")' in body, (
            f"the `{build_type}` build type no longer takes src/p2pShare/java, so it "
            "has no OfflineShareHost at all"
        )
        assert 'java.srcDir("src/noP2pShare/java")' not in body, (
            f"the `{build_type}` build type takes the INERT ceremony twin — it would "
            "ship a Folders page whose ceremony does nothing"
        )
    body = _src_dirs("storeSafe")
    assert 'java.srcDir("src/noP2pShare/java")' in body, (
        "the storeSafe build type no longer takes src/noP2pShare/java"
    )
    assert 'java.srcDir("src/p2pShare/java")' not in body, (
        "the storeSafe build type takes the BUILT ceremony glue, which names FFI types "
        "its bindings do not export — the excision is undone (or the build breaks)"
    )

    def _public_members(path: Path) -> set[str]:
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        return set(re.findall(r"^    (?:suspend )?(?:fun|val) (\w+)", code, re.MULTILINE))

    built, excised = (_public_members(src / _ANDROID_P2P_HOST) for src in _ANDROID_P2P_VARIANT_SRCS)
    assert built, "the built OfflineShareHost declares no public members — the pin is vacuous"
    assert built == excised, (
        "the two OfflineShareHost twins declare different public members, so one build "
        f"type will not compile: only in built={sorted(built - excised)!r}, only in "
        f"excised={sorted(excised - built)!r}"
    )


def test_no_shared_android_source_names_a_ceremony_ffi_symbol():
    """The glue half, caught without building the storeSafe variant: a ceremony
    face named outside the two twins does not compile in the store-safe flavor."""
    offenders = {}
    scanned = 0
    for path in _android_shared_sources():
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        scanned += 1
        hits = [f for f in _ANDROID_P2P_FACES if re.search(rf"\b{re.escape(f)}\b", code)]
        if re.search(_ANDROID_P2P_FUNCTION_PREFIX, code):
            hits.append("com.fauna.ffi.offlineShare*")
        if hits:
            offenders[path.relative_to(_REPO).as_posix()] = hits
    assert scanned, f"no Kotlin sources found under {_ANDROID_SHARED_SRC} — the pin is vacuous"
    assert not offenders, (
        "these SHARED android sources name a ceremony UniFFI face a store-safe build "
        "does not export. Move the reference into app/src/p2pShare/ (with its twin in "
        f"app/src/noP2pShare/) and let the shared code talk to the seam: {offenders!r}"
    )


def test_every_android_p2p_share_render_carries_its_compile_condition():
    """THE RENDER TRAP for this member: a ceremony section that compiles in both
    flavors, paints nothing in store-safe (the inert host answers `null`), and
    still ships every id it would have painted. Dead is not absent."""
    prefixes = tuple(_gated_catalog()["p2p-share"]["id_prefixes"])
    assert prefixes, "ui.yaml declares no p2p-share id prefixes, so this pin asserts nothing"
    tokens = _paint_tokens(prefixes, "scream")
    offenders = []
    carriers = []
    for path in _android_shared_sources():
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        if not any(token in code for token in tokens):
            continue
        carriers.append(path.relative_to(_REPO).as_posix())
        if _ANDROID_P2P_CONDITION not in code:
            offenders.append(path.relative_to(_REPO).as_posix())
    assert carriers, (
        f"no source under {_ANDROID_SHARED_SRC} paints a p2p-share element id at all, "
        "so this pin asserts nothing — fix the pin, do not delete it"
    )
    assert not offenders, (
        "these shared android sources paint p2p-share element ids with no "
        f"`{_ANDROID_P2P_CONDITION}` anywhere in the file: {offenders!r}"
    )


def test_the_android_witness_watches_the_p2p_share_member():
    """All four catalog prefixes in the store-safe column; in the DEFAULT column,
    the prefix the default dex MEASURABLY carries. The plane's two are painted
    nowhere on android, and `offline-receive-` measured 0 in the default release
    dex on 2026-09-30: R8 folds the ceremony section's gates in every minified
    build, a release-only defect that predates this leg. Add `offline-receive-` here and to the recipe when that lands."""
    body = _recipe_body("android-store-safe-check")
    ss_col, sep, default_col = body.partition("column 2")
    assert sep, "android-store-safe-check lost its `column 2` marker, so the columns cannot be told apart"
    for pat in _gated_catalog()["p2p-share"]["id_prefixes"]:
        assert f"'{pat}'" in ss_col, f"android-store-safe-check's store-safe column lost its '{pat}' pattern"
    for pat in ("offline-share-",):
        assert f"'{pat}'" in default_col, (
            f"android-store-safe-check's DEFAULT column no longer checks '{pat}', so "
            "the store-safe absence is indistinguishable from a renamed id"
        )


# ── the WEB app shell (2026-08-16, the web excision leg) ─────────────────────
#
# The fifth app shell and the THIRD non-Rust one. Its mechanics are unlike all
# four above (`dynamic-features.md` § Platform-family surface excision):
#
#   • the flavor is a **vite define** (`__FAUNA_PAYMENTS__`, fed by the
#     `FAUNA_WEB_PAYMENTS` env var), so — as with apple's compilation condition —
#     no manifest anywhere states which flavor an artifact is. Every pin here is
#     therefore recipe- or source-shaped;
#   • the define is POSITIVE and defaults ON, which apple's could not be: a vite
#     `define` is a plain config expression, so a build CAN remove a condition,
#     and the flavor-root rule ("a plain build ships the plane") applies as
#     written;
#   • and the removal is done by the **isolated-module pattern**, not by a
#     compiler. Folding a branch to `false` does not by itself unlink a module,
#     so the glue lives in exactly one place — `$lib/payments` — and the renders
#     in `$lib/components/payments/`. Both are imported ONLY from behind the
#     condition, and the three source pins below are what keep it that way.
#
# ⚠ web's criterion-2 axis is FACE NAMES, not kind strings. The SPA never spells
# a `fauna.payments.*` kind — it calls a wasm-bindgen export by name, and the
# only kind strings in web source are comments the build strips. Measured
# 2026-08-16 against the default bundle: every element-id prefix is present and
# `fauna\.payments\.` is **0**, so a kind-string pattern here would match nothing
# in BOTH columns — the vacuity the second column exists to prevent.

_WEB_SRC = _REPO / "apps/fauna-web/src"
_WEB_VITE_CONFIG = _REPO / "apps/fauna-web/vite.config.ts"
_WEB_CONDITION = "__FAUNA_PAYMENTS__"

# The one glue module and the one render directory. Everything else under `src/`
# is "shared" — unconditionally in every bundle.
_WEB_GLUE_MODULE = _WEB_SRC / "lib/payments.ts"
_WEB_COMPONENT_DIR = _WEB_SRC / "lib/components/payments"

_WEB_ELEMENT_ID_PREFIXES = (
    "subscription-provider-", "subscription-claim-", "post-tip-",
    "subscription-tier-form-asking-price", "compose-sell-asking-price",
)

# The faces a store-safe bundle must not name — web's twin of the UniFFI symbols
# `ffi-store-safe-check` greps. Kept in step with `web-store-safe-check`.
_WEB_PAYMENTS_FACES = (
    "paymentsProvidersSet",
    "paymentsProvidersList",
    "paymentsProvidersRemove",
    "paymentsClaimsMint",
    "paymentsClaimsList",
    "paymentsClaimsRedeem",
    "paymentsKnownKinds",
    "paymentsWebhookUrl",
    "tipAmount",
    "tipCount",
    "tipMore",
    "claimStatusLabel",
    "providerStatusLabel",
)


def _web_sources() -> list[Path]:
    """Every `.ts` / `.svelte` source under the SPA, in a stable order."""
    return sorted(
        p
        for p in _WEB_SRC.rglob("*")
        if p.is_file() and p.suffix in (".ts", ".svelte") and not p.name.endswith(".test.ts")
    )


def _web_shared_sources() -> list[Path]:
    """SPA sources that are NOT part of the isolated payments surface.

    These ship in every flavor, so anything they name survives into the
    store-safe bundle no matter what the define is set to.
    """
    return [
        p for p in _web_sources() if p != _WEB_GLUE_MODULE and _WEB_COMPONENT_DIR not in p.parents
    ]


def _web_code_only(text: str) -> str:
    """Strip comments, so a doc comment naming a face or an id is not a hit.

    These files are heavily commented BY DESIGN — the excision rules are
    explained where they are enforced — so a naive substring scan would report
    every explanatory note as a leak.

    ⚠ The JS arms match only comments that OPEN THEIR OWN LINE, and that is a
    correctness requirement rather than a simplification. A general `/\\*.*?\\*/`
    over a `.svelte` file matches inside string literals too: measured
    2026-08-16, `accept="image/*,video/*"` in `routes/feed/+page.svelte` opened a
    fake block comment that swallowed 4,109 characters of real markup — the
    `{#if __FAUNA_PAYMENTS__}` gate among them, which made this module's own
    pins report a false leak. Over-stripping is the dangerous direction here: it
    hides real ones. Every explanatory comment in these sources starts its own
    line, so the narrow form loses nothing; a TRAILING comment naming a face or
    an id will be reported, which is the safe way to be wrong (move the prose to
    its own line).
    """
    text = re.sub(r"<!--.*?-->", "", text, flags=re.DOTALL)
    text = re.sub(r"^[ \t]*/\*.*?\*/", "", text, flags=re.DOTALL | re.MULTILINE)
    return re.sub(r"^[ \t]*//[^\n]*", "", text, flags=re.MULTILINE)


def _web_payments_imports(text: str) -> list[str]:
    """Import specifiers naming the isolated payments surface.

    Matched as import STATEMENTS on the raw text rather than as substrings: the
    surface is discussed in prose all over these files, and a relative
    specifier (`./payments/TipSurface.svelte`, which is how a sibling component
    imports it) shares no prefix with the `$lib/` form a substring scan would
    look for. Any specifier containing `payments` is this surface — the SPA has
    no other module by that name.
    """
    return [
        spec
        for spec in re.findall(
            r"^[ \t]*import\s[^\n]*?from\s*['\"]([^'\"]+)['\"]", text, re.MULTILINE
        )
        if "payments" in spec
    ]


def test_the_web_shell_declares_its_compile_condition_positively():
    """The define exists, reads the env var the recipe sets, and defaults ON.

    Direction is the whole assertion. A flavor root's plain build must ship the
    plane (`dynamic-features.md` § Platform-family surface excision), so the
    default has to be `true` and the *flavor* is what turns it off — the reverse
    of apple's negative condition, which the Swift toolchain forced. A define
    written `=== '1'` instead would make every ordinary build a store-safe one
    and silently drop the plane from production.
    """
    config = _WEB_VITE_CONFIG.read_text(encoding="utf-8")
    m = re.search(rf"{re.escape(_WEB_CONDITION)}\s*:\s*(.+)", config)
    assert m, (
        f"`{_WEB_CONDITION}` is no longer defined in {_WEB_VITE_CONFIG.name}, so the web "
        "shell has no compile condition at all and every build ships the payments plane"
    )
    expr = m.group(1)
    assert "FAUNA_WEB_PAYMENTS" in expr, (
        f"`{_WEB_CONDITION}` no longer reads the FAUNA_WEB_PAYMENTS env var, so "
        f"`just web-store-safe` cannot switch the flavor. Got: {expr.strip()}"
    )
    assert "!==" in expr and "'0'" in expr, (
        f"`{_WEB_CONDITION}` must default ON — the flavor turns it OFF, not the other way "
        "round. A positive test (`=== '1'`) makes every plain build store-safe and drops "
        f"the plane from production without failing anything. Got: {expr.strip()}"
    )


def test_no_shared_web_source_names_a_payments_face():
    """The glue half — the isolated-module pattern's actual invariant.

    Unlike a cargo feature or a Swift `#if`, a folded vite define does not
    detach a module: the branch disappears, the *import* beside it does not
    necessarily, and even when it does, a function defined in an
    unconditionally-bundled module keeps its name in the artifact. So every
    payments face must live in `$lib/payments`, whose only importers are the
    render components — themselves imported only from behind the condition.

    `$lib/rpc`, `$lib/wasm` and `$lib/value-format` are where these functions
    naturally belong and where they used to live; all three are in every bundle.
    That is precisely why they had to move.
    """
    offenders: dict[str, list[str]] = {}
    scanned = 0
    for path in _web_shared_sources():
        code = _web_code_only(path.read_text(encoding="utf-8"))
        scanned += 1
        hits = [f for f in _WEB_PAYMENTS_FACES if re.search(rf"\b{re.escape(f)}\b", code)]
        if hits:
            offenders[path.relative_to(_REPO).as_posix()] = hits
    assert scanned, f"no SPA sources found under {_WEB_SRC} — the pin is vacuous"
    assert not offenders, (
        "these SHARED web sources name a payments face, so the store-safe bundle carries "
        'its name even with every caller folded away (dynamic-features.md § What '
        '"completely compiled away" means, criterion 2 — web\'s axis is face names, since '
        "the SPA never spells a kind string). Move the function into `$lib/payments` and "
        f"let a `$lib/components/payments/` component call it: {offenders!r}"
    )


def test_no_shared_web_source_paints_a_payments_element_id():
    """The render trap, in web's idiom and caught without a build.

    tui, linux, apple and android all keep their payments renders in shared
    files and gate them in place. web cannot: a Svelte component's template is a
    module-level construct, so an id inside a folded `{#if}` may still be emitted
    with the module. The web answer is to move the render OUT — into
    `$lib/components/payments/` — which makes the invariant checkable as an
    absence rather than as the presence of a condition.

    `PostSummary.tips` is why this cannot be skipped: it is a deliberately
    ungated inert record, so a tip surface left in `PostCard.svelte` compiles,
    paints nothing, and still ships every `post-tip-*` id. Dead is not absent.
    """
    offenders: dict[str, list[str]] = {}
    for path in _web_shared_sources():
        code = _web_code_only(path.read_text(encoding="utf-8"))
        hits = [p for p in _WEB_ELEMENT_ID_PREFIXES if p in code]
        if hits:
            offenders[path.relative_to(_REPO).as_posix()] = hits
    # Vacuity guard: the ids must exist SOMEWHERE, or this pin and the witness's
    # store-safe column are both asserting nothing.
    carriers = [
        p.relative_to(_REPO).as_posix()
        for p in _web_sources()
        if _WEB_COMPONENT_DIR in p.parents
        and any(
            x in _web_code_only(p.read_text(encoding="utf-8")) for x in _WEB_ELEMENT_ID_PREFIXES
        )
    ]
    assert carriers, (
        f"no component under {_WEB_COMPONENT_DIR} paints a payments element id at all, so "
        "this pin is asserting nothing. Either the tree moved or the ids were renamed — "
        "in both cases fix the pin, do not delete it."
    )
    assert not offenders, (
        "these SHARED web sources paint payments element ids, so a store-safe bundle "
        'ships them (dynamic-features.md § What "completely compiled away" means, '
        f"criterion 1). Move the render into {_WEB_COMPONENT_DIR.name}/ and import it "
        f"from behind `{_WEB_CONDITION}`: {offenders!r}"
    )


def test_every_payments_import_sits_behind_the_web_compile_condition():
    """A shared module may import the payments surface ONLY if it also carries the
    condition that folds the use away.

    The other half of the two pins above: they say the surface lives in its own
    modules, this one says nothing pulls those modules into the shared graph
    unconditionally. An import with no `__FAUNA_PAYMENTS__` anywhere in the file
    is an unconditional edge, and rollup then has every reason to keep the chunk
    — ids, faces and all.
    """
    offenders = []
    importers = []
    for path in _web_shared_sources():
        raw = path.read_text(encoding="utf-8")
        if not _web_payments_imports(raw):
            continue
        importers.append(path.relative_to(_REPO).as_posix())
        # The condition must appear in CODE, not merely in the comment that
        # explains it — `_web_code_only` drops the latter.
        if _WEB_CONDITION not in _web_code_only(raw):
            offenders.append(path.relative_to(_REPO).as_posix())
    assert importers, (
        "no shared SPA source imports the payments surface at all, so this pin is "
        "asserting nothing — the surface is either unreachable (a product regression) or "
        "it moved. Fix the pin, do not delete it."
    )
    assert not offenders, (
        f"these web sources import the payments surface with no `{_WEB_CONDITION}` "
        "anywhere in the file, so the import is unconditional and the chunk survives into "
        f"a store-safe bundle: {offenders!r}"
    )


def test_only_the_payments_components_import_the_payments_glue():
    """`$lib/payments` has exactly one class of importer.

    The glue module is what carries every face name, so the moment a shared
    module imports it directly the isolation is gone — the chunk joins the
    always-bundled graph and no `{#if}` anywhere can take it back out. Keeping
    the importer set to `$lib/components/payments/` is what makes the pin above
    sufficient: gate the components, and the glue travels with them.
    """
    offenders = {}
    for path in _web_shared_sources():
        glue = [s for s in _web_payments_imports(path.read_text(encoding="utf-8")) if "$lib/payments" in s]
        if glue:
            offenders[path.relative_to(_REPO).as_posix()] = glue
    assert not offenders, (
        "these shared web sources import `$lib/payments` directly. Only components under "
        f"{_WEB_COMPONENT_DIR.name}/ may — everything else must go through one of them, or "
        f"the glue chunk is pulled into every bundle: {offenders!r}"
    )


def test_the_web_store_safe_recipe_sets_the_flavor_and_is_not_staleness_gated():
    """`just web-store-safe` is the named per-platform recipe § The App-Store
    escape hatch requires, and it must build EVERY time.

    ⚠ The staleness half is web-specific and a real trap, not a hypothetical.
    `just web` is gated on `apps/fauna-web/.web-build.stamp`; a payments flavor
    flip changes no source file, so a gated store-safe recipe would find the tree
    "fresh" and hand back whichever flavor was built last — shipping the plane in
    the artifact a store submission carries, with nothing failing. (`just
    web-test` escapes the identical trap only by accident: its onboarding wasm
    chunk swap dirties `static/`.)
    """
    body = _recipe_body("web-store-safe")
    assert "FAUNA_WEB_PAYMENTS=0" in body, (
        "web-store-safe no longer sets FAUNA_WEB_PAYMENTS=0, so it builds the DEFAULT "
        "flavor under an excised name — the worst possible failure for an escape hatch"
    )
    assert "deno task build" in body, "web-store-safe no longer builds the SPA"
    assert "build-if-stale" not in body and ".web-build.stamp" not in body, (
        "web-store-safe is build-if-stale gated. A flavor flip touches no source, so the "
        "stamp reports the tree fresh and the recipe returns the other flavor's bundle"
    )


def test_the_web_app_shell_has_its_own_two_column_artifact_witness():
    """Criterion 1 lives in a shell, and web's is a JS bundle: no ffi/wasm/nest
    column can see it, and neither can the four sibling shells' — every shell
    renders the money plane from its own code.

    Three web-specific properties are asserted because each one, missing, makes
    the witness pass while the plane ships: both axes' patterns, the SPA-only
    grep scope, and the per-column snapshot that stops the second build
    overwriting the first.
    """
    body = _recipe_body("web-store-safe-check")
    assert "FAUNA_WEB_PAYMENTS=0" in body, (
        "web-store-safe-check's first column no longer builds the store-safe flavor"
    )
    # Criterion 1 (element ids) as PREFIXES, so a new §4/§5 or tip element is
    # covered the day it is added.
    for pat in _WEB_ELEMENT_ID_PREFIXES:
        assert pat in body, f"web-store-safe-check lost its '{pat}' element-id pattern"
    # Criterion 2, web's axis — see this section's header for why it is not a
    # kind-string grep.
    for face in ("paymentsProvidersSet", "paymentsClaimsMint", "paymentsKnownKinds", "tipAmount"):
        assert face in body, (
            f"web-store-safe-check lost its '{face}' face-name pattern. The SPA never "
            "spells a payments kind string, so these names are the ONLY criterion-2 "
            "evidence a web bundle can carry"
        )
    assert "_app" in body, (
        "web-store-safe-check no longer scopes its grep to the SPA's own output. "
        "SvelteKit copies `static/` into `build/`, so an unscoped grep also reads the "
        "wasm chunks — which this recipe does not rebuild per column, so column 1 would "
        "fail on a plane it never excised"
    )
    assert "snapshot" in body, (
        "web-store-safe-check no longer snapshots each column's output before the next "
        "build. Both flavors write the same `build/`, so without it the second column can "
        "pass on leftovers from the first"
    )
    assert re.search(r"rm -rf .*BUILD", body), (
        "web-store-safe-check no longer clears `build/` between columns, so a stale chunk "
        "from the previous flavor can satisfy either assertion"
    )
    assert "vacuous" in body, (
        "web-store-safe-check lost the comment explaining why the second column exists"
    )


def test_the_web_witness_is_registered_in_the_merge_gate_check():
    """The keep-green leg (§ The feature-matrix test story): a flavor nobody
    builds has rotted by the time it is needed.

    web's scope constant cannot be the `.rs`-shaped one the tui and linux columns
    ride — the web render trap is a Svelte edit and its glue is TypeScript — so
    the gate needs a scope of its own, exactly as android's did.
    """
    gate_script = _REPO / "scripts/merge-gate-check.sh"
    if not gate_script.exists():
        pytest.skip("merge-gate-check.sh is fleet-only tooling and does not ship")
    script = gate_script.read_text(encoding="utf-8")
    assert "web-store-safe-check" in script, (
        "merge-gate-check.sh no longer runs web-store-safe-check, so nothing keeps the "
        "web escape hatch green between releases"
    )
    assert "SCOPE_WEB_STORE_SAFE" in script, (
        "the web scope constant is gone. `$SCOPE_TEST_COMPILE` would miss every Svelte "
        "and TypeScript change, i.e. exactly the edits that break this gate."
    )


# ═══════════════════════════════════════════════════════════════════════════════
# windows — the SIXTH app shell, and the one whose compile condition cannot reach
# its own renders (`dynamic-features.md` § Platform-family surface excision).
#
# The family condition is a C# `#define`, positive and default-ON like every
# family whose toolchain can REMOVE a condition. But XAML has no preprocessor, so
# the define reaches the glue and NOT the markup: the WinUI XAML compiler emits
# every `AutomationProperties.AutomationId` literal into the generated `.g.cs` and
# the binary XBF, both of which ship inside `FaunaApp.dll`. Windows' shape is
# therefore BOTH halves at once, as android's is — a define for the C#, and
# csproj ITEM removal for the markup — and they fail differently: drop the define
# and the excised flavor stops compiling; drop the item removal and it compiles,
# paints nothing, and ships every element id.
#
# There is a third carrier here that no Rust shell has: the GENERATED id table. A
# C# `const string` is stored in the assembly's `Constant` metadata table whether
# or not anything references it, so an ungated table ships all 30 payments ids on
# its own. That is why `GATED_EMISSION["csharp"]` exists and why the table pin
# below is not redundant with the source pin above it.
#
# Every test here is UNPARAMETRIZED on purpose: an `[windows]` parametrization id
# would be silently deselected by conftest's app filter on a machine whose app set
# does not include windows, and these are toolchain-free source pins that must run
# everywhere (the silent-deselect trap).
# ═══════════════════════════════════════════════════════════════════════════════

_WINDOWS_APP = _REPO / "apps/fauna-windows/FaunaApp"
_WINDOWS_CONDITION = "PAYMENTS"
_WINDOWS_FLAVOR_PROPERTY = "FaunaStoreSafe"
_UI_IDS_CSHARP = _WINDOWS_APP / "FaunaApp/Generated/UiIds.cs"
# The one directory the csproj removes; every payments render must live here.
_WINDOWS_PAYMENTS_DIR = _WINDOWS_APP / "FaunaApp/Views/Payments"
# UniFFI faces the shell would call. `mod payments_client` is `#[cfg(feature =
# "payments")]` whole, so a store-safe build's generated C# has none of them.
# `FfiNostrZapSignerClient`/`FfiZapSignerEntry` are here because `zaps` is a
# SUBSET member of `payments` and rides the SAME store-safe axis (mirrors
# `_APPLE_ID_PREFIXES`'s `nostr-zap-signer-` entry).
_WINDOWS_FFI_FACES = (
    "FfiPaymentsClient", "PaymentsKnownKinds", "PaymentsWebhookUrl",
    "FfiNostrZapSignerClient", "FfiZapSignerEntry",
)


def _windows_sources():
    """Every hand-written windows source, excluding generated trees and build output."""
    for path in sorted(_WINDOWS_APP.rglob("*")):
        if path.suffix not in (".cs", ".xaml"):
            continue
        parts = set(path.parts)
        if {"bin", "obj", "Generated"} & parts:
            continue
        yield path


def _strip_regions(text: str, condition: str) -> str:
    """`text` with every `#if <condition> … #endif` region removed.

    A stack of EVERY `#if`, not a counter of the named one, so a region of the
    other member's condition nested inside (or around) this one closes on its own
    `#endif` rather than this one's. An `#else`/`#elif` inside a named region
    starts its not-`condition` branch, which stays. The regions do not nest in this
    codebase today, but a future nested one must never make this under-strip:
    that would fail a correctly-gated file and teach the next session to delete
    the pin.
    """
    out: list[str] = []
    stack: list[bool] = []   # per open `#if`: is its current branch the named one?
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("#if"):
            stack.append(stripped.startswith(f"#if {condition}"))
            continue
        if stripped.startswith(("#else", "#elif")) and stack:
            stack[-1] = False
            continue
        if stripped.startswith("#endif") and stack:
            stack.pop()
            continue
        if not any(stack):
            out.append(line)
    return "\n".join(out)


def _strip_payments_regions(text: str) -> str:
    """`text` with every `#if PAYMENTS … #endif` region removed."""
    return _strip_regions(text, _WINDOWS_CONDITION)


def test_the_windows_ffi_has_a_store_safe_flavor():
    """The Rust half. An excised flavor is the FFI built without the feature PLUS
    the shell built with the condition off, and the shell half alone is worthless:
    the generated C# face is a pure function of the FFI feature set, so it is the
    FFI flavor that turns a forgotten `#if PAYMENTS` into a compile error.

    `store-safe` must be spelled as the COMPLEMENT feature, never as a hand-listed
    excision set — one list, so a future default-on client surface is added to
    `store-safe` once in fauna-ffi and is in this flavor everywhere.
    """
    assert '_windows-ffi-flavor "{{profile}}" "store-safe' in _recipe_body(
        "windows-ffi-store-safe"
    ), "windows-ffi-store-safe no longer asks _windows-ffi-flavor for the store-safe flavor"

    body = _recipe_body("_windows-ffi-flavor")
    assert "--no-default-features --features store-safe" in body, (
        "_windows-ffi-flavor no longer maps the store-safe axis onto the complement "
        "feature. Passing it as an ordinary `--features store-safe` would ADD the "
        "complement to the default set and excise nothing at all"
    )


# The only extras a shell's store-safe FFI flavor may add to the complement,
# each with the reason it is there. `offline-share` is the `p2p-share` member's
# ceremony half: it left fauna-ffi's `store-safe` on 2026-09-28, and android and
# windows carried it back by name until each shell's own leg gated its ceremony
# (windows' `P2P_SHARE` define, android's `P2P_SHARE` build-config field and
# `p2pShare`/`noP2pShare` glue twin, both 2026-09-29); apple carried none. Every
# entry is now the EMPTY set: kept rather than dropped, so a carry that creeps
# back goes red here.
_STORE_SAFE_INTERIM_CARRY = {
    "android-ffi-store-safe": set(),
    "android-store-safe-check": set(),
    "windows-ffi-store-safe": set(),
}


@pytest.mark.parametrize("recipe", sorted(_STORE_SAFE_INTERIM_CARRY))
def test_a_shell_store_safe_ffi_flavor_carries_only_its_declared_interim_extras(recipe):
    """`store-safe,<extra>` re-admits a feature the complement excludes, so an
    extra that grows unnoticed turns a store-safe build back into a partial
    default. The carry is pinned to exactly what is declared: adding one is a
    visible edit here, and removing one without editing here goes red, so the
    declaration can't go stale.
    """
    body = _recipe_body(recipe)
    carried: set[str] = set()
    for m in re.finditer(r"ffi-flavor\b[^\n]*?\bstore-safe((?:,[a-z0-9-]+)*)", body):
        carried |= {f for f in m.group(1).split(",") if f}
    assert carried == _STORE_SAFE_INTERIM_CARRY[recipe], (
        f"{recipe}'s store-safe FFI flavor carries {sorted(carried)!r} beyond the "
        f"complement; declared: {sorted(_STORE_SAFE_INTERIM_CARRY[recipe])!r}"
    )


def test_the_windows_flavor_marker_covers_the_feature_set():
    """All three FFI flavors share ONE staging slot (the WinUI project consumes one
    fixed pair of paths), so the marker is the only thing standing between a warm
    tree and a wrong-flavor false-green — and a store-safe artifact scan reading a
    payments-carrying dll is exactly the false green this whole family exists to
    prevent. A profile-only marker reads "fresh" across a flavor switch at the same
    profile.
    """
    body = _recipe_body("_windows-ffi-flavor")
    assert '"$RID:$PROFILE:$FEATURES" > "$FLAVOR_MARKER"' in body, (
        "the .ffi-flavor marker no longer records RID:PROFILE:FEATURES, so a "
        "store-safe build off a warm release tree can be served the "
        "payments-carrying dll (RID joined the marker 2026-08-24 with the x64 "
        "cross leg — same collision class the FEATURES axis already guards, one "
        "axis further)"
    )
    # The bindgen's own recipe carries the production assertion's predicate.
    bindgen = _recipe_body("_windows-ffi-bindgen")
    assert "test-helpers" in bindgen and "FLAVOR_FLAG=--production-tree" in bindgen, (
        "_windows-ffi-bindgen no longer decides its production assertion by whether the "
        "flavor carries test-helpers. store-safe is a non-empty PRODUCTION flavor, so an "
        "emptiness test would hand it --test-tree and stop asserting a seam-free face for "
        "the artifact that most needs it"
    )


def test_the_windows_shell_declares_its_compile_condition_positively():
    """windows' switch is positive and default-ON — the rule for every family whose
    toolchain can remove a condition (`dynamic-features.md` § Platform-family
    surface excision). Apple's negative polarity is the documented exception,
    forced by SwiftPM's inability to un-define; MSBuild has no such limit, so an
    inversion here would be a re-ruling, never a build detail.

    All three projects must carry it: MSBuild does not propagate a DefineConstants
    across a ProjectReference, so a define set only in FaunaApp would leave
    FaunaApp.Core compiling its whole payments glue in the excised flavor.
    """
    for name in ("FaunaApp/FaunaApp.csproj", "FaunaApp.Core/FaunaApp.Core.csproj",
                 "FaunaApp.Tests/FaunaApp.Tests.csproj"):
        text = (_WINDOWS_APP / name).read_text(encoding="utf-8")
        assert f"'$({_WINDOWS_FLAVOR_PROPERTY})'!='true'" in text, (
            f"{name} no longer guards the payments define on {_WINDOWS_FLAVOR_PROPERTY}, "
            "so its half of the shell cannot be excised"
        )
        assert f"$(DefineConstants);{_WINDOWS_CONDITION}" in text, (
            f"{name} no longer appends the {_WINDOWS_CONDITION} define"
        )


def test_the_windows_payments_markup_is_confined_to_one_removable_item():
    """The half a define cannot do. XAML has no preprocessor, so removing the ITEM
    is the only construct that removes an AutomationId literal from the artifact —
    the C# twin of web's isolated-module pattern and android's source-set split.

    `<Compile Remove>` must accompany `<Page Remove>`: an orphaned `.xaml.cs`
    referencing a removed page's generated partial does not compile, so dropping it
    would turn the excised flavor un-buildable rather than clean.
    """
    text = (_WINDOWS_APP / "FaunaApp/FaunaApp.csproj").read_text(encoding="utf-8")
    assert f"'$({_WINDOWS_FLAVOR_PROPERTY})'=='true'" in text, (
        "FaunaApp.csproj has no store-safe ItemGroup at all, so the payments markup "
        "ships in every flavor however the define is set"
    )
    for item in ("Page Remove", "Compile Remove"):
        assert item in text and "Views\\Payments" in text, (
            f"FaunaApp.csproj no longer removes Views\\Payments\\** via <{item}>. Without "
            "it the excised build still carries every element id the §4/§5 renders paint"
        )


def test_no_shared_windows_source_paints_a_payments_element_id():
    """THE RENDER TRAP, caught without MSBuild — the defect class every app-shell
    column exists for, and the one no compile gate can see.

    A payments render placed outside `Views/Payments/` compiles clean in BOTH
    flavors: its data comes from an ungated inert record or simply never arrives,
    so it paints nothing — and still ships every `AutomationId` it would have
    painted. The artifact witness catches it, but only when someone runs a two-app-
    build recipe; this catches it on any machine, in a second.
    """
    # `zaps` is a SUBSET member of `payments` and rides the SAME store-safe axis
    # (dynamic-features.md § Which element IDs belong to a gated feature) — union
    # both so a zap-signer render outside Views/Payments/ is caught exactly like
    # a payments one.
    prefixes = tuple(_gated_catalog()["payments"]["id_prefixes"]) + tuple(
        _gated_catalog()["zaps"]["id_prefixes"]
    )
    assert prefixes, "ui.yaml declares no payments/zaps id prefixes, so this pin asserts nothing"

    offenders: list[str] = []
    for path in _windows_sources():
        if _WINDOWS_PAYMENTS_DIR in path.parents:
            continue
        text = path.read_text(encoding="utf-8")
        if path.suffix == ".cs":
            # C# CAN carry an id conditionally, so only ungated CODE counts. XML doc
            # comments never reach IL — unlike a Rust docstring, which UniFFI puts in
            # the artifact's metadata, which is why criterion 1's "prose included"
            # rule bites there and not here.
            text = _strip_payments_regions(text)
            text = re.sub(r"//.*", "", text)
        hits = sorted(p for p in prefixes if p in text)
        if hits:
            offenders.append(f"{path.relative_to(_REPO).as_posix()} -> {hits}")
    assert not offenders, (
        "these windows sources paint a payments element id where a store-safe build "
        "still carries it. For a .xaml that means ANYWHERE outside Views/Payments/ — the "
        "one directory the csproj removes, and markup has no `#if` to hide behind. For a "
        ".cs it means outside a `#if PAYMENTS` region (doc comments are exempt: they "
        "never reach IL). Either way the excised artifact ships the id "
        "(dynamic-features.md § What \"completely compiled away\" means, criterion 1): "
        f"{offenders!r}"
    )


def test_no_shared_windows_source_names_a_payments_ffi_symbol():
    """The glue half's source pin. A store-safe build's generated C# face has no
    `FfiPaymentsClient` at all, so a call site outside `#if PAYMENTS` does not
    compile — which is the property, but only once someone builds that flavor. This
    says the same thing with no toolchain, so the excised build's compilability is
    a fact about the tree rather than about the last time anyone ran the recipe.
    """
    offenders: list[str] = []
    for path in _windows_sources():
        if _WINDOWS_PAYMENTS_DIR in path.parents:
            continue
        if path.suffix != ".cs":
            continue   # XAML has no `#if`; the element-id pin above covers it
        ungated = _strip_payments_regions(path.read_text(encoding="utf-8"))
        # Comments are not code, but they are also not worth a C# parser here: the
        # faces are distinctive identifiers, so strip the two comment forms that
        # legitimately name them in prose.
        ungated = re.sub(r"//.*", "", ungated)
        hits = sorted(f for f in _WINDOWS_FFI_FACES if f in ungated)
        if hits:
            offenders.append(f"{path.relative_to(_REPO).as_posix()} -> {hits}")
    assert not offenders, (
        "these windows sources name a payments UniFFI face outside a "
        f"`#if {_WINDOWS_CONDITION}` region, so the store-safe flavor does not compile "
        f"(its generated face has no such type): {offenders!r}"
    )


def test_the_generated_windows_id_table_puts_every_payments_id_behind_the_condition():
    """The third carrier, and the one unique to this shell among the non-Rust three.

    Rust needs no gated table because an unreferenced `pub const` emits no bytes.
    C# is the opposite: a `const string` is stored in the assembly's `Constant`
    metadata table with its UTF-16 value whether or not anything reads it. So an
    ungated `UiIds.cs` ships all 30 payments ids in the store-safe artifact by
    itself, with no render involved at all — which is precisely why the emitter
    grew a `csharp` row rather than the "none needed" the Rust row carries.
    """
    text = _UI_IDS_CSHARP.read_text(encoding="utf-8")

    excised: list[bool] = []
    depth = 0
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(f"#if {_WINDOWS_CONDITION}"):
            depth += 1
            excised.append(True)
            continue
        if depth and stripped.startswith("#endif"):
            depth -= 1
            excised.append(True)
            continue
        excised.append(depth > 0)

    # `zaps` is a SUBSET member of `payments` and rides the SAME store-safe axis
    # — union both, same reasoning as the element-id source pin above.
    gated = sorted(_gated_ids("payments") | _gated_ids("zaps"))
    assert gated, (
        "ui.yaml's `gated_features.payments`/`zaps` resolved to no ids, so this "
        "pin is asserting nothing — fix the catalog, do not delete the pin."
    )

    leaked: list[str] = []
    seen: set[str] = set()
    for line, is_excised in zip(text.splitlines(), excised):
        for element_id in gated:
            if f'"{element_id}"' in line:
                seen.add(element_id)
                if not is_excised:
                    leaked.append(element_id)
    missing = [i for i in gated if i not in seen]
    assert not missing, (
        "these payments ids are declared in ui.yaml's `gated_features:` block but "
        f"appear nowhere in {_UI_IDS_CSHARP.name}, so the table and the catalog have "
        f"drifted — regenerate with `just ui-ids-generate`: {missing!r}"
    )
    assert not leaked, (
        f"these payments element ids sit OUTSIDE the `#if {_WINDOWS_CONDITION}` region "
        f"of {_UI_IDS_CSHARP.name}, so a store-safe windows build carries them in "
        "assembly metadata even though nothing references them "
        "(dynamic-features.md § Which element IDs belong to a gated feature): "
        f"{sorted(set(leaked))!r}"
    )


# ── windows' `p2p-share` leg — its own `P2P_SHARE` define ─────────────────────
# `p2p-share` is a registry member of its own and NOT a subset of `payments`, so
# windows gives it its own positive define (one condition per member — the swift
# row's `FAUNA_EXCISE_P2P_SHARE`, inverted as the family rule wants). Windows
# renders only the member's ceremony half (the offline co-present share on the
# Folders page), in Views/P2pShare/; the plane half's two prefixes ride the
# generated table alone. Same four carriers as payments, same pins.
_WINDOWS_P2P_CONDITION = "P2P_SHARE"
_WINDOWS_P2P_DIR = _WINDOWS_APP / "FaunaApp/Views/P2pShare"
# The ceremony's UniFFI face. `mod offline_share` (fauna-ffi) and
# `group_ceremony_view` (fauna-client-capabilities) are feature-gated whole, so
# a store-safe build's generated C# has none of these. Word-bounded: the custody
# and recovery ceremonies own unrelated `*CeremonyStatus`-shaped names.
_WINDOWS_P2P_FACES = (
    "FfiCeremonySeat", "CeremonyStatus", "OfflineSharePanel", "FfiGroupShareViews",
    "FfiPendingGroupShare", "FfiGroupScope", "OfflineShareView", "OfflineShareGates",
)


def test_the_windows_shell_declares_its_p2p_share_condition_positively():
    """All three projects, for the payments define's reason: MSBuild does not
    propagate DefineConstants across a ProjectReference, and Core carries the
    ceremony doors while Tests carries the mock's ceremony arm."""
    for name in ("FaunaApp/FaunaApp.csproj", "FaunaApp.Core/FaunaApp.Core.csproj",
                 "FaunaApp.Tests/FaunaApp.Tests.csproj"):
        text = (_WINDOWS_APP / name).read_text(encoding="utf-8")
        assert f"$(DefineConstants);{_WINDOWS_P2P_CONDITION}" in text, (
            f"{name} no longer appends the {_WINDOWS_P2P_CONDITION} define"
        )


def test_the_windows_p2p_share_markup_is_confined_to_one_removable_item():
    """The half a define cannot do: XAML has no preprocessor, so the store-safe
    ItemGroup must remove Views\\P2pShare\\ from both Page and Compile."""
    text = (_WINDOWS_APP / "FaunaApp/FaunaApp.csproj").read_text(encoding="utf-8")
    for item in ("Page Remove", "Compile Remove"):
        assert re.search(rf'<{item}="Views\\P2pShare\\\*\*', text), (
            f"FaunaApp.csproj no longer removes Views\\P2pShare\\** via <{item}>, so the "
            "excised build still carries every ceremony element id"
        )


def test_no_shared_windows_source_paints_a_p2p_share_element_id():
    """THE RENDER TRAP for this member: a ceremony render outside
    `Views/P2pShare/` compiles clean in both flavors and ships its ids."""
    prefixes = tuple(_gated_catalog()["p2p-share"]["id_prefixes"])
    assert prefixes, "ui.yaml declares no p2p-share id prefixes, so this pin asserts nothing"
    # Windows spells most ids through the generated table (`{x:Bind ids:Ids.
    # OfflineShareButton}`, `Ids.FolderRow`), not as literals, so a literal-only
    # scan would miss exactly the render this pin exists for. Match the table's
    # PascalCase spelling too: `offline-share-` -> `Ids.OfflineShare`.
    spelled = tuple(
        "Ids." + "".join(w.capitalize() for w in p.strip("-").split("-")) for p in prefixes
    )

    offenders: list[str] = []
    for path in _windows_sources():
        if _WINDOWS_P2P_DIR in path.parents:
            continue
        text = path.read_text(encoding="utf-8")
        if path.suffix == ".cs":
            text = _strip_regions(text, _WINDOWS_P2P_CONDITION)
            text = re.sub(r"//.*", "", text)
        else:
            text = re.sub(r"<!--.*?-->", "", text, flags=re.S)
        hits = sorted(p for p in prefixes + spelled if p in text)
        if hits:
            offenders.append(f"{path.relative_to(_REPO).as_posix()} -> {hits}")
    assert not offenders, (
        "these windows sources paint a p2p-share element id where a store-safe build "
        "still carries it — a .xaml outside Views/P2pShare/, or a .cs line outside a "
        f"`#if {_WINDOWS_P2P_CONDITION}` region: {offenders!r}"
    )


def test_no_shared_windows_source_names_a_ceremony_ffi_symbol():
    """The glue half's source pin: a ceremony face outside `#if P2P_SHARE` does
    not compile in the store-safe flavor. This says so with no toolchain."""
    offenders: list[str] = []
    for path in _windows_sources():
        if _WINDOWS_P2P_DIR in path.parents or path.suffix != ".cs":
            continue
        ungated = _strip_regions(path.read_text(encoding="utf-8"), _WINDOWS_P2P_CONDITION)
        ungated = re.sub(r"//.*", "", ungated)
        hits = sorted(f for f in _WINDOWS_P2P_FACES if re.search(rf"\b{f}\b", ungated))
        if hits:
            offenders.append(f"{path.relative_to(_REPO).as_posix()} -> {hits}")
    assert not offenders, (
        "these windows sources name a ceremony UniFFI face outside a "
        f"`#if {_WINDOWS_P2P_CONDITION}` region, so the store-safe flavor does not "
        f"compile (its generated face has no such type): {offenders!r}"
    )


def test_the_generated_windows_id_table_puts_every_p2p_share_id_behind_its_condition():
    """The table carrier: a C# `const string` ships in assembly metadata whether
    or not anything reads it, so every p2p-share id must sit in the
    `#if P2P_SHARE` half — the plane's ids included, which windows never paints."""
    text = _UI_IDS_CSHARP.read_text(encoding="utf-8")
    gated = sorted(_gated_ids("p2p-share"))
    assert gated, "ui.yaml's `gated_features.p2p-share` resolved to no ids"
    ungated = _strip_regions(text, _WINDOWS_P2P_CONDITION)
    missing = [i for i in gated if f'"{i}"' not in text]
    leaked = [i for i in gated if f'"{i}"' in ungated]
    assert not missing, (
        f"these p2p-share ids appear nowhere in {_UI_IDS_CSHARP.name} — regenerate with "
        f"`just ui-ids-generate`: {missing!r}"
    )
    assert not leaked, (
        f"these p2p-share element ids sit OUTSIDE the `#if {_WINDOWS_P2P_CONDITION}` "
        f"region of {_UI_IDS_CSHARP.name}: {leaked!r}"
    )


def test_the_windows_witness_watches_the_p2p_share_member():
    """All four catalog prefixes in the element-id scan, the ceremony faces on the
    managed assembly, and the render directory's .xbf in both columns."""
    body = _recipe_body("_windows-store-safe-check-impl")
    for pat in _gated_catalog()["p2p-share"]["id_prefixes"]:
        assert f"'{pat}'" in body, f"windows-store-safe-check lost its '{pat}' pattern"
    for face in ("FfiCeremonySeat", "uniffi_fauna_ffi_fn_func_offline_share_"):
        assert f"'{face}'" in body, f"windows-store-safe-check lost its '{face}' face pattern"
    assert "Views/P2pShare/" in body, (
        "windows-store-safe-check no longer checks the ceremony render's .xbf, so the "
        "item removal can silently stop matching"
    )


def test_the_windows_store_safe_app_recipe_excises_both_halves():
    """`just windows-store-safe` is the named per-platform recipe § The App-Store
    escape hatch requires, and it must set BOTH halves: half an excision is a build
    that either still ships the plane or does not compile.

    ⚠ It must also build EVERY time. `windows-release` right above it is
    build-if-stale gated on `target/windows-release.stamp`; a flavor flip changes
    no source file, so a gated store-safe recipe would find the tree fresh and hand
    back whichever flavor was built last — the plane shipping in the artifact a
    store submission carries, with nothing failing. That is web's finding (3), and
    it applies here for the identical reason.
    """
    body = _recipe_body("windows-store-safe")
    assert f"p:{_WINDOWS_FLAVOR_PROPERTY}=true" in body, (
        "windows-store-safe no longer passes the store-safe flavor property, so it "
        "builds the DEFAULT shell under an excised name"
    )
    assert "Configuration=Release" in body, (
        "windows-store-safe no longer builds Release. A Debug build compiles the "
        "`#if DEBUG` automation surface, whose *ForTest calls resolve only against the "
        "test-helpers FFI flavor a shipping artifact must never carry (apple's "
        "finding (1); e2e-automation-surface-gating.md point 15)"
    )
    assert "build-if-stale" not in body and "windows-release.stamp" not in body, (
        "windows-store-safe is build-if-stale gated. A flavor flip touches no source, so "
        "the stamp reports the tree fresh and the recipe returns the other flavor's app"
    )
    # The FFI half rides the recipe's DEPENDENCY line, not its body — the same
    # shape apple's recipes use, so `_recipe_header` is what reads it.
    assert "windows-ffi-store-safe" in _recipe_header("windows-store-safe"), (
        "windows-store-safe no longer takes the store-safe FFI as a prerequisite, so it "
        "links a payments-carrying fauna_ffi.dll and every `#if PAYMENTS` it forgot stays "
        "compilable — the compile-error property is gone"
    )


def test_the_windows_app_shell_has_its_own_two_column_artifact_witness():
    """Criterion 1 lives in a shell, and no library/server column can see this one:
    the five sibling shells render the money plane from unrelated code.

    Three windows-specific properties are asserted, because each one missing makes
    the witness pass while the plane ships: the UTF-16-aware scan, the split of the
    two criteria across the managed and native artifacts, and the per-column wipe.
    """
    body = _recipe_body("_windows-store-safe-check-impl")
    assert "just windows-store-safe" in body, (
        "the first column no longer builds the store-safe flavor"
    )
    assert "just windows-release" in body, (
        "the second column no longer builds the DEFAULT flavor, so every absence "
        "assertion is indistinguishable from a grep that matches nothing"
    )
    # `zaps` is a SUBSET member of `payments` and rides the SAME store-safe axis
    # — union both, same reasoning as the element-id source pin above.
    for pat in _gated_catalog()["payments"]["id_prefixes"] + _gated_catalog()["zaps"]["id_prefixes"]:
        assert pat in body, (
            f"windows-store-safe-check lost its '{pat}' element-id pattern. They are "
            "PREFIXES so a new §4/§5/zap-signer element is covered the day it is added"
        )
    for face in _WINDOWS_FFI_FACES:
        assert face in body, (
            f"windows-store-safe-check lost its '{face}' face-name pattern. Windows C# "
            "spells no payments kind string anywhere, so on the MANAGED artifact these "
            "names are the only criterion-2-class evidence there is"
        )
    assert "fauna_ffi.dll" in body and "fauna\\.payments\\." in body, (
        "windows-store-safe-check no longer scans the packaged native cdylib for kind "
        "strings. Asserting them on the managed assembly instead would be vacuous — the "
        "only C# occurrences are XML doc comments, which never reach IL"
    )
    assert "strings-utf16.py" in body, (
        "windows-store-safe-check no longer scans through the UTF-16-aware scanner. A "
        ".NET assembly stores const values and ldstr literals as UTF-16LE and the "
        "`strings` on Windows is llvm-strings, which has no encoding flag — so a plain "
        "`strings -a | grep` reports 0 for every pattern in BOTH columns"
    )
    assert ".xbf" in body, (
        "windows-store-safe-check no longer scans the compiled-XAML .xbf files. The WinUI "
        "XAML compiler emits them BESIDE FaunaApp.dll, not inside it, so every "
        "AutomationId literal lives there — an assembly-only scan measures the generated "
        "const table and NOTHING about the renders, which is the half the csproj "
        "item-removal is responsible for. Measured 2026-08-24: that scope passed green "
        "with an ungated post-tip-count in a page outside Views\\Payments\\"
    )
    assert re.search(r"rm -rf .*bin.*obj", body), (
        "windows-store-safe-check no longer wipes bin/ + obj/ between columns. Both "
        "flavors write the same output directory and a flavor flip touches no source, "
        "so the second column can pass on the first column's assembly"
    )
    assert "vacuous" in body, (
        "windows-store-safe-check lost the comment explaining why the second column exists"
    )


def test_the_windows_witness_scan_actually_sees_a_utf16_element_id():
    """The scanner is a MECHANISM this witness rests on, so it gets a pin of its own
    rather than an argument.

    Everything else about windows' column is a claim about the recipe's text; this
    is the one claim about behaviour, and it is the claim the whole column would
    fail silently without: if the scan cannot see UTF-16, column 1 finds nothing
    (reads as a clean excision) and column 2 finds nothing (reads as a renamed id).
    Red-verify by pointing the recipe at `strings` — both columns go wrong at once.
    """
    import subprocess
    import sys
    import tempfile

    element_id = "subscription-provider-section"
    with tempfile.TemporaryDirectory() as tmp:
        probe = Path(tmp) / "probe.bin"
        # An odd byte first, so the UTF-16 run is not 2-byte aligned — a real
        # assembly's #US heap gives no alignment guarantee either.
        probe.write_bytes(b"\x01" + element_id.encode("utf-16-le") + b"\x00\xff")
        out = subprocess.run(
            [sys.executable, str(_REPO / "scripts/strings-utf16.py"), str(probe)],
            capture_output=True, text=True, check=True,
        ).stdout
    assert element_id in out, (
        "scripts/strings-utf16.py did not find a UTF-16LE element id. Every absence "
        "assertion in windows-store-safe-check would read as a clean excision and every "
        "presence assertion as a renamed id — the witness would be measuring nothing"
    )
