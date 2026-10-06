"""Convention 15's native-FFI half: the production build recipes must not
compile the e2e automation seams into the artifacts they ship.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention —
convention 15 (== point 15, split out of testing.md 2026-08-17) — the
automation surface is compiled out of release artifacts. Rule (b) of its
shared-Rust bullet keys any UniFFI export of a seam on the FEATURE ALONE, never
the profile, so "the production flavor" is exactly "the build that does not pass
`test-helpers`" and the generated Kotlin/Swift/C# face is a pure function of the
feature set.

Two independent failure modes this pins, both of which have actually happened:

  1. A dep line naming `test-helpers` turns it on unconditionally. That was
     `fauna-ffi`'s state until 2026-08-01, and it rode 12 seams into five
     release artifacts. Fixed then; asserted here so it cannot come back.
  2. The *recipe* naming `test-helpers` puts the seams back even with the crate
     compliant. That was the state of all three native recipes until the android
     split (2026-08-01), followed by windows (2026-08-01) and apple
     (2026-08-02). All three legs are closed and asserted below.

Pure text analysis of the justfile and the crate manifest — no build, no driver.
The artifact-level witness (0 `*ForTest` symbols in the generated production
bindings) runs inside each production `_*-ffi-bindgen` on every production build,
deriving its seam set from the tree it just generated rather than a list anyone
has to maintain — the shape `scripts/check-wasm-seam-exclusion.py` established.
"""

import re
from pathlib import Path

import pytest

from helpers.manifest_seams import shipping_dep_lines as _shipping_dep_lines

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"


def _recipe_body(name: str) -> str:
    """The indented body of justfile recipe `name`, joined into one string.

    A body whose LAST non-blank line is a bare delegation call — `[{{slot_build}}]
    just _<name>`, nothing else on that line — resolves through it: the slot-lock
    lease split moved the content these pins look for into the
    delegate's own body, so reading only the wrapper's literal text would see
    none of it (`_apple-ffi-flavor`'s split is the case that needs this —
    its cache-lookup fast path stays here, unslotted, ahead of the delegate
    call). Any preceding lines are kept; the delegate's body is appended after
    them. Ported from `test_payments_excision_spine.py::_recipe_body` — small helpers are still duplicated per
    this suite's no-cross-import-between-test-files convention.
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


def _cfg_attrs_above(lines: list[str], i: int) -> str:
    """The ATTRIBUTE text immediately above declaration line `i`, comments removed.

    Convention 15's gate lives in a `#[cfg(...)]`, never in prose. Searching the
    raw preceding lines for the feature name cannot tell the two apart, so a
    platform-only `#[cfg(not(target_arch = "wasm32"))]` sitting under a doc
    comment that merely *mentions* `feature = "e2e-agent"` satisfied both needles
    and passed — the exact shape these pins exist to catch, measured green
    on `conv_push_source` and on linux's `e2e_mode_enabled`. Walk up
    the contiguous prelude and keep only the attribute lines, so the needle must
    appear inside a real cfg. Multi-line `#[cfg(all(...))]` bodies are kept: the
    walk collects lines, not balanced attributes.
    """
    out: list[str] = []
    j = i - 1
    while j >= 0:
        stripped = lines[j].strip()
        if not stripped or stripped == "}":
            break
        if not stripped.startswith("//"):
            out.append(stripped)
        j -= 1
    return "\n".join(reversed(out))


def _recipe_header(name: str) -> str:
    """The recipe's own `name deps...:` line, i.e. its dependency list."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*$", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(0)


# ── the crate half (rule (b)): no dep line may name the feature ──────────────
# `_shipping_dep_lines` is imported from `helpers.manifest_seams` above — see
# that module's docstring for why (multi-line dep entries used to defeat this
# scan even in the one file that names the crate directly).


def test_fauna_ffi_never_names_test_helpers_on_a_dependency_line():
    """A consumer forwards `test-helpers` from its own feature; it never names it
    on a dep line, where cargo turns it on unconditionally — surviving even
    `--no-default-features`, which is how the seams reached the release Go
    binding (e2e-automation-surface-gating.md § convention 15, rule (b))."""
    manifest = (_REPO / "libs" / "fauna-ffi" / "Cargo.toml").read_text(encoding="utf-8")
    offenders = [line for line in _shipping_dep_lines(manifest) if "test-helpers" in line]
    assert not offenders, (
        "libs/fauna-ffi/Cargo.toml names `test-helpers` on a dependency line, which "
        "turns the seams on unconditionally in every artifact regardless of what the "
        "build recipe asks for (e2e-automation-surface-gating.md convention 15 rule (b)). Forward it from "
        "fauna-ffi's own `test-helpers` feature instead. Offending line(s): " + repr(offenders)
    )


def _feature_closure(features: dict, root: str) -> set[str]:
    """Every own-crate feature `root` turns on, transitively (`dep:` and
    `crate/feature` entries are not own features and end the walk)."""
    seen: set[str] = set()
    stack = [root]
    while stack:
        name = stack.pop()
        if name in seen or name not in features:
            continue
        seen.add(name)
        stack.extend(f for f in features[name] if "/" not in f and not f.startswith("dep:"))
    return seen


def test_the_harness_seed_exports_ship_in_no_app_flavor():
    """The Python harness's networked seed exports (`libs/fauna-ffi/src/cabi/
    harness.rs` — a set created and change records signed as any actor whose
    seed the caller holds) are test surface: fauna-ffi's `e2e-harness` feature
    gates the module behind a real cfg, no shipping feature set (`default`,
    `store-safe`) reaches it, and the only build recipe that turns it on is the
    harness's own `e2e-ffi` (e2e-automation-surface-gating.md convention 15).
    The one other recipe that may name the feature is `e2e-ffi-flavor-check`,
    the heavy gate that type-checks that same flavor: it must stay a `cargo
    check` (no artifact) and must name exactly the flavor `e2e-ffi` builds."""
    import tomllib

    manifest_path = _REPO / "libs" / "fauna-ffi" / "Cargo.toml"
    features = tomllib.loads(manifest_path.read_text(encoding="utf-8"))["features"]
    assert "e2e-harness" in features, "fauna-ffi lost its `e2e-harness` feature"
    for shipping in ("default", "store-safe"):
        assert "e2e-harness" not in _feature_closure(features, shipping), (
            f"fauna-ffi's `{shipping}` feature set reaches `e2e-harness`, so every app "
            "artifact would ship the harness's networked seed exports"
        )

    # The whole C ABI module is test surface: every `extern "C"` symbol in it is
    # exported from a shipped artifact unless the module declaration itself is
    # gated (fix dccxxix / dcccxli). Feature alone — never `debug_assertions`,
    # because `e2e-ffi` builds the release profile (convention 15).
    lib = (_REPO / "libs" / "fauna-ffi" / "src" / "lib.rs").read_text(encoding="utf-8").splitlines()
    mod_decl = [i for i, line in enumerate(lib) if line.strip() == "pub mod cabi;"]
    assert len(mod_decl) == 1, "lib.rs must declare `pub mod cabi;` exactly once"
    assert _cfg_attrs_above(lib, mod_decl[0]).replace(" ", "") == '#[cfg(feature="e2e-harness")]', (
        "lib.rs's `pub mod cabi;` must sit behind exactly `#[cfg(feature = \"e2e-harness\")]`: "
        "ungated, every shipped fauna-ffi exports the C ABI's `fauna_*` symbols"
    )

    cabi = (_REPO / "libs" / "fauna-ffi" / "src" / "cabi.rs").read_text(encoding="utf-8").splitlines()
    decl = [i for i, line in enumerate(cabi) if line.strip() == "mod harness;"]
    assert len(decl) == 1, "cabi.rs must declare `mod harness;` exactly once"
    assert 'feature = "e2e-harness"' in _cfg_attrs_above(cabi, decl[0]), (
        "cabi.rs's `mod harness;` must sit behind `#[cfg(feature = \"e2e-harness\")]`"
    )

    text = _JUSTFILE.read_text(encoding="utf-8")
    naming = [line.strip() for line in text.splitlines()
              if "e2e-harness" in line and "cargo" in line]
    e2e_ffi = _recipe_body("e2e-ffi")
    check = _recipe_body("e2e-ffi-flavor-check")
    assert naming and all(line in e2e_ffi or line in check for line in naming), (
        "only the `e2e-ffi` recipe may build fauna-ffi with `e2e-harness` (and only "
        "`e2e-ffi-flavor-check` may type-check it); "
        f"cargo lines naming it: {naming!r}"
    )

    def flavor(body):
        """The `-p … --no-default-features --features …` selection of the cargo
        line naming the feature in `body`, and that line's cargo subcommand."""
        found = [
            (m.group(1), m.group(2))
            for m in re.finditer(
                r"cargo (\w+)[^\n]*?(-p fauna-ffi --no-default-features --features [\w,-]+)", body
            )
            if "e2e-harness" in m.group(2)
        ]
        assert len(found) == 1, f"expected one cargo line naming `e2e-harness`, found {found!r}"
        return found[0]

    built, checked = flavor(e2e_ffi), flavor(check)
    assert built[0] == "build" and checked[0] == "check", (
        "`e2e-ffi` builds the harness library and `e2e-ffi-flavor-check` only type-checks "
        f"it — a second BUILD recipe would be a second source of the test surface: {built!r}, "
        f"{checked!r}"
    )
    assert built[1] == checked[1], (
        "`e2e-ffi-flavor-check` must type-check exactly the flavor `e2e-ffi` builds, or the "
        f"gate goes green over a library that no longer builds: {built[1]!r} vs {checked[1]!r}"
    )


# ── the recipe half: android is split, apple/windows are the open legs ───────


def test_android_production_ffi_recipe_does_not_build_the_seams():
    """`just android-ffi` builds what `android-release` packages."""
    chain = _recipe_body("android-ffi") + _recipe_body("_android-ffi-flavor")
    assert '_android-ffi-flavor release ""' in _recipe_body("android-ffi"), (
        "android-ffi must delegate to the shared impl with the PRODUCTION feature "
        "set (no extra features, no `test-helpers`) and stage into the `release` "
        "source set"
    )
    # The shared impl parameterises the features, so `test-helpers` must appear
    # nowhere in the chain as a literal cargo argument.
    build_lines = [ln for ln in chain.splitlines() if "cargo build" in ln]
    assert build_lines, "no cargo build line found in the android-ffi chain"
    for line in build_lines:
        assert "test-helpers" not in line, (
            "an android-ffi cargo build hard-codes `test-helpers`, so the seams ship "
            "in the release APK's libfauna_ffi.so no matter which flavor was asked "
            f"for (e2e-automation-surface-gating.md convention 15). Offending line: {line.strip()}"
        )


def test_android_bindgen_runs_the_flavor_diff_witness():
    """`_android-ffi-bindgen` must call the flavor-diff seam witness
    (`scripts/check-ffi-seam-diff.py`, e2e-conventions.md point 15 build-out,
    2026-08-13) beside its `*ForTest` suffix grep. The suffix grep sees only
    conventionally-NAMED seams; the script also pins the non-test-named floor
    (setProviderBaseUrls, resolvedNestDialUrl, callMachineMethod*,
    FfiChildAgentSpawner, …) — a dead cfg gate puts a seam in BOTH flavors'
    faces, which an absence-only grep can never see. Dropping this call from
    the recipe would silently revert the witness to suffix-only coverage."""
    body = _recipe_body("_android-ffi-bindgen")
    assert "check-ffi-seam-diff.py" in body, (
        "_android-ffi-bindgen no longer invokes scripts/check-ffi-seam-diff.py — "
        "the flavor-diff seam witness (e2e-conventions.md point 15) is unwired, so "
        "non-test-named seams (the dial family, FfiChildAgentSpawner, …) have no "
        "witness again"
    )
    script = _REPO / "scripts" / "check-ffi-seam-diff.py"
    assert script.is_file(), "scripts/check-ffi-seam-diff.py is gone but the recipe calls it"
    text = script.read_text(encoding="utf-8")
    # The floor must keep naming the dial family — the socket-redirect seams the
    # mechanism was ratified over. Renames land here together with the script's
    # own presence-assert failure, never silently.
    for rust_name in ("set_provider_base_urls", "resolved_nest_dial_url", "FfiChildAgentSpawner"):
        assert rust_name in text, (
            f"check-ffi-seam-diff.py's KNOWN_SEAMS floor no longer names {rust_name!r} "
            "— if the seam was renamed, update the floor entry; if it was deleted, "
            "record that in the e2e-conventions.md point 15 build-out bullet"
        )


def test_apple_bindgen_runs_the_flavor_diff_witness():
    """`_apple-ffi-bindgen` must call the flavor-diff seam witness with the
    `swift` language, beside its `*ForTest` suffix grep — the apple leg of
    `test_android_bindgen_runs_the_flavor_diff_witness` above. Dropping this call would silently revert the witness
    to suffix-only coverage on every apple flavor (production, test-helpers,
    AND store-safe)."""
    body = _recipe_body("_apple-ffi-bindgen")
    assert "check-ffi-seam-diff.py" in body and "--lang swift" in body, (
        "_apple-ffi-bindgen no longer invokes scripts/check-ffi-seam-diff.py "
        "--lang swift — the flavor-diff seam witness (e2e-conventions.md "
        "point 15) is unwired, so non-test-named seams (the dial family, "
        "FfiChildAgentSpawner, …) have no witness again"
    )
    script = _REPO / "scripts" / "check-ffi-seam-diff.py"
    assert script.is_file(), "scripts/check-ffi-seam-diff.py is gone but the recipe calls it"
    text = script.read_text(encoding="utf-8")
    assert '"swift": {' in text, (
        "scripts/check-ffi-seam-diff.py's DECL_PATTERNS lost its 'swift' "
        "entry — the apple flavor-diff witness has no declaration extractor"
    )


def test_apple_bindgen_routes_the_store_safe_flavor_to_the_production_arm():
    """Apple has a THIRD flavor (`store-safe`, the App Store escape hatch) that
    the binary `[ -z "$FEATURES" ]` test windows/android use would mis-route:
    store-safe is shippable and must take `--production-tree`, exactly like
    empty FEATURES — only `test-helpers` may take `--test-tree`. This is the
    same three-flavor caveat the pre-existing `*ForTest` suffix grep already
    carries a few lines above; the flavor-diff call must not regress it."""
    body = _recipe_body("_apple-ffi-bindgen")
    m = re.search(
        r'if \[ "\$FEATURES" (=|!=) "test-helpers" \]; then\s*\n\s*FLAVOR_FLAG=(--\S+)',
        body,
    )
    assert m, (
        "_apple-ffi-bindgen's flavor-diff call has no recognizable "
        '`if [ "$FEATURES" =/!= "test-helpers" ]; then FLAVOR_FLAG=--…` '
        "routing guard"
    )
    op, flag = m.group(1), m.group(2)
    # `= "test-helpers"` must route to --test-tree (and everything else,
    # including store-safe, falls to --production-tree); `!= "test-helpers"`
    # inverted would route store-safe to --test-tree instead.
    assert (op, flag) == ("=", "--test-tree"), (
        f'flavor-diff routing reads `[ "$FEATURES" {op} "test-helpers" ]` -> '
        f"{flag} — store-safe (neither empty nor test-helpers) would take "
        "the wrong tree flag unless the guard is exactly "
        '`= "test-helpers"` -> --test-tree (else --production-tree)'
    )


def test_windows_bindgen_runs_the_flavor_diff_witness():
    """`_windows-ffi-bindgen` must call the flavor-diff seam witness with the
    `csharp` language, beside its `*ForTest` suffix grep — the windows leg of
    `test_android_bindgen_runs_the_flavor_diff_witness` above. It was the one native
    leg with no structural pin: android's and apple's recipes would go red on
    an unwired call, windows' would not. Dropping this call would
    silently revert the witness to suffix-only coverage on every windows
    flavor (production, test-helpers, AND store-safe)."""
    body = _recipe_body("_windows-ffi-bindgen")
    assert "check-ffi-seam-diff.py" in body and "--lang csharp" in body, (
        "_windows-ffi-bindgen no longer invokes scripts/check-ffi-seam-diff.py "
        "--lang csharp — the flavor-diff seam witness "
        "(e2e-automation-surface-gating.md § The convention, point 15) is "
        "unwired, so non-test-named seams (the dial family, "
        "FfiChildAgentSpawner, …) have no witness on windows again"
    )
    script = _REPO / "scripts" / "check-ffi-seam-diff.py"
    assert script.is_file(), "scripts/check-ffi-seam-diff.py is gone but the recipe calls it"
    text = script.read_text(encoding="utf-8")
    assert '"csharp": {' in text, (
        "scripts/check-ffi-seam-diff.py's DECL_PATTERNS lost its 'csharp' "
        "entry — the windows flavor-diff witness has no declaration extractor"
    )


def test_windows_bindgen_routes_the_store_safe_flavor_to_the_production_arm():
    """Windows has the same THIRD flavor apple does (`windows-ffi-store-safe`),
    so the same routing hazard: `store-safe` is shippable and must take
    `--production-tree`, exactly like empty FEATURES — only `test-helpers` may
    take `--test-tree`. The recipe routes with a `case ",$FEATURES," in
    *,test-helpers,*)` membership test rather than apple's `[ = ]` string
    test; pin the membership arm to --test-tree and the catch-all arm to
    --production-tree, so an inverted or emptiness-based rewrite reds here
    (the apple pin above is the same assertion for its own guard shape)."""
    body = _recipe_body("_windows-ffi-bindgen")
    m = re.search(
        r'case ",\$FEATURES," in\s*\n'
        r"\s*\*,test-helpers,\*\)\s*FLAVOR_FLAG=(--\S+)\s*;;\s*\n"
        r"\s*\*\)\s*FLAVOR_FLAG=(--\S+)\s*;;",
        body,
    )
    assert m, (
        "_windows-ffi-bindgen's flavor-diff call has no recognizable "
        '`case ",$FEATURES," in *,test-helpers,*) FLAVOR_FLAG=--… ;; *) '
        "FLAVOR_FLAG=--… ;;` routing guard"
    )
    member_flag, default_flag = m.group(1), m.group(2)
    assert (member_flag, default_flag) == ("--test-tree", "--production-tree"), (
        f"flavor-diff routing sends the test-helpers arm to {member_flag} and "
        f"every other flavor to {default_flag} — store-safe (neither empty nor "
        "test-helpers) takes the catch-all arm, which must be --production-tree, "
        "and only the test-helpers membership arm may be --test-tree"
    )


def test_android_test_ffi_recipe_exists_and_carries_the_seams():
    """The e2e flavor has to exist, or there is nowhere for the feature to live —
    the gap that kept all three native recipes on `test-helpers` until 2026-08-01
    (`apple-ffi-test` was cited by fauna-ffi's manifest but was never a recipe)."""
    assert "_android-ffi-flavor debug test-helpers" in _recipe_body("android-ffi-test"), (
        "android-ffi-test must build WITH `test-helpers` and stage into the `debug` "
        "source set, where the e2e TestAgent that calls the seams is compiled"
    )


def test_android_debug_and_release_consume_the_matching_ffi_flavor():
    """The staging split is only structural if the Gradle-facing recipes point at
    the right flavors: a debug build must never be handed production bindings
    (its TestAgent would not compile) and a release build must never be handed
    test ones (the seams would ship)."""
    assert "android-ffi-test" in _recipe_header("android-debug"), (
        "android-debug must depend on android-ffi-test — the debug variant compiles "
        "src/debug's TestAgent, which calls the *ForTest UniFFI seams"
    )
    release_deps = _recipe_header("android-release")
    assert "android-ffi" in release_deps and "android-ffi-test" not in release_deps, (
        "android-release must depend on the PRODUCTION android-ffi flavor — it is the "
        "recipe that packages the shipped APK"
    )


def test_android_ffi_stages_per_buildtype_not_into_a_shared_slot():
    """What makes the android gate structural rather than ordering-dependent:
    Gradle picks the source set by buildType, so `assembleRelease` cannot see the
    test-flavored bindings even if `android-ffi-test` ran last. A shared
    `src/main/java` staging slot would make correctness depend on build order."""
    body = _recipe_body("_android-ffi-flavor")
    assert 'STAGE_JAVA="apps/fauna-android/app/src/$BUILDTYPE/java"' in body
    assert 'STAGE_JNI="apps/fauna-android/app/src/$BUILDTYPE/jniLibs"' in body
    assert "src/main/java/uniffi" not in body and "src/main/jniLibs" not in body, (
        "the android FFI stage went back to the shared src/main slot — both flavors "
        "would then write the same files and the last build would decide what "
        "assembleRelease packages"
    )


def test_production_android_bindgen_asserts_zero_seams_in_its_own_output():
    """The self-maintaining artifact witness: the production bindgen greps the
    tree it just generated and fails if a `*ForTest` export survived, so a NEW
    ungated seam is caught with no list for anyone to update."""
    body = _recipe_body("_android-ffi-bindgen")
    assert "ForTest" in body and "*/src/release/*" in body, (
        "_android-ffi-bindgen lost its production-flavor seam assertion — a newly "
        "added ungated seam would ship silently"
    )


# ── the sync-agent pin gate (the 2026-08-02 seam-witness ruling's open half) ─
#
# `FAUNA_E2E_SYNC_AGENT_BIN` is a process-execution redirect: whoever controls
# the launch environment chooses the binary the client spawns (or bakes into
# its systemd user unit). The read lives in `fauna-client-sync` behind
# `cfg(any(test, debug_assertions, feature = "test-helpers"))`; these pins hold
# both halves of that gate in place.


def _manifest(rel: str) -> str:
    return (_REPO / rel / "Cargo.toml").read_text(encoding="utf-8")


def test_client_sync_declares_the_test_helpers_feature():
    """The gate's feature arm must exist: without it a release-profile e2e build
    has no way to keep the pin, and the harness silently drives a stale
    machine-global agent — masking a client that spawns nothing at all (the
    2026-07-24 multiseat failure both this pin and the pin itself exist for)."""
    manifest = _manifest("libs/fauna-client-sync")
    assert re.search(r"^test-helpers\s*=", manifest, re.MULTILINE), (
        "libs/fauna-client-sync lost its `test-helpers` feature — the "
        "FAUNA_E2E_SYNC_AGENT_BIN pin read has no release-e2e arm without it "
        "(e2e-automation-surface-gating.md convention 15, the 2026-08-02 seam-witness ruling)"
    )


@pytest.mark.parametrize(
    "consumer",
    ["libs/fauna-ffi", "apps/fauna-linux", "apps/fauna-tui"],
)
def test_no_spawner_consumer_names_client_sync_test_helpers_on_a_dep_line(consumer):
    """Rule (b): a dep line naming the feature turns it on unconditionally, so
    the pin read would ride every release artifact regardless of the recipe."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(consumer))
        if "fauna-client-sync" in line and "test-helpers" in line
    ]
    assert not offenders, (
        f"{consumer}/Cargo.toml names fauna-client-sync's `test-helpers` on a "
        "dependency line — the FAUNA_E2E_SYNC_AGENT_BIN read would ship in every "
        f"release artifact (e2e-automation-surface-gating.md convention 15 rule (b)): {offenders!r}"
    )


@pytest.mark.parametrize(
    ("consumer", "feature"),
    [
        ("apps/fauna-linux", "e2e-agent"),
        ("apps/fauna-tui", "e2e-agent"),
        ("libs/fauna-ffi", "test-helpers"),
    ],
)
def test_e2e_features_forward_the_agent_pin(consumer, feature):
    """The trap in the gate: `debug_assertions` alone loses the pin in
    release-profile e2e builds, whose silent fallback drives the box's INSTALLED
    agent (2026-07-24). Each consumer's own e2e feature must forward the crate
    feature, so `--release --features <e2e>` keeps the pin working."""
    manifest = _manifest(consumer)
    m = re.search(rf"^{re.escape(feature)}\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, f"{consumer}/Cargo.toml has no `{feature}` feature"
    assert "fauna-client-sync/test-helpers" in m.group(1), (
        f"{consumer}'s `{feature}` feature no longer forwards "
        "fauna-client-sync/test-helpers — a release-profile e2e build would lose "
        "the FAUNA_E2E_SYNC_AGENT_BIN pin and silently fall through to the box's "
        "installed agent (e2e-automation-surface-gating.md convention 15; the 2026-07-24 multiseat bite)"
    )


# ── the connect-side pipe override (the same ruling's other open half) ───────
#
# `FAUNA_E2E_SYNC_PIPE` is an ENDPOINT redirect: whoever controls the launch
# environment names the pipe a released windows app treats as its sync agent.
# It is not the lesser sibling of the binary pin it was filed as — what the
# client pushes down that pipe is `RequestMethod::ProvisionCapability`, i.e. the
# owner's `BackupKey`, the per-folder content keys and a renewable nest bearer
# (`libs/fauna-ipc/src/sync.rs`), so an attacker-named pipe is key-material
# disclosure. The read lives in `fauna-ipc` behind
# `cfg(any(test, debug_assertions, feature = "test-helpers"))`, and the spawn
# side (`fauna-client-sync`) must carry the SAME flavor or a release-profile e2e
# build spawns on the harness pipe and connects to the per-SID one.
#
# Measured two-column on the real windows-target object code (2026-08-10, from a
# Linux dev VM — no Windows machine involved):
# `ar x` the `x86_64-pc-windows-msvc` release rlib and grep the `.o` members —
# 0 occurrences of the env name in the production flavor, 1 under
# `--features test-helpers`. (The rlib as a whole shows 2 either way: a `pub
# const`'s value lives in `.rmeta` for downstream crates, so a whole-rlib
# `strings` is a false red — grep the object members, not the archive.)


def test_ipc_declares_the_test_helpers_feature():
    """The gate's feature arm must exist: without it a release-profile e2e build
    of a windows app resolves the per-SID pipe while the harness-spawned agent
    bound the per-launch one — the two halves disagree and the run reads as "no
    agent running", the exact misdiagnosis `pipe_name_from_env`'s doc warns of."""
    manifest = _manifest("libs/fauna-ipc")
    assert re.search(r"^test-helpers\s*=", manifest, re.MULTILINE), (
        "libs/fauna-ipc lost its `test-helpers` feature — the "
        "FAUNA_E2E_SYNC_PIPE connect-side override has no release-e2e arm "
        "without it (convention 15; the 2026-08-02 seam-witness ruling)"
    )


def test_client_sync_test_helpers_forwards_the_connect_side_override():
    """The spawn side and the connect side must flip together. `fauna-ipc` has no
    e2e feature of its own for an app to name, so the one crate that composes the
    matching `--pipe-name` is the one that forwards it — and every app already
    forwards `fauna-client-sync/test-helpers`, so the whole fleet inherits it."""
    manifest = _manifest("libs/fauna-client-sync")
    m = re.search(r"^test-helpers\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, "libs/fauna-client-sync has no `test-helpers` feature"
    assert "fauna-ipc/test-helpers" in m.group(1), (
        "fauna-client-sync's `test-helpers` no longer forwards "
        "fauna-ipc/test-helpers — a release-profile e2e windows build would keep "
        "the spawn-side --pipe-name forward and lose the connect-side override, "
        "so the client would talk to the per-SID pipe its own agent never bound"
    )


@pytest.mark.parametrize(
    "consumer",
    ["libs/fauna-client-sync", "libs/fauna-ffi", "apps/fauna-linux", "apps/fauna-tui"],
)
def test_no_consumer_names_ipc_test_helpers_on_a_dep_line(consumer):
    """Rule (b) again, for the crate the pipe read lives in: a dep line naming the
    feature turns it on unconditionally, putting the override back into every
    release artifact regardless of the recipe."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(consumer))
        if "fauna-ipc" in line and "test-helpers" in line
    ]
    assert not offenders, (
        f"{consumer}/Cargo.toml names fauna-ipc's `test-helpers` on a dependency "
        "line — the FAUNA_E2E_SYNC_PIPE read would ship in every release artifact "
        f"(convention 15 rule (b)): {offenders!r}"
    )


# ── the automation-port reads: gated, not allowlisted ────────────────────────
#
# `FAUNA_E2E_AGENT_PORT` is a mode toggle, not a redirect, so it was weighed for
# the ruling's conclusion-(3) allowlist and rejected: that allowlist is for reads
# production genuinely needs, and this is the very gate convention 15 requires to
# be compiled out. Every remaining ungated read on tui and linux now carries its
# app's own `cfg(any(debug_assertions, feature = "e2e-agent"))`. linux is here
# because gating tui alone would be the per-app divergence priority #1 forbids —
# its two automation-SERVER reads were already gated, these were the peers that
# were not.

#
# tui's two module-local readers (`sync_agent::e2e_gated`,
# `conversations::e2e_mode`) were lifted into one crate-level
# `e2e_mode_enabled()` in `main.rs` (with a production twin) — the linux shape —
# so tui is pinned there now; a new ungated reader anywhere under `apps/*/src/`
# is caught by `test_shared_crate_seam_gating.py`'s env-seed scan.

_AUTOMATION_PORT_READS = [
    ("apps/fauna-tui/src/main.rs", "pub fn e2e_mode_enabled"),
    ("apps/fauna-linux/src/main.rs", "pub fn e2e_mode_enabled"),
    ("apps/fauna-linux/src/instance_remote.rs", "fn e2e_child_agent_port"),
]


@pytest.mark.parametrize(("rel", "fn_decl"), _AUTOMATION_PORT_READS)
def test_automation_port_reads_are_compile_gated(rel, fn_decl):
    """Each read's `fn` line must be immediately preceded by a cfg carrying the
    `e2e-agent` arm. A runtime `is_some()` check is the inner switch, never the
    boundary — an ungated read leaves the env name in the shipped binary and lets
    whoever sets it flip the agent's residency mode."""
    text = (_REPO / rel).read_text(encoding="utf-8")
    decls = [i for i, line in enumerate(text.splitlines()) if line.startswith(fn_decl)]
    assert decls, f"{rel} no longer declares `{fn_decl}` at module level"
    lines = text.splitlines()
    for i in decls:
        preceding = _cfg_attrs_above(lines, i)
        assert 'feature = "e2e-agent"' in preceding and "#[cfg(" in preceding, (
            f"{rel}'s `{fn_decl}` reads FAUNA_E2E_AGENT_PORT without a "
            "compile-time gate on the lines above it — convention 15 requires the "
            f"automation surface out of release artifacts. Saw:\n{preceding}"
        )


# ── the credential-store redirect: gated, not allowlisted ────────────────────
#
# `FAUNA_E2E_CREDENTIAL_DIR` is where the identity secret is read AND written:
# it redirects the credential store — libsecret / the macOS login Keychain —
# onto a caller-named plaintext-JSON dir, so whoever controls a released app's
# environment both harvests what the app stores and supplies what the app
# loads. Same class as the C# arm this suite already pins
# (test_no_windows_app_source_names_a_fauna_e2e_var_outside_the_gate); gating
# windows alone was the per-app divergence priority #1 forbids. The read lives
# in `fauna-credential-store` behind
# `cfg(any(test, debug_assertions, feature = "e2e-agent"))` with a
# `None`-returning production twin (the `fauna_ipc::endpoint::e2e_pipe_override`
# pair shape). `FAUNA_E2E_FORCE_HEADLESS_STORE` — same crate, declared test-only
# by its own doc — is gated as the same family: it flips a released tui onto the
# sealed-file backend, a store redirect even if a weaker one.

#
# `FAUNA_KEYRING_APP` joined the family 2026-08-15. It is
# a lesser class — a namespace *within* the user's own keyring, not a relocation
# out of it, so it neither harvests nor supplies the secret — and it is gated
# anyway for conclusion (3)'s reason: the allowlist is for reads production
# genuinely needs (`APPIMAGE`, `FLATPAK_ID`), and this is a harness knob no
# deployment sets. ⚠ It is NOT `FAUNA_E2E_`-prefixed, which is exactly why the
# 2026-08-11 sweep never met it — the C# pin below matches on that prefix, so
# this read needed naming explicitly on both faces.
_CREDENTIAL_REDIRECT_READS = [
    ("libs/fauna-credential-store/src/lib.rs", "pub fn cred_file_dir"),
    ("libs/fauna-credential-store/src/lib.rs", "pub fn force_headless_store"),
    ("libs/fauna-credential-store/src/lib.rs", "pub fn keyring_app_override"),
]


def _line_is_in_a_cfg_test_module(text, lineno):
    """Is 1-based `lineno` inside a `#[cfg(test)] mod … { … }` block?

    Brace-tracked rather than "is it below the first `#[cfg(test)]`", because
    Rust files here put test modules mid-file as often as at the end, and the
    loose form would exempt every production read that follows one.
    """
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if lines[i].strip().startswith("#[cfg(test)]"):
            # Walk to the `{` that opens the module/item, then to its match.
            j, depth, opened = i, 0, False
            while j < len(lines):
                depth += lines[j].count("{") - lines[j].count("}")
                if "{" in lines[j]:
                    opened = True
                if opened and depth <= 0:
                    break
                j += 1
            if i < lineno <= j + 1:
                return True
            i = j + 1
            continue
        i += 1
    return False


def test_no_second_copy_of_the_keyring_namespace_read():
    """The namespace read has exactly ONE home: the shared crate's gated
    accessor.

    A per-app `std::env::var("FAUNA_KEYRING_APP")` is both the priority-#1
    divergence row 194 existed to fix AND a live gate bypass — it would keep the
    env name in a shipped `fauna-desktop`/`fauna-tui` after the shared crate
    stopped naming it, which is the failure this whole family guards against.
    linux's `cred_app()` was the one such copy; it now calls the accessor.

    Test sources are exempt: the `test` cfg arm compiles the real accessor
    anyway, so a test naming the var directly ships nothing.
    """
    offenders = []
    for rel in ("apps/fauna-linux/src", "apps/fauna-tui/src", "bins/fauna-sync-agent/src"):
        for path in (_REPO / rel).rglob("*.rs"):
            text = path.read_text(encoding="utf-8", errors="replace")
            for n, line in enumerate(text.splitlines(), 1):
                if 'env::var' in line and "FAUNA_KEYRING_APP" in line:
                    if _line_is_in_a_cfg_test_module(text, n):
                        continue
                    offenders.append(f"{path.relative_to(_REPO)}:{n}: {line.strip()}")
    assert not offenders, (
        "these read FAUNA_KEYRING_APP directly instead of "
        "`fauna_credential_store::keyring_app_override()` — an ungated copy "
        "defeats the gate on the shared accessor:\n  " + "\n  ".join(offenders)
    )


@pytest.mark.parametrize(("rel", "fn_decl"), _CREDENTIAL_REDIRECT_READS)
def test_credential_redirect_reads_are_compile_gated(rel, fn_decl):
    """Every declaration — the gated real and its production twin — must carry a
    cfg naming the `e2e-agent` arm on the lines above it. A runtime `is_some()`
    is the inner switch, never the boundary: an ungated read leaves the env name
    in the shipped binary and lets whoever sets it relocate where the identity
    secret is read and written."""
    text = (_REPO / rel).read_text(encoding="utf-8")
    decls = [i for i, line in enumerate(text.splitlines()) if line.startswith(fn_decl)]
    assert decls, f"{rel} no longer declares `{fn_decl}` at module level"
    lines = text.splitlines()
    for i in decls:
        preceding = _cfg_attrs_above(lines, i)
        assert 'feature = "e2e-agent"' in preceding and "#[cfg(" in preceding, (
            f"{rel}'s `{fn_decl}` reads its FAUNA_E2E_* store redirect without a "
            "compile-time gate on the lines above it — convention 15 requires the "
            f"automation surface out of release artifacts. Saw:\n{preceding}"
        )


def test_credential_store_declares_the_e2e_agent_feature():
    """The gate's feature arm must exist: without it a release-profile e2e build
    has no way to keep the redirect — it silently falls back to libsecret and
    writes the run's credentials into the dev box's real keyring (the exact trap
    the capture warned about)."""
    manifest = _manifest("libs/fauna-credential-store")
    assert re.search(r"^e2e-agent\s*=", manifest, re.MULTILINE), (
        "libs/fauna-credential-store has no `e2e-agent` feature — the "
        "FAUNA_E2E_CREDENTIAL_DIR redirect has no release-e2e arm without it "
        "(e2e-conventions.md convention 15, the 2026-08-02 seam-witness ruling)"
    )


@pytest.mark.parametrize(
    "consumer",
    ["apps/fauna-linux", "apps/fauna-tui", "bins/fauna-sync-agent"],
)
def test_e2e_features_forward_the_credential_redirect(consumer):
    """`debug_assertions` alone loses the redirect in release-profile e2e builds
    (the same trap as the agent pin): each consumer's own `e2e-agent` feature
    must forward the crate feature, so `--release --features e2e-agent` keeps
    per-launch credential isolation working."""
    manifest = _manifest(consumer)
    m = re.search(r"^e2e-agent\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, f"{consumer}/Cargo.toml has no `e2e-agent` feature"
    assert "fauna-credential-store/e2e-agent" in m.group(1), (
        f"{consumer}'s `e2e-agent` feature no longer forwards "
        "fauna-credential-store/e2e-agent — a release-profile e2e build would "
        "lose the FAUNA_E2E_CREDENTIAL_DIR redirect and silently write into the "
        "box's real keyring (convention 15)"
    )


def test_client_sync_test_helpers_forwards_the_credential_redirect():
    """The UniFFI family's leg of the same forward — and the one the pin above
    could never have covered, because those apps reach the store through a crate
    rather than a dep line of their own.

    `fauna-ffi` has no `fauna-credential-store` dependency to name (a Cargo
    feature may only reference a DIRECT dep); it reaches the crate through
    `fauna-client-sync` and `fauna-sync-engine`. So the forward lives on
    `fauna-client-sync`'s `test-helpers` — exactly where the `fauna-ipc` one
    does, and for exactly the same reason — and every UniFFI app inherits it,
    since `fauna-ffi`'s `test-helpers` already forwards
    `fauna-client-sync/test-helpers`.

    ⚠ **The parametrized pin above listed only linux, tui and the sync agent, so
    macos/ios/windows/android were never checked — and all four were broken the
    whole time.** Their FFI slices build `--release`, so `cred_file_dir()`
    compiled to its production twin and `CredentialStore::new` fell through to
    the platform keyring arm. macOS and windows have REAL keyring arms, so their
    e2e runs silently wrote writer keys into the box's own keychain — precisely
    the isolation loss this family of pins exists to forbid, invisible because
    the writes succeeded. iOS has no keyring arm at all: `no_keyring` drops the
    write, the W3 (account-data-plane.md § Workstreams) account runtime's writer-key mint fails its read-back, and the
    assembly refuses — which is how it was finally caught, as `--app ios`
    sitting at `role: (False, False)` for 240 s.
    """
    manifest = _manifest("libs/fauna-client-sync")
    m = re.search(r"^test-helpers\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, "libs/fauna-client-sync has no `test-helpers` feature"
    assert "fauna-credential-store/e2e-agent" in m.group(1), (
        "fauna-client-sync's `test-helpers` no longer forwards "
        "fauna-credential-store/e2e-agent — every UniFFI app's release-profile "
        "e2e build would lose the FAUNA_E2E_CREDENTIAL_DIR redirect: macos and "
        "windows would write the run's credentials into the box's real keyring, "
        "and iOS (no keyring arm) could not host a W3 account runtime at all"
    )


@pytest.mark.parametrize(
    "consumer",
    ["apps/fauna-linux", "apps/fauna-tui", "bins/fauna-sync-agent"],
)
def test_no_consumer_names_credential_store_e2e_agent_on_a_dep_line(consumer):
    """Rule (b): a dep line naming the feature turns it on unconditionally, so
    the store redirect would ride every release artifact regardless of the
    recipe."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(consumer))
        if "fauna-credential-store" in line and "e2e-agent" in line
    ]
    assert not offenders, (
        f"{consumer}/Cargo.toml names fauna-credential-store's `e2e-agent` on a "
        "dependency line — the FAUNA_E2E_CREDENTIAL_DIR redirect would ship in "
        f"every release artifact (convention 15 rule (b)): {offenders!r}"
    )


# ── the conversations push-arm kill-switch ───────────────────────────────────
#
# `FAUNA_E2E_SUPPRESS_CONV_PUSH` makes `conv_push_source` return `None`, so the
# session is built with no push source and conversations degrade to drain-only
# receive — no real-time message delivery until the process restarts without the
# variable. Until 2026-08-21 its only gate was
# `cfg(not(target_arch = "wasm32"))`, a PLATFORM gate rather than a build-flavor
# one, so the read compiled into every native RELEASE artifact and reached six of
# the seven apps through three un-cfg-gated production call sites
# (`libs/fauna-ffi/src/nest_client.rs` → macos/ios/windows/android, plus tui's and
# linux's `conv_backend.rs`). Worse than a stray debug knob on three counts: the
# trigger is `var_os(…).is_some()`, so an EMPTY value suffices; the only notice is
# one `tracing::warn!`; and both native call sites carried a comment saying the arm
# is "always live in production", the sentence that stops the next reader checking.
#
# ⚠ It is NOT in the `*ForTest`/export-shaped seam class, so neither the binding
# flavor-diff witness nor any `strings`-over-exports grep could ever have seen it —
# the 2026-08-02 ruling's conclusion (2) exactly (a plain env read in ungated
# production code is not an export at all). It was found by hand, which is why the
# reverse witness over behaviour primitives stays owed.

_CONV_PUSH_SUPPRESSION_READS = [
    ("libs/fauna-client-conversations/src/lib.rs", "pub fn conv_push_source"),
]


@pytest.mark.parametrize(("rel", "fn_decl"), _CONV_PUSH_SUPPRESSION_READS)
def test_conv_push_suppression_read_is_compile_gated(rel, fn_decl):
    """Both declarations — the gated real and its production twin — must carry a
    cfg naming the `e2e-agent` arm above them.

    A platform-only `cfg(not(target_arch = "wasm32"))` is what shipped the
    kill-switch: it is not a build-flavor gate, so it excludes the read from
    exactly one target and from no artifact anyone installs.
    """
    text = (_REPO / rel).read_text(encoding="utf-8")
    lines = text.splitlines()
    decls = [i for i, line in enumerate(lines) if line.startswith(fn_decl)]
    assert len(decls) == 2, (
        f"{rel} declares `{fn_decl}` {len(decls)} time(s); convention 15's shape "
        "here is a PAIR — the gated real plus a same-signature production twin — "
        "so the three native session-build call sites compile unchanged in both arms"
    )
    for i in decls:
        preceding = _cfg_attrs_above(lines, i)
        assert 'feature = "e2e-agent"' in preceding and "#[cfg(" in preceding, (
            f"{rel}'s `{fn_decl}` reads FAUNA_E2E_SUPPRESS_CONV_PUSH without a "
            "compile-time gate on the lines above it — convention 15 requires the "
            "automation surface out of release artifacts, and this one lets whoever "
            f"sets the var kill real-time delivery on six apps. Saw:\n{preceding}"
        )


def test_sync_agent_e2e_agent_forwards_its_own_test_helpers():
    """The custodian poke's HANDLER must compile whenever its VARIANT exists, and
    only this forward makes that true in a release-profile e2e build.

    Unlike its siblings above, this one does not degrade — it fails the BUILD.
    `fauna_ipc::sync::RequestMethod::CustodianRunPassNow` and
    `pipe_server.rs`'s arm for it carry the SAME
    `any(test, debug_assertions, feature = "test-helpers")` gate, but the
    `feature` in each names a DIFFERENT crate: the variant's presence is decided
    by `fauna-ipc`'s feature, which cargo unifies across the whole build, while
    the arm's is decided by `fauna-sync-agent`'s own.

    The flatpak e2e flavour runs exactly one command, and it turns both on for
    reasons that do not meet::

        cargo build --release -p fauna-linux -p fauna-sync-agent \
            --features fauna-linux/e2e-agent,fauna-sync-agent/e2e-agent

    `fauna-linux/e2e-agent` reaches `fauna-ipc/test-helpers` through
    `fauna-client-sync/test-helpers`, so the variant EXISTS; without the forward
    asserted here, `--release` has no `debug_assertions` and the arm is compiled
    out, so the match stops being exhaustive and the whole linux app fails to
    build with `error[E0004]`. Measured 2026-09-02: that is exactly how
    `tests/real_session/test_sync_agent_flatpak_seam.py` died at setup, and no
    gate ran the build that would have caught it.

    ⚠ The `test-helpers` feature's own comment in that manifest claims forwarding
    the wire half means "the variant and its handler can never be present on only
    one side of the seam". That is true only for a build that asks THIS crate for
    the feature; it says nothing about a sibling in the same build asking
    `fauna-ipc` directly, which is what feature unification then does. Convention
    15 rule (b)'s wording is the fix and this pin is its witness: a consumer
    "forwards it from its own `e2e-agent` feature".
    """
    manifest = _manifest("bins/fauna-sync-agent")
    m = re.search(r"^e2e-agent\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, "bins/fauna-sync-agent/Cargo.toml has no `e2e-agent` feature"
    # Comments STRIPPED before matching, and this is not defensive tidiness: the
    # block's own comment explains the forward and necessarily spells
    # `"test-helpers"` while doing so, so a naive search over the raw block
    # matches the prose and passes with the entry deleted. Caught by
    # red-verifying this pin — the first version was a false green.
    entries = "\n".join(
        line.split("#", 1)[0] for line in m.group(1).splitlines()
    )
    assert re.search(r'"test-helpers"', entries), (
        "bins/fauna-sync-agent's `e2e-agent` no longer forwards its own "
        "`test-helpers` — a release-profile e2e build of the linux app now fails "
        "to COMPILE (error[E0004] on pipe_server.rs's match), because "
        "fauna-linux/e2e-agent turns fauna-ipc/test-helpers on through "
        "fauna-client-sync while this crate's handler arm compiles out "
        "(e2e-automation-surface-gating.md, convention 15 rule (b))"
    )


def test_client_conversations_declares_the_e2e_agent_feature():
    """The gate's feature arm must exist: without it a release-profile e2e build
    has no way to keep the suppression knob, and the layer-5 missed-push proofs
    (`test_conv_rail_push_wakes_native.py`, `test_fauna_mls_two_client_inbox_drain.py`)
    silently run with the push arm LIVE — i.e. unable to fail."""
    manifest = _manifest("libs/fauna-client-conversations")
    assert re.search(r"^e2e-agent\s*=", manifest, re.MULTILINE), (
        "libs/fauna-client-conversations has no `e2e-agent` feature — the "
        "FAUNA_E2E_SUPPRESS_CONV_PUSH knob has no release-e2e arm without it "
        "(e2e-automation-surface-gating.md § convention 15)"
    )


@pytest.mark.parametrize(
    ("consumer", "feature"),
    [
        ("apps/fauna-linux", "e2e-agent"),
        ("apps/fauna-tui", "e2e-agent"),
        ("libs/fauna-ffi", "test-helpers"),
    ],
)
def test_e2e_features_forward_the_conv_push_suppression(consumer, feature):
    """`debug_assertions` alone loses the knob in release-profile e2e builds (the
    same trap as the agent pin and the credential redirect). Each consumer that
    builds a conversations session must forward the crate feature from its own e2e
    feature — `fauna-ffi` covers the four UniFFI apps, whose test flavors all pass
    `test-helpers` whatever profile they use."""
    manifest = _manifest(consumer)
    m = re.search(rf"^{re.escape(feature)}\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, f"{consumer}/Cargo.toml has no `{feature}` feature"
    assert "fauna-client-conversations/e2e-agent" in m.group(1), (
        f"{consumer}'s `{feature}` feature no longer forwards "
        "fauna-client-conversations/e2e-agent — a release-profile e2e build would "
        "lose FAUNA_E2E_SUPPRESS_CONV_PUSH and the drain-alone receive proofs would "
        "pass with the push arm live (convention 15)"
    )


@pytest.mark.parametrize(
    "consumer",
    ["libs/fauna-ffi", "apps/fauna-linux", "apps/fauna-tui", "bins/fauna-sync-agent"],
)
def test_no_consumer_names_client_conversations_e2e_agent_on_a_dep_line(consumer):
    """Rule (b): a dep line naming the feature turns it on unconditionally, so the
    kill-switch would ride every release artifact regardless of the recipe — which
    is the state this whole row existed to end."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(consumer))
        if "fauna-client-conversations" in line and "e2e-agent" in line
    ]
    assert not offenders, (
        f"{consumer}/Cargo.toml names fauna-client-conversations' `e2e-agent` on a "
        "dependency line — FAUNA_E2E_SUPPRESS_CONV_PUSH would ship in every release "
        f"artifact (convention 15 rule (b)): {offenders!r}"
    )


# ── the conversations backstop-cadence override (the kill-switch's sibling) ──
#
# `FAUNA_CONV_POLL_SECS` sets the receive loop's backstop ticker. A LESSER class
# than the kill-switch above — the push arm stays live and a mis-set value falls
# back to the production cadence — but setting it huge in a shipped build mutes
# the missed-push recovery rail (api-layers.md § Inbox & Messaging, layer 3)
# until the process restarts. Gated for the 2026-08-02 ruling's conclusion (3)
# reason rather than severity's: the allowlist is for reads production genuinely
# needs (`APPIMAGE`, `FLATPAK_ID`), and this is a harness knob no deployment
# sets. ⚠ It carries no `FAUNA_E2E_` prefix — the `FAUNA_KEYRING_APP` lesson
# again: a harness variable is defined by WHO SETS IT, not by how it is spelled,
# which is why every prefix-keyed sweep walked past it. Reuses the crate's own
# `test-helpers` feature (already forwarded by linux, tui and fauna-ffi).


def test_conv_poll_secs_read_is_compile_gated():
    """The override read must live behind a cfg with a feature arm, in the
    real-plus-production-twin pair shape — never bare at the call site, where it
    was until 2026-08-21."""
    rel = "libs/fauna-conversations/src/session.rs"
    text = (_REPO / rel).read_text(encoding="utf-8")
    lines = text.splitlines()
    decls = [i for i, line in enumerate(lines) if line.startswith("fn poll_secs_override")]
    assert len(decls) == 2, (
        f"{rel} declares `fn poll_secs_override` {len(decls)} time(s); the shape is "
        "a PAIR — the gated real plus a `None`-returning production twin"
    )
    for i in decls:
        preceding = _cfg_attrs_above(lines, i)
        assert 'feature = "test-helpers"' in preceding and "#[cfg(" in preceding, (
            f"{rel}'s `poll_secs_override` reads FAUNA_CONV_POLL_SECS without a "
            "compile-time gate on the lines above it — convention 15 keeps harness "
            f"knobs out of release artifacts. Saw:\n{preceding}"
        )


def test_no_second_copy_of_the_conv_poll_secs_read():
    """The override has exactly ONE home: the gated accessor.

    A second `std::env::var("FAUNA_CONV_POLL_SECS")` anywhere in shipped source
    would keep the name in the release binary after the accessor stopped naming
    it — the `FAUNA_KEYRING_APP` second-copy failure, which is why that family
    grew the same pin. Test sources are exempt: the `test` cfg arm compiles the
    real accessor anyway, so a test naming the var directly ships nothing.
    """
    offenders = []
    roots = (
        "libs/fauna-conversations/src",
        "libs/fauna-client-conversations/src",
        "apps/fauna-linux/src",
        "apps/fauna-tui/src",
        "libs/fauna-ffi/src",
    )
    for rel in roots:
        for path in (_REPO / rel).rglob("*.rs"):
            text = path.read_text(encoding="utf-8", errors="replace")
            for n, line in enumerate(text.splitlines(), 1):
                if "env::var" in line and "FAUNA_CONV_POLL_SECS" in line:
                    if _line_is_in_a_cfg_test_module(text, n):
                        continue
                    if path == _REPO / "libs/fauna-conversations/src/session.rs":
                        continue  # the one gated accessor
                    offenders.append(f"{path.relative_to(_REPO)}:{n}: {line.strip()}")
    assert not offenders, (
        "these read FAUNA_CONV_POLL_SECS directly instead of the gated "
        "`poll_secs_override()` accessor — an ungated copy defeats the gate:\n  "
        + "\n  ".join(offenders)
    )


# ── row 392: fauna-ffi's under-forward vs linux/tui ──────────────────────────
#
# linux and tui each forward `e2e-agent` for 7 crates from their own feature of
# the same name; `fauna-ffi`'s `test-helpers` forwarded only 3 (the atproto
# delegation clock, the backup audit clock, and the conv-push kill-switch
# above) until now. The other three of linux/tui's seven —
# `fauna-sync-engine`'s `always_resident::rescan_interval`
# (FAUNA_E2E_RESCAN_MS), `fauna-core`'s screen_time.rs test-clock seams, and
# `fauna-launch-machine`'s dial seams — were never forwarded, so every UniFFI
# app's `--release` e2e flavor (apple-ffi-test, windows-ffi-test,
# android-ffi-test) compiled the PRODUCTION twin of each:
# `drivers/ios.py::launch` has set `SIMCTL_CHILD_FAUNA_E2E_RESCAN_MS` since
# 2026-08-25 with no test able to see or budget for it. All three crates are
# already DIRECT `fauna-ffi` dependencies (unlike `fauna-credential-store`,
# which needed the `fauna-client-sync` hop above), so each forward is a plain
# `"<crate>/e2e-agent"` entry in `test-helpers` — the same shape as the
# conv-push entry above, no intermediary crate involved.

_FFI_PARITY_SEAMS = (
    "fauna-sync-engine/e2e-agent",
    "fauna-core/e2e-agent",
    "fauna-launch-machine/e2e-agent",
)


@pytest.mark.parametrize("target", _FFI_PARITY_SEAMS)
@pytest.mark.parametrize(
    ("consumer", "feature"),
    [
        ("apps/fauna-linux", "e2e-agent"),
        ("apps/fauna-tui", "e2e-agent"),
        ("libs/fauna-ffi", "test-helpers"),
    ],
)
def test_e2e_features_forward_the_ffi_parity_seams(consumer, feature, target):
    """Each consumer's own e2e feature must forward the crate feature, so a
    release-profile e2e build keeps the seam — the same trap as the agent pin,
    the credential redirect and the conv-push kill-switch above. `fauna-ffi`'s
    three test flavors (apple-ffi-test, windows-ffi-test, android-ffi-test)
    all pass `test-helpers` whatever build profile they use, so its forward is
    what covers all four UniFFI apps at once."""
    manifest = _manifest(consumer)
    m = re.search(rf"^{re.escape(feature)}\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
    assert m, f"{consumer}/Cargo.toml has no `{feature}` feature"
    assert target in m.group(1), (
        f"{consumer}'s `{feature}` feature no longer forwards {target} — a "
        "release-profile e2e build would compile the production twin of that "
        "seam and lose the harness's ability to see or budget for it "
        "(e2e-automation-surface-gating.md convention 15)"
    )


@pytest.mark.parametrize("crate", ["fauna-sync-engine", "fauna-core", "fauna-launch-machine"])
@pytest.mark.parametrize(
    "consumer",
    ["libs/fauna-ffi", "apps/fauna-linux", "apps/fauna-tui", "bins/fauna-sync-agent"],
)
def test_no_consumer_names_ffi_parity_seam_on_a_dep_line(consumer, crate):
    """Rule (b): a dep line naming the feature turns it on unconditionally, so
    the seam would ride every release artifact regardless of the recipe."""
    offenders = [
        line
        for line in _shipping_dep_lines(_manifest(consumer))
        if crate in line and "e2e-agent" in line
    ]
    assert not offenders, (
        f"{consumer}/Cargo.toml names {crate}'s `e2e-agent` on a dependency "
        f"line — the seam would ship in every release artifact (convention 15 "
        f"rule (b)): {offenders!r}"
    )


# ── the artifact witness: the checked-in production Go binding ───────────────
#
# `libs/fauna-mail-go/` is generated by `just mail-bridge-ffi` from the
# PRODUCTION feature set (fauna-ffi names `test-helpers` on no dep line, and the
# recipe passes none), and it is TRACKED. That makes it the one generated native
# face a tier_1 text test can read directly — no build, no toolchain — and the
# `mail-bridge-ffi-check` heavy gate is what stops it going stale.

_GO_ONBOARDING = (
    _REPO / "libs" / "fauna-mail-go" / "fauna_onboarding_machine" / "fauna_onboarding_machine.go"
)


def test_production_go_binding_has_no_provider_base_url_override_surface():
    """The provider base-URL override redirects the wizard's VPS-provisioning,
    DNS and nest-health calls to a caller-supplied host. It was exported with no
    cfg at all — not even the `debug_assertions` arm its neighbours use — so it
    rode every native release artifact until 2026-08-01.

    Both halves had to move behind the gate, which is why the constructor is
    asserted too: the map was *also* a parameter on the exported constructor, so
    gating only the setter would have left the injection channel wide open.
    """
    src = _GO_ONBOARDING.read_text(encoding="utf-8")
    offenders = sorted({m for m in re.findall(r"\b(?:Set)?ProviderBaseUrls?\b", src)})
    assert not offenders, (
        "the production Go binding exports the provider base-URL override again "
        f"({offenders}) — an automation seam in a shipped artifact "
        "(e2e-automation-surface-gating.md convention 15). It belongs in the "
        "`test-helpers`-gated impl block."
    )


def test_production_go_binding_constructor_takes_no_override_map():
    """The constructor's own signature is the second half of the same seam."""
    src = _GO_ONBOARDING.read_text(encoding="utf-8")
    m = re.search(r"^func NewOnboardingMachine\(([^)]*)\)", src, re.MULTILINE)
    assert m, "NewOnboardingMachine vanished from the generated production binding"
    params = [p for p in m.group(1).split(",") if p.strip()]
    assert len(params) == 1, (
        "NewOnboardingMachine grew a second parameter — if that is the provider "
        f"base-URL override map it is an injection channel in a shipped artifact. Got: {params}"
    )


# ── the windows leg (closed 2026-08-01) ──────────────────────────────────────
#
# Windows cannot use android's buildType source sets: the WinUI project consumes
# ONE fixed pair of paths (`runtimes/win-arm64/native/fauna_ffi.dll` and
# `FaunaApp.Core/Generated/uniffi`), so both flavors share a staging slot and the
# `.ffi-flavor` marker is what keeps the gate honest — exactly the shape
# `_android-ffi-flavor`'s own comment predicted apple/windows would need.


def test_windows_production_ffi_recipe_does_not_build_the_seams():
    """`just windows-ffi` builds what `windows-release` packages."""
    chain = _recipe_body("windows-ffi") + _recipe_body("_windows-ffi-flavor")
    build_lines = [ln for ln in chain.splitlines() if "cargo-win.cmd rustc" in ln]
    assert build_lines, "no cargo rustc line found in the windows-ffi chain"
    for line in build_lines:
        assert "test-helpers" not in line, (
            "a windows-ffi cargo line hard-codes `test-helpers`, so the seams ship in "
            "the fauna_ffi.dll staged into runtimes/win-arm64/native/ — the one "
            f"`windows-release` packages (e2e-automation-surface-gating.md convention 15). Offending line: {line.strip()}"
        )
    assert '_windows-ffi-flavor "{{profile}}" ""' in _recipe_body("windows-ffi"), (
        "windows-ffi must delegate to the shared impl with an EMPTY feature set; "
        "that empty string is the production flavor"
    )


def test_windows_test_ffi_recipe_exists_and_carries_the_seams():
    """The e2e flavor has to exist, or there is nowhere for the feature to live —
    the gap that kept all three native recipes on `test-helpers` until 2026-08-01."""
    assert '_windows-ffi-flavor "{{profile}}" "test-helpers"' in _recipe_body("windows-ffi-test"), (
        "windows-ffi-test must build WITH `test-helpers` — it feeds the Debug "
        "configuration, where the `#if DEBUG` TestAgent calls the *ForTest seams"
    )


def test_windows_debug_and_release_consume_the_matching_ffi_flavor():
    """A Debug build must never be handed production bindings (its `#if DEBUG`
    TestAgent and the 5 unit-test files calling `InstallMockBackendsForTest`
    would not compile) and a Release build must never be handed test ones."""
    for debug_recipe in ("windows-debug", "windows-cs-test"):
        assert "windows-ffi-test" in _recipe_header(debug_recipe), (
            f"{debug_recipe} must depend on windows-ffi-test — it builds "
            "Configuration=Debug, which compiles the seam call sites"
        )
    release_deps = _recipe_header("windows-release")
    assert "windows-ffi" in release_deps and "windows-ffi-test" not in release_deps, (
        "windows-release must depend on the PRODUCTION windows-ffi flavor — it is "
        "the recipe that builds the shipped app"
    )


def test_release_workflow_builds_the_production_windows_ffi_flavor():
    """The justfile recipe is the DEV path; `release.yml` is what actually builds
    the shipped MSI (the local recipe is arch-hardcoded, so CI open-codes it). It
    carried `--features test-helpers` until 2026-08-01, self-rationalised in its
    own comment as "unused by the shipping app" — so a green `just windows-ffi`
    would have proved nothing about the artifact users install."""
    # Shipped from the public-files tree since 2026-08-31 (release-integrity.md
    # § Release signing → *When a release workflow publishes* owns the port).
    # The curated public tree prunes that source directory and injects the file
    # at .github/workflows/, so resolve whichever location this tree has.
    source = (
        _REPO / "scripts" / "publish" / "public-files" / ".github" / "workflows" / "release.yml"
    )
    workflow_path = source if source.exists() else _REPO / ".github" / "workflows" / "release.yml"
    workflow = workflow_path.read_text(encoding="utf-8")
    offenders = [
        ln.strip()
        for ln in workflow.splitlines()
        if "fauna-ffi" in ln and "test-helpers" in ln and not ln.lstrip().startswith("#")
    ]
    assert not offenders, (
        "the release workflow builds fauna-ffi with `test-helpers`, so the shipped "
        "Windows MSI carries the e2e seams no matter what the local recipe does "
        f"(e2e-automation-surface-gating.md convention 15). Offending line(s): {offenders}"
    )


def test_windows_ffi_flavor_marker_records_the_feature_set_not_just_the_profile():
    """Both flavors write the same staging slot, so the marker is the ONLY thing
    standing between a warm tree and a wrong-flavor false-green: build test then
    production and cargo may not relink, leaving build-if-stale reading "fresh"
    over a seam-carrying .dll. The marker must therefore key on the feature set,
    not just the profile it keyed on before the split."""
    body = _recipe_body("_windows-ffi-flavor")
    assert 'FLAVOR_MARKER' in body and '"$RID:$PROFILE:$FEATURES"' in body, (
        "the .ffi-flavor marker no longer records the RID AND FEATURE set — a "
        "profile-only (or RID-less) marker reads 'fresh' across a flavor switch at "
        "the same profile and stages the other flavor's .dll (the deterministic "
        "wrong-flavor false-green the android recipe's comment describes)"
    )


# ── the apple leg (closed 2026-08-02) ────────────────────────────────────────
#
# Apple has windows' problem twice over: TWO staging axes over ONE fixed path.
# `Package.swift`'s binaryTarget path is fixed, so the full 5-slice flavor
# (`apple-ffi*`) and the host-only 1-slice flavor (`apple-ffi-host*`) already
# shared `FaunaFFI.xcframework` behind a `.ffi-flavor` marker; the feature set is
# now a third value in that same marker. Both public recipes delegate to a shared
# `_apple-ffi-*-flavor` impl, exactly as windows and android do.


def test_apple_production_ffi_recipes_do_not_build_the_seams():
    """`just apple-ffi` is what device/CI builds link and `just apple-ffi-host
    release` is what `mac-release` / `mac-app` / `mac-dmg` + the installer
    `build.sh` package — so neither may name the feature anywhere in its chain."""
    chain = (
        _recipe_body("apple-ffi")
        + _recipe_body("apple-ffi-host")
        + _recipe_body("_apple-ffi-flavor")
        + _recipe_body("_apple-ffi-host-flavor")
    )
    build_lines = [
        ln for ln in chain.splitlines() if "cargo build" in ln or "cargo rustc" in ln
    ]
    assert build_lines, "no cargo build/rustc line found in the apple-ffi chain"
    for line in build_lines:
        assert "test-helpers" not in line, (
            "an apple-ffi cargo line hard-codes `test-helpers`, so the seams ship in "
            "the FaunaFFI.xcframework that mac-release/mac-app/mac-dmg package and "
            f"every iOS device build links (e2e-automation-surface-gating.md convention 15). Offending line: {line.strip()}"
        )
    assert '_apple-ffi-flavor ""' in _recipe_body("apple-ffi"), (
        "apple-ffi must delegate to the shared impl with an EMPTY feature set; "
        "that empty string is the production flavor"
    )
    assert '_apple-ffi-host-flavor "{{config}}" ""' in _recipe_body("apple-ffi-host"), (
        "apple-ffi-host must delegate to the shared impl with an EMPTY feature set"
    )


def test_apple_test_ffi_recipes_exist_and_carry_the_seams():
    """The e2e flavors have to exist, or there is nowhere for the feature to live
    — the gap that kept all three native recipes on `test-helpers` until
    2026-08-01. `fauna-ffi`'s manifest cited an `apple-ffi-test` build for months
    while no such recipe existed."""
    assert '_apple-ffi-flavor "test-helpers"' in _recipe_body("apple-ffi-test"), (
        "apple-ffi-test must build WITH `test-helpers` — it is what the iOS e2e "
        "path builds, and the simulator app's `#if DEBUG` TestAgent calls the seams"
    )
    assert '_apple-ffi-host-flavor "{{config}}" "test-helpers"' in _recipe_body(
        "apple-ffi-host-test"
    ), (
        "apple-ffi-host-test must build WITH `test-helpers` — it feeds swift-test "
        "and mac-debug, whose DEBUG Swift compiles the seam call sites"
    )


def test_apple_debug_and_release_consume_the_matching_ffi_flavor():
    """A DEBUG Swift build must never be handed production bindings (its
    `#if DEBUG` TestAgent, in-process automation server and
    `ConversationsTestInject` would not compile) and a release build must never
    be handed test ones."""
    for debug_recipe in ("swift-test", "mac-debug"):
        assert "apple-ffi-host-test" in _recipe_header(debug_recipe), (
            f"{debug_recipe} must depend on apple-ffi-host-test — it builds Swift in "
            "DEBUG, which compiles the seam call sites"
        )
    release_deps = _recipe_header("mac-release")
    assert "apple-ffi-host" in release_deps and "apple-ffi-host-test" not in release_deps, (
        "mac-release must depend on the PRODUCTION apple-ffi-host flavor — it is the "
        "recipe that builds the shipped macOS binary"
    )


def test_apple_ffi_flavor_marker_records_the_feature_set_not_just_the_flavor():
    """Both feature flavors write the same staging slot, so the marker is the ONLY
    thing standing between a warm tree and a wrong-flavor false-green: build test
    then production and cargo may not relink, leaving build-if-stale reading
    "fresh" over a seam-carrying xcframework. The marker must therefore key on the
    feature set, not just the `full`/`host`/`host-release` flavor it keyed on
    before the split.

    ⚠ Assert the `:$FEATURES` SUFFIX, never a whole literal. The flavor half
    gained a second axis on 2026-08-22 — the slice shape, `full-3slice` vs
    `full-5slice`, when the watchOS slices became opt-in — and a pin spelling the
    whole marker out goes red on a change that strengthens exactly the property
    it guards. `test_apple_ffi_watch_slices_are_opt_in.py` owns the shape half."""
    full = _recipe_body("_apple-ffi-flavor")
    host = _recipe_body("_apple-ffi-host-flavor")
    assert '"$FLAVOR:$FEATURES"' in full, (
        "the .ffi-flavor marker written by _apple-ffi-flavor no longer records the "
        "FEATURE set — a flavor-only marker reads 'fresh' across a production↔test "
        "switch and stages the other flavor's xcframework"
    )
    assert '"$FLAVOR:$FEATURES"' in host, (
        "the .ffi-flavor marker written by _apple-ffi-host-flavor no longer records "
        "the FEATURE set (same wrong-flavor false-green as above)"
    )


def test_production_apple_bindgen_asserts_zero_seams_in_its_own_output():
    """The self-maintaining artifact witness: every SHIPPING bindgen flavor greps
    the tree it just generated and fails if a `*ForTest` export survived, so a NEW
    ungated seam is caught with no list for anyone to update.

    ⚠ The condition must be *"not the TEST flavor"*, never *"the production
    flavor"*. `store-safe` (the App-Store escape hatch, 2026-08-15) is a third
    flavor and every bit as shippable, so the older `[ -z "$FEATURES" ]` form
    exempted the one artifact an App Store review actually receives. The recipe
    was strengthened when store-safe landed and this pin was not: it went on
    asserting the retired form and had been RED on main ever since — six days,
    unnoticed, because tier_1 pytest is in no merge gate (the same shape as the
    2026-08-10 rule-(b) false positive, and the same lesson: a permanently-red
    security pin is indistinguishable from one nobody runs).
    """
    body = _recipe_body("_apple-ffi-bindgen")
    assert "ForTest" in body, (
        "_apple-ffi-bindgen lost its seam assertion — a newly added ungated seam "
        "would ship silently in the next mac-release"
    )
    assert 'if [ "$FEATURES" != "test-helpers" ]' in body, (
        "_apple-ffi-bindgen's seam assertion no longer guards every SHIPPING "
        "flavor — it must fire on anything that is not the test flavor, so "
        "`store-safe` (the App-Store build) is covered too"
    )


def test_apple_bindgen_wipes_both_staging_slots_before_regenerating():
    """uniffi-bindgen only ever WRITES. A seam-carrying `.swift` from a
    test-flavored run would otherwise survive a production regeneration that no
    longer emits it — the whole point of the split, defeated by a stale file.
    Apple has TWO slots to clear (windows had one): the bindgen out-dir, and the
    `FaunaFFISwift/Sources` copies the SPM target actually compiles."""
    body = _recipe_body("_apple-ffi-bindgen")
    assert "rm -rf apps/fauna-apple/generated/" in body, (
        "_apple-ffi-bindgen no longer wipes its out-dir before regenerating"
    )
    assert "! -name 'FFICompat.swift' -delete" in body, (
        "_apple-ffi-bindgen no longer drops the previous generated copies from "
        "FaunaFFISwift/Sources — a namespace file the production flavor stops "
        "emitting would survive there and keep feeding seams to the Swift compiler "
        "(FFICompat.swift is hand-written + tracked and must be preserved)"
    )


# ── the Go-bridge leg: the atproto e2e seams ─────────────────────────────────
#
# Not an FFI flavor, but the same convention and the same failure mode: surfaces
# that must exist only in test builds, gated at compile time rather than by a
# runtime env var. Two tags, one e2e flavor:
#
#   * `fauna_e2e_seize` — the hostile-rotation seam. It signs a PLC operation
#     with a live per-user rotation key, so an ungated one would ship "seize any
#     identity this deployment hosts" inside every production nest image.
#   * `fauna_e2e_fixtures` (2026-09-13) — the three harness redirect seams
#     (fake PLC directory, fake DNS, proxy fixtures). Exempted on 2026-08-02 as
#     "redirecting without adding a capability"; the permission-set chain made
#     the DNS redirect its root of trust the next day and the fixtures seam a
#     guard bypass for the proof fetch, so they are gated on the seize shape now
#     (e2e-automation-surface-gating.md § Implementation status today → the Go
#     bridges' leg).
#
# The production recipes witness both on the ARTIFACT: the seize seam by its
# flag name (the whole reachable surface), the redirect seams by the env-var
# names they read — a production twin never spells one.

_SEIZE_TAG = "fauna_e2e_seize"
_FIXTURES_TAG = "fauna_e2e_fixtures"
_E2E_TAGS = f"{_SEIZE_TAG},{_FIXTURES_TAG}"
_BRIDGES = _REPO / "bins" / "fauna-bridges"
_BRIDGE_CMD = _BRIDGES / "cmd" / "fauna-atproto-bridge"
_ATPROTOID = _BRIDGES / "internal" / "atprotoid"
#: Every literal the production recipes grep their own binary for, and the e2e
#: recipes grep for the presence of. The seize flag name plus the three env-var
#: names; a fourth seam adds its variable here AND to the recipes' `for lit in`
#: line (the recipe pin below cross-checks the two lists).
_BRIDGE_SEAM_LITERALS = (
    "seize-did",
    "FAUNA_ATPROTO_FAKE_DNS_URL",
    "FAUNA_ATPROTO_PLC_DIRECTORY_URL",
    "FAUNA_ATPROTO_PROXY_FIXTURES",
)
#: (real file, twin file, the same-signature fns both must define) — the
#: redirect seams' real-plus-twin pairs.
_FIXTURE_SEAM_PAIRS = (
    (_ATPROTOID / "resolve_seam.go", _ATPROTOID / "resolve_seam_absent.go", ("TXTResolverFromEnv",)),
    (_ATPROTOID / "directory_seam.go", _ATPROTOID / "directory_seam_absent.go", ("PLCDirectoryBaseURL",)),
    (_BRIDGE_CMD / "fixtures_seam.go", _BRIDGE_CMD / "fixtures_seam_absent.go", ("proxyFixtureWrap", "proofFetcherFromEnv")),
)
_ATPROTO_SEAM_LITERAL = re.compile(r'"FAUNA_ATPROTO_[A-Z_]+"')


def _strip_shell_comments(body: str) -> str:
    """Recipe body minus its `#` comment lines — so an assertion about what a
    recipe DOES is not satisfied (or defeated) by prose about what it must not
    do. The production recipe's own comment names the tags on purpose."""
    return "\n".join(
        line for line in body.splitlines() if not line.lstrip().startswith("#")
    )


def test_the_seize_seam_carries_its_build_tag_and_its_absent_twin():
    """Both halves of the gate must exist: the real implementation under the tag,
    and a same-signature no-op under its negation. Without the twin the
    production build stops compiling, which is the failure mode that tempts the
    next session into an env-var gate instead."""
    real = (_BRIDGE_CMD / "seize.go").read_text(encoding="utf-8")
    twin = (_BRIDGE_CMD / "seize_absent.go").read_text(encoding="utf-8")
    assert real.startswith(f"//go:build {_SEIZE_TAG}\n"), (
        "seize.go must lead with its build tag — a stray edit that drops it "
        "compiles the seizure capability into every production bridge"
    )
    assert twin.startswith(f"//go:build !{_SEIZE_TAG}\n"), (
        "seize_absent.go must lead with the NEGATED tag"
    )
    for fn in ("registerSeizeFlags", "maybeRunSeize"):
        assert fn in real and fn in twin, (
            f"{fn} must exist in both flavors — the twin is what keeps the "
            "production build compiling with the seam absent"
        )


@pytest.mark.parametrize("real_path, twin_path, fns", _FIXTURE_SEAM_PAIRS, ids=lambda v: v.name if isinstance(v, Path) else "")
def test_each_redirect_seam_carries_the_fixtures_tag_and_its_absent_twin(real_path, twin_path, fns):
    """The seize shape, applied to the redirect seams: the real reader under
    `fauna_e2e_fixtures`, a same-signature twin under its negation, and the
    twin must not spell the variable it is inert to — the production binary
    must carry no literal for the recipe witness to find."""
    real = real_path.read_text(encoding="utf-8")
    twin = twin_path.read_text(encoding="utf-8")
    assert real.startswith(f"//go:build {_FIXTURES_TAG}\n"), (
        f"{real_path.name} must lead with the fixtures tag — dropping it compiles "
        "the harness redirect into every production bridge"
    )
    assert twin.startswith(f"//go:build !{_FIXTURES_TAG}\n"), (
        f"{twin_path.name} must lead with the NEGATED fixtures tag"
    )
    for fn in fns:
        assert f"func {fn}(" in real and f"func {fn}(" in twin, (
            f"{fn} must exist in both flavors — the twin keeps the production "
            "build compiling with the seam absent"
        )
    assert _ATPROTO_SEAM_LITERAL.search(real), (
        f"{real_path.name} is the flavor that READS the variable; it must name it"
    )
    assert not _ATPROTO_SEAM_LITERAL.search(twin), (
        f"{twin_path.name} spells a FAUNA_ATPROTO_* literal — the production twin "
        "must carry none, or the artifact witness finds the name in a binary that "
        "never reads it and the gate becomes unverifiable"
    )


def test_no_ungated_fauna_atproto_literal_in_the_bridges():
    """The census the 2026-07-22 reviewer asked for, as a gate: every non-test
    Go file under bins/fauna-bridges that spells a `"FAUNA_ATPROTO_…"` string
    literal must lead with the fixtures tag. A fourth redirect seam therefore
    cannot join the plane ungated — the way the first three sat ungated for
    seven weeks after the chain they redirect became a trust root."""
    offenders = []
    for path in sorted(_BRIDGES.rglob("*.go")):
        if path.name.endswith("_test.go"):
            continue
        code = path.read_text(encoding="utf-8")
        if not _ATPROTO_SEAM_LITERAL.search(code):
            continue
        if not code.startswith(f"//go:build {_FIXTURES_TAG}\n"):
            offenders.append(str(path.relative_to(_REPO)))
    assert not offenders, (
        "FAUNA_ATPROTO_* read outside the fixtures tag (convention 15 — gate it "
        f"on the *_seam.go / *_seam_absent.go shape): {offenders}"
    )
    # And the pair table above is the complete set of tagged files, so a new
    # seam file is added to it (and thereby to the twin/literal checks).
    tagged = sorted(
        str(p.relative_to(_REPO)) for p in _BRIDGES.rglob("*.go")
        if not p.name.endswith("_test.go")
        and p.read_text(encoding="utf-8").startswith(f"//go:build {_FIXTURES_TAG}\n")
    )
    assert tagged == sorted(str(real.relative_to(_REPO)) for real, _, _ in _FIXTURE_SEAM_PAIRS), (
        f"fixtures-tagged files {tagged} != the pinned pair table — add the new "
        "seam (and its twin) to _FIXTURE_SEAM_PAIRS"
    )


def test_only_the_e2e_recipe_sets_the_bridge_tags():
    """The production recipe and the Dockerfile must never pass either tag. This
    is the leg that actually failed on windows (`release.yml` open-coded its own
    FFI build with `--features test-helpers` and rode the seams into every
    installed MSI), so it is asserted on every build path that produces a
    shipped binary, not only on the justfile recipe a developer reads."""
    prod = _strip_shell_comments(_recipe_body("atproto-bridge-build"))
    e2e = _recipe_body("atproto-bridge-build-e2e")
    for tag in (_SEIZE_TAG, _FIXTURES_TAG):
        assert tag not in prod, (
            f"atproto-bridge-build must not pass {tag} — it builds the "
            "production bridge flavor"
        )
    assert f"-tags {_E2E_TAGS}" in e2e, (
        f"atproto-bridge-build-e2e must pass exactly `-tags {_E2E_TAGS}`; a "
        "missing tag leaves that seam's tests running against a binary that "
        "compiled its twin, and the e2e flavor is then part-production"
    )
    assert "-o fauna-atproto-bridge-e2e" in e2e, (
        "the e2e flavor must write its own path — sharing the production slot is "
        "how a warm tree serves a test binary to a release step"
    )
    dockerfile = (_REPO / "Dockerfile").read_text(encoding="utf-8")
    for tag in (_SEIZE_TAG, _FIXTURES_TAG):
        assert tag not in dockerfile, (
            "the Dockerfile builds the shipped nest image's bridge; it must never "
            f"pass {tag}"
        )


def _assert_two_way_artifact_witness(prod: str, e2e: str, prod_name: str, e2e_name: str) -> None:
    """Text analysis proves what the recipes SAY; this pins the recipes that
    prove what the binary IS: the production recipe greps its own artifact for
    every seam literal and fails on presence, the e2e recipe fails on absence."""
    assert "grep -a -q" in prod and "exit 1" in prod, (
        f"{prod_name} lost its artifact witness — the tag discipline would then "
        "rest entirely on nobody mistyping a recipe"
    )
    assert "if ! grep -a -q" in e2e, (
        f"{e2e_name} must assert the seams ARE present: a gate that is never "
        "verified in the positive direction is indistinguishable from a tag "
        "nothing honours, and the production witness would pass vacuously"
    )
    for literal in _BRIDGE_SEAM_LITERALS:
        assert literal in _strip_shell_comments(prod), (
            f"{prod_name} does not grep its artifact for {literal!r}"
        )
        assert literal in _strip_shell_comments(e2e), (
            f"{e2e_name} does not assert {literal!r} is present in the e2e flavor"
        )


def test_the_production_bridge_recipe_witnesses_its_own_artifact():
    _assert_two_way_artifact_witness(
        _recipe_body("atproto-bridge-build"),
        _recipe_body("atproto-bridge-build-e2e"),
        "atproto-bridge-build",
        "atproto-bridge-build-e2e",
    )


# ── the same discipline on the windows gnullvm-cgo twins ─────────────────────
#
# The windows atproto bridge is a SEPARATE build path (Go's cgo cannot link the
# MSVC fauna_ffi.dll, so it goes through `_windows-go-cgo-build`'s gnullvm slice).
# A separate path is exactly where a seam rides into a shipped binary unnoticed —
# the release.yml `--features test-helpers` MSI in this module's docstring was
# precisely that failure — so the windows recipes carry the same two-way witness
# as the Unix pair, and it is pinned here rather than resting on Windows being the
# only machine that ever runs them.


def test_only_the_windows_e2e_recipe_sets_the_bridge_tags():
    prod = _strip_shell_comments(_recipe_body("windows-atproto-bridge-build"))
    e2e = _recipe_body("windows-atproto-bridge-build-e2e")
    for tag in (_SEIZE_TAG, _FIXTURES_TAG):
        assert tag not in prod, (
            f"windows-atproto-bridge-build must not pass {tag} — it builds "
            "the production bridge flavor"
        )
    assert f'"{_E2E_TAGS}"' in e2e, (
        f"windows-atproto-bridge-build-e2e must pass exactly \"{_E2E_TAGS}\" "
        "through to _windows-go-cgo-build's go_tags argument; a missing tag makes "
        "the windows e2e flavor part-production"
    )
    assert '"fauna-atproto-bridge-e2e.exe"' in e2e, (
        "the windows e2e flavor must write its own path — sharing the production "
        "slot is how a warm tree serves a test binary to a release step"
    )


def test_the_windows_production_bridge_recipe_witnesses_its_own_artifact():
    _assert_two_way_artifact_witness(
        _recipe_body("windows-atproto-bridge-build"),
        _recipe_body("windows-atproto-bridge-build-e2e"),
        "windows-atproto-bridge-build",
        "windows-atproto-bridge-build-e2e",
    )


_FFI_CARGO_BUILD = re.compile(r"cargo build\b[^\n]*-p fauna-ffi")


def test_the_windows_go_cgo_recipes_share_one_feature_set():
    """Every windows Go/cgo recipe (mail-bridge, seal-helper, atproto bridge, and
    the go-TEST sibling `mail-bridge-test-win-cgo`) must reach `fauna-ffi`
    through the ONE shared cargo step, so the `--no-default-features --features
    labeler` line has a single home: `_windows-go-cgo-env`. The `go build`
    recipes reach it via `_windows-go-cgo-build`; the test recipe sources it
    directly (it runs `go test`, not `go build -o`).

    This pins the fix for a drift that actually shipped: the flags were
    copy-pasted per recipe and silently diverged from `mail-bridge-ffi` for weeks
    (build-system.md § Generated-file parity gates — the 2026-07-08 miss that
    produced a labeler-less windows MDA *and* seal-helper, caught by a build
    failure rather than a gate). A second `cargo build … -p fauna-ffi` appearing
    in any caller is that drift re-opening.
    """
    shared = _strip_shell_comments(_recipe_body("_windows-go-cgo-env"))
    assert "--no-default-features --features labeler" in shared, (
        "_windows-go-cgo-env is the single home of the windows gnullvm feature "
        "set; it must stay in lockstep with mail-bridge-ffi"
    )
    callers = {
        "_windows-go-cgo-build": "_windows-go-cgo-env",
        "mail-bridge-test-win-cgo": "_windows-go-cgo-env",
        "windows-mail-bridge-build": "_windows-go-cgo-build",
        "windows-seal-helper-build": "_windows-go-cgo-build",
        "windows-atproto-bridge-build": "_windows-go-cgo-build",
        "windows-atproto-bridge-build-e2e": "_windows-go-cgo-build",
    }
    for caller, via in callers.items():
        body = _strip_shell_comments(_recipe_body(caller))
        assert via in body, (
            f"{caller} must reach fauna-ffi through {via}, not its own "
            "cargo invocation"
        )
        assert not _FFI_CARGO_BUILD.search(body), (
            f"{caller} re-opened the copy-pasted fauna-ffi build the shared "
            "recipe exists to collapse"
        )


# ── the C# arm: every FAUNA_E2E_* read is compile-gated ──────────────────────
#
# Convention 15's C# mechanism is `#if DEBUG` (+ the opt-in `FAUNA_E2E_AGENT`
# flavor for the one automatable Release build, tests/platform/windows/
# test_installer.py). The FFI legs above gate the Rust *seams* the windows
# artifact links; these are reads in the **C# app source**, which no FFI flavor
# touches — `just windows-release` compiles them into the shipped MSI as written.
#
# Measured 2026-08-10, before the fix: 21 reads, 5 gated, **16 ungated** across 8
# env names. The three sharpest were not mode toggles but REDIRECTS:
#   * `FAUNA_E2E_CREDENTIAL_DIR` in `AccountReauth.ConfirmActivationAsync` —
#     setting it and writing "approve" into {dir}/reauth-result REPLACED the
#     Windows Hello verification with a file read: a re-auth bypass.
#   * the same var in `CredentialStore.Build` — relocates the whole credential
#     store (where the identity secret is read AND written) out of Credential
#     Manager into an attacker-named directory.
#   * `FAUNA_E2E_AGENT_LOG` in `E2eTrace` — a path taken verbatim, i.e. an
#     append-anywhere primitive at app privilege.
# Severity is the payload's, not the mechanism's — the same lesson the
# `FAUNA_E2E_SYNC_PIPE` disposition records one section above.
#
# The fix funnels all of them through ONE gated file with same-signature
# production twins (`FaunaApp.Core/Services/E2eEnv.cs`), so the boundary is a
# single reviewable surface instead of sixteen chances to forget a directive.

_WIN_APP = _REPO / "apps" / "fauna-windows" / "FaunaApp"
_E2E_ENV_FILE = _WIN_APP / "FaunaApp.Core" / "Services" / "E2eEnv.cs"
_CS_GATE = "DEBUG || FAUNA_E2E_AGENT"


def _win_app_sources():
    """Every hand-written C# source in the windows app (tests and the generated
    UniFFI bindings excluded — neither ships in the MSI's app code)."""
    out = []
    for p in sorted(_WIN_APP.rglob("*.cs")):
        rel = p.relative_to(_REPO).as_posix()
        if any(seg in rel for seg in ("/FaunaApp.Tests/", "/Generated/", "/obj/", "/bin/")):
            continue
        out.append((rel, p))
    return out


#: Harness env names that must not appear in unconditionally-compiled C#.
#
# ⚠ The list is NOT just the `FAUNA_E2E_` prefix, and that is the lesson of row
# 212: `FAUNA_KEYRING_APP` sat ungated in `CredentialStore.cs` for the whole
# life of this pin *because* the pin matched on the prefix, so the 2026-08-11
# sweep that gated the rest walked straight past it. A harness variable is
# defined by who sets it, not by how it is spelled — add any new one here.
_HARNESS_ENV_PATTERN = re.compile(r'"(FAUNA_E2E_[A-Z_]+|FAUNA_KEYRING_APP)"')


def _ungated_e2e_literals(path):
    """Harness env-var literals in code the C# preprocessor keeps unconditionally.

    Tracks the `#if` stack rather than grepping, because the question "does this
    ship" is a preprocessor question. Comments are skipped: a doc comment naming
    the variable is documentation, not a read.
    """
    hits, stack = [], []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        s = line.strip()
        if s.startswith("#if "):
            stack.append(s[4:].strip())
            continue
        if s.startswith("#endif"):
            if stack:
                stack.pop()
            continue
        if s.startswith("#el"):  # #else / #elif — still inside the conditional
            continue
        if stack or s.startswith("//") or s.startswith("///"):
            continue
        code = line.split("//")[0]
        for m in _HARNESS_ENV_PATTERN.finditer(code):
            hits.append((n, m.group(1)))
    return hits


def test_the_windows_app_source_glob_is_not_empty():
    """Guard against the vacuous green: a glob that matches nothing makes every
    assertion below pass while checking exactly zero source files."""
    sources = _win_app_sources()
    assert len(sources) > 100, (
        f"only {len(sources)} windows app sources found — the glob broke, and "
        "the convention-15 C# pins below are silently checking nothing"
    )
    assert _E2E_ENV_FILE.exists(), f"{_E2E_ENV_FILE} is gone — the single gated read site"


def test_no_windows_app_source_names_a_fauna_e2e_var_outside_the_gate():
    """The whole C# arm in one assertion: no `FAUNA_E2E_*` literal may sit in
    code a Release build compiles.

    Asserting on the LITERAL rather than on `GetEnvironmentVariable(...)` is
    deliberate and load-bearing — `InstanceSpawner` reached two of its reads
    through `private const string BridgeEnv = "FAUNA_E2E_BRIDGE"`, which a
    call-shaped pattern misses entirely (it missed them in this row's own first
    recon pass). The literal is also exactly what a `strings` of the shipped MSI
    would find, so this pin and the artifact witness measure the same thing.
    """
    offenders = []
    for rel, path in _win_app_sources():
        for n, name in _ungated_e2e_literals(path):
            offenders.append(f"{rel}:{n} {name}")
    assert not offenders, (
        "these harness env names are compiled into the shipped windows MSI "
        "(e2e-conventions.md convention 15 — the automation surface is compiled "
        "out of release artifacts). Read them through "
        "`FaunaApp.Core/Services/E2eEnv.cs`, whose production twins return null, "
        "so the call site's production arm is unchanged:\n  " + "\n  ".join(offenders)
    )


def test_e2e_env_reads_are_gated_and_have_production_twins():
    """`E2eEnv` is the one file allowed to name these variables, so its own gate
    is the whole boundary — and the `#else` twin arm must expose the SAME members.

    A member added to the gated arm and forgotten in the twin compiles fine in
    Debug and fails only in a Release build nobody runs locally, which is the
    one build that matters here.
    """
    text = _E2E_ENV_FILE.read_text(encoding="utf-8")
    assert f"#if {_CS_GATE}" in text, (
        f"E2eEnv.cs must gate its reads on `#if {_CS_GATE}` — convention 15's C# "
        "mechanism (e2e-conventions.md, the C# (windows) bullet)"
    )
    assert "#else" in text and "#endif" in text, (
        "E2eEnv.cs lost its `#else` production-twin arm; without it a release "
        "build cannot compile the call sites at all"
    )
    real, twin = text.split(f"#if {_CS_GATE}", 1)[1].split("#else", 1)
    twin = twin.split("#endif", 1)[0]
    member = re.compile(r"internal static string\?\s+(\w+)\s*=>")
    real_members, twin_members = set(member.findall(real)), set(member.findall(twin))
    assert real_members, "E2eEnv.cs's gated arm exposes no members — the pin would be vacuous"
    assert real_members == twin_members, (
        "E2eEnv's production twin arm does not mirror its gated arm; a release "
        f"build would not compile. Gated-only: {sorted(real_members - twin_members)}; "
        f"twin-only: {sorted(twin_members - real_members)}"
    )
    assert not re.search(r'"FAUNA_E2E_[A-Z_]+"', twin), (
        "the production twin arm names an env var — it must be a pure null twin"
    )


# ── the Rust arm: every FAUNA_E2E_* read is compile-gated ────────────────────
#
# The C# arm above is a class SWEEP —
# `_ungated_e2e_literals` walks every file, so a NEW harness literal anywhere
# in the app is caught with no list to update. The Rust arm earlier in this
# file (`_AUTOMATION_PORT_READS` / `_CREDENTIAL_REDIRECT_READS`) is a
# per-variable HAND-LIST instead, so a harness env read landing in a NEW
# libs/ or bins/ file has no test watching it at all — the exact class exploited: `conv_push_source` carried only a platform cfg for months and
# the read compiled into every native release artifact. This mirrors the C# sweep's shape, adapted for Rust's
# item/statement-level `#[cfg(...)]` gating (no `#if`/`#endif` block to
# track the way the C# preprocessor gives one).

#: `apps` is here because the two Rust apps (fauna-tui, fauna-desktop) ship
#: their own sources in release artifacts exactly as libs/ and bins/ do. The
#: root set once stopped at libs/+bins/ with no stated reason, and an ungated
#: `FAUNA_E2E_DOWNLOAD_DIR` read shipped in release tui for a month because no
#: witness of convention 15 read `apps/`. Non-Rust apps
#: contribute no `.rs` files; the C# app has its own sweep above.
_RUST_SCAN_ROOTS = ("libs", "bins", "apps")

#: The three arms convention 15 recognizes as a build-FLAVOR gate. A
#: platform-only cfg (`not(target_arch = "wasm32")` alone) does NOT count —
#: that is exactly the shape.
_RUST_FLAVOR_CFG_NEEDLES = (
    "debug_assertions",
    'feature = "e2e-agent"',
    'feature = "test-helpers"',
)


def _rust_scan_files():
    """Hand-written `.rs` files under libs/, bins/ and apps/ — never `tests/` (a
    separate cargo target that never ships in a release artifact) or
    generated output."""
    out = []
    for root in _RUST_SCAN_ROOTS:
        for p in sorted((_REPO / root).rglob("*.rs")):
            rel = p.relative_to(_REPO).as_posix()
            if "/tests/" in rel or "/target/" in rel or "/generated/" in rel:
                continue
            out.append((rel, p))
    return out


def _rust_harness_env_call_sites(text):
    """(lineno, var_name) for every harness env-var literal that is the
    argument of an actual `env::var`/`env::var_os` call on an
    unconditionally-compiled line — never a doc comment merely naming it
    (`_HARNESS_ENV_PATTERN`, shared with the C# arm above, is checked against
    the code with `//` comments stripped), and never inside a
    `#[cfg(test)]` module (test code never ships).

    Requiring `env::var` on the SAME line — unlike the C# arm's whole-line
    literal scan — is deliberate: `AGENT_BIN_ENV`/`E2E_PIPE_ENV` hold these
    two literals in a plain, always-compiled `pub const` and are read only
    through that NAME (never the literal again) at their call sites; that
    indirection is already covered by the sync-agent pin gate section earlier
    in this file (feature-forwarding, not a literal scan), and flagging the
    const's own definition line here would be a false positive on
    already-correct code.
    """
    hits = []
    lines = text.splitlines()
    for n, line in enumerate(lines, 1):
        if line.strip().startswith("//"):
            continue
        code = line.split("//")[0]
        if "env::var" not in code:
            continue
        if _line_is_in_a_cfg_test_module(text, n):
            continue
        for m in _HARNESS_ENV_PATTERN.finditer(code):
            hits.append((n, m.group(1)))
    return hits


def test_rust_harness_env_sweep_is_not_vacuous():
    """Guard the guard: a broken glob or pattern would let the assertion
    below pass by finding nothing at all."""
    per_root = dict.fromkeys(_RUST_SCAN_ROOTS, 0)
    for rel, path in _rust_scan_files():
        text = path.read_text(encoding="utf-8", errors="replace")
        per_root[rel.split("/", 1)[0]] += len(_rust_harness_env_call_sites(text))
    # Floors per root, not one total: the apps leg alone could silently stop
    # matching while libs/ kept the sum above a combined floor.
    assert per_root["libs"] + per_root["bins"] >= 7, (
        f"the Rust harness-env sweep found only {per_root} call site(s) — "
        "expected at least 7 across libs/+bins/ (trust.rs x1, "
        "fauna-credential-store x3, always_resident.rs x2, "
        "fauna-client-conversations x1); the glob or pattern likely broke and "
        "the test below is checking nothing"
    )
    assert per_root["apps"] >= 10, (
        f"the Rust harness-env sweep found only {per_root['apps']} call site(s) "
        "under apps/ — expected at least 10 (fauna-desktop's agent start, "
        "e2e predicate and download-dir readers; fauna-tui's agent port, e2e "
        "predicate and download dir); the apps leg went vacuous"
    )


def _enclosing_scope_cfg_attrs(lines: list[str], i: int) -> str:
    """Fallback for `_cfg_attrs_above`: when the immediate contiguous prelude
    above line `i` (0-based, the call's own line) carries no gate, climb
    outward by brace depth to the nearest enclosing `fn`/`impl`/`mod` header
    and return ITS attribute stack instead.

    `_cfg_attrs_above` alone misses a fn-level gate whenever a *balanced*
    inner block sits directly above the call — e.g. `cred_file_dir`'s
    `#[cfg(test)] if let Some(dir) = … { … }` precheck: its closing `}` sits
    directly above the harness-env read, so the tight-prelude walk stops
    there and never sees the fn's own
    `#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]` three lines
    further up . Brace-depth climbing — not
    merely continuing past a bare `}` — is required: continuing past `}`
    unconditionally would also sweep a PRECEDING SIBLING statement's
    unrelated `#[cfg(...)]` into `preceding` and manufacture a false gate.
    This walk only stops at a line whose brace is genuinely unmatched
    relative to everything scanned so far (balanced inner/sibling blocks
    net to zero and are climbed straight through), which is exactly the
    block that structurally encloses `i`.

    Deliberately kept separate from `_cfg_attrs_above` rather than folded
    into it: that function's tight prelude is what makes it safe for the
    fn-DECLARATION callers elsewhere in this file (a doc comment merely
    mentioning the feature name must not count ) — those sites
    already start AT the fn line, so this fallback would never fire for
    them and is not wired in there.

    The unmatched `{` need not share a line with its header: a multi-line
    parameter list or `where` clause leaves it on a bare `) {` / `{` line
    (`start_test_agent_if_enabled` in fauna-desktop, nine parameters).
    `_signature_header_above` walks from that line back through the
    signature to the header, so the attribute stack read is the fn's own.
    """
    depth = 0
    j = i - 1
    while j >= 0:
        code = lines[j].split("//", 1)[0]
        depth += code.count("}") - code.count("{")
        if depth < 0:
            if re.search(r"\b(fn|impl|mod)\b", code):
                return _cfg_attrs_above(lines, j)
            header = _signature_header_above(lines, j)
            if header is not None:
                return _cfg_attrs_above(lines, header)
            depth = 0
        j -= 1
    return ""


#: A line that OPENS an item signature — the header a multi-line parameter
#: list or `where` clause continues from.
_ITEM_HEADER_LINE = re.compile(
    r'^\s*(?:pub(?:\([^)]*\))?\s+)?'
    r'(?:(?:async|const|unsafe|default)\s+|extern\s+"[^"]*"\s+)*'
    r"(?:fn|impl|mod)\b"
)


def _signature_header_above(lines: list[str], j: int) -> int | None:
    """Index of the `fn`/`impl`/`mod` header whose signature line `j` (a line
    carrying an unmatched `{` but no header keyword) closes, or None.

    Walks upward through the signature's continuation lines and stops — with
    no header — at anything a signature cannot contain: a blank line, or a
    line ending a statement or block (`;`, `{`, `}`). That stop is what keeps
    a `match x {` or a closure's `|y| {` body from being mistaken for a
    header: the preceding statement's `;` ends the walk first.
    """
    k = j - 1
    while k >= 0:
        code = lines[k].split("//", 1)[0].rstrip()
        stripped = code.strip()
        if lines[k].strip().startswith("//"):  # a comment inside the parameter list
            k -= 1
            continue
        if not stripped or stripped.endswith((";", "{", "}")):
            return None
        if _ITEM_HEADER_LINE.match(code):
            return k
        k -= 1
    return None


def test_enclosing_scope_climb_crosses_a_multi_line_parameter_list():
    """The fallback must find a fn's gate when the fn's `{` closes a
    parameter list spanning several lines.

    fauna-desktop's `start_test_agent_if_enabled` is exactly this shape: its
    nine parameters put the body's `{` on a bare `) {` line, which carries no
    `fn` keyword, so a climb that inspected only the unmatched-brace line
    reset and walked on past the header — flagging two correctly-gated reads
    the day the sweep first reached `apps/`. The sibling-attribute half is
    pinned too: an unrelated `#[cfg(...)]` on a PRECEDING item must not be
    borrowed as the gate of an ungated fn with the same shape.
    """
    gated = """\
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
fn start_test_agent_if_enabled(
    application: &adw::Application,
    stack: Option<gtk::Stack>,
) {
    start_element_agent_once(application);

    let bridge_url = std::env::var("FAUNA_E2E_BRIDGE").ok();
}
""".splitlines()
    call = next(i for i, line in enumerate(gated) if "FAUNA_E2E_BRIDGE" in line)
    assert _cfg_attrs_above(gated, call) == "", "fixture must defeat the tight walk"
    attrs = _enclosing_scope_cfg_attrs(gated, call)
    assert 'feature = "e2e-agent"' in attrs, (
        "the climb stopped at the bare `) {` line instead of reaching the fn "
        f"header's attribute stack; got {attrs!r}"
    )

    ungated = """\
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn sibling() {}

fn download_dir(
    home: &Path,
) {
    let x = 1;

    let d = std::env::var("FAUNA_E2E_DOWNLOAD_DIR").ok();
}
""".splitlines()
    call = next(i for i, line in enumerate(ungated) if "FAUNA_E2E_DOWNLOAD_DIR" in line)
    assert "cfg" not in _enclosing_scope_cfg_attrs(ungated, call), (
        "the climb borrowed a preceding sibling's gate for an ungated fn"
    )


def test_no_rust_harness_env_read_compiles_unconditionally():
    """Every `env::var`/`env::var_os` call in libs/+bins/+apps/ naming a harness
    variable must sit under a compile-time flavor gate.

    `_cfg_attrs_above` — defined earlier in this file for the fn-decl checks
    — is applied to the CALL's own line here first, not to an enclosing
    `fn`'s decl line: this codebase gates both shapes (a `#[cfg(...)]` on the
    whole fn, as in `debounce_delay`, AND a `#[cfg(...)]` directly on the
    gated statement inside an otherwise-unconditional fn, as in
    `trust::trusted_escrow_holders`), and walking up from the call catches
    either when the attribute sits in the same blank-line-delimited prelude
    as the call. That tight walk stops at the first blank line or bare `}`
    though, so it misses a fn-level gate separated from the call by a
    *balanced* inner block — `cred_file_dir`'s `#[cfg(test)]` precheck is
    exactly this shape. `_enclosing_scope_cfg_attrs` is the
    fallback: only tried when the tight walk finds nothing, it climbs by
    brace depth to the nearest enclosing `fn`/`impl`/`mod` header and checks
    that header's own attribute stack instead.
    """
    offenders = []
    for rel, path in _rust_scan_files():
        text = path.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        for n, name in _rust_harness_env_call_sites(text):
            preceding = _cfg_attrs_above(lines, n - 1)
            gated = "#[cfg(" in preceding and any(
                needle in preceding for needle in _RUST_FLAVOR_CFG_NEEDLES
            )
            if not gated:
                preceding = _enclosing_scope_cfg_attrs(lines, n - 1)
                gated = "#[cfg(" in preceding and any(
                    needle in preceding for needle in _RUST_FLAVOR_CFG_NEEDLES
                )
            if not gated:
                offenders.append(f"{rel}:{n} {name}")
    assert not offenders, (
        "these harness env reads compile into every release artifact "
        "(e2e-automation-surface-gating.md convention 15 — the automation "
        "surface is compiled out of release artifacts). Gate the read (or "
        "its enclosing fn/statement) on `cfg(any(debug_assertions, "
        'feature = "e2e-agent"))` or `test-helpers` — a platform-only cfg '
        '(e.g. `not(target_arch = "wasm32")`) is not a flavor gate, and is '
        "the exact shape exploited:\n  " + "\n  ".join(offenders)
    )
