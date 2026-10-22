"""The warm-takes-no-slot fix for apple's host-only FFI + mac-debug builds —
the macOS/iOS leg of the cross-app "warm tree must not queue for a build slot"
track (build-system.md § Build/e2e slot locks; `test_e2e_binary_prebuild.py`
pins the same discipline for the nest/mail-bridge legs, which this file
mirrors for the macOS-only legs those tests cannot reach — no Xcode/Swift
toolchain on the other dev machines).

Before this: `_apple-ffi-host-flavor` (the shared implementation behind
`apple-ffi-host` and `apple-ffi-host-test`, which `mac-debug`/`swift-test`
take) ran its `cargo build -p fauna-ffi` unconditionally on every invocation,
and `mac-debug`'s own `swift build` line had no gate at all — every single
`mac-debug`/`swift-test` run queued for a machine-wide `build` slot even on a
fully warm tree. Text/structural analysis only, no real Xcode build needed to
prove the wiring exists (real-build verification: `just mac-debug` twice in a
row on a macOS box, confirmed both `build-if-stale` lines read `up-to-date` and
`build-slot.py --status` showed no slot touched).
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name *ARGS:` declaration alike. Duplicated from
    `test_e2e_binary_prebuild.py` per this suite's no-cross-import-between-
    test-files convention (small helpers get duplicated, not imported)."""
    lines = text.splitlines()
    header = re.compile(rf"^{re.escape(name)}(\s+\S+)*:")
    for i, ln in enumerate(lines):
        if header.match(ln):
            body = []
            for bl in lines[i + 1:]:
                if bl and not bl[:1].isspace():
                    break
                body.append(bl)
            return "\n".join(body)
    return ""


def test_apple_ffi_host_flavor_cargo_step_is_freshness_gated_outside_the_slot():
    """`_apple-ffi-host-flavor` (shared by `apple-ffi-host` and
    `apple-ffi-host-test`) must decide "is there work?" BEFORE queueing:
    build-if-stale outermost, {{slot_build}} inside it, cargo innermost —
    same shape as `mail-bridge-ffi`'s gate. --stamp, not the .a, must carry
    freshness: a cargo no-op leaves the .a untouched, so an artifact-keyed
    gate would go permanently stale on rebase mtime churn and queue on every
    warm run anyway."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    joined = " ".join(body.split())
    cargo_needle = "cargo build --locked -p fauna-ffi"
    assert cargo_needle in joined, "_apple-ffi-host-flavor must build fauna-ffi"
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "the cargo step must be gated by build-if-stale --stamp (a warm tree "
        "must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    cargo_pos = joined.index(cargo_needle, gate_pos)
    assert gate_pos < slot_pos < cargo_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} "
        "OUTSIDE cargo — a slot acquired before the freshness verdict queues "
        "a warm tree"
    )


def _expand_recipe_vars(body: str, expr: str) -> str:
    """`expr` with `$VAR`/`${VAR}` replaced by the recipe's own assignments,
    repeatedly (assignments reference each other). Used so a path assertion
    reads the SHAPE of the path rather than the spelling of one line: since
    2026-08-22 the stamp is feature-scoped through `$ARTIFACTS` rather than by a
    suffix in its own filename, and a literal substring check called that a
    regression when it is strictly stronger scoping."""
    assigns = dict(re.findall(r'^\s*(\w+)="([^"]*)"', body, re.M))
    for _ in range(5):
        before = expr
        expr = re.sub(r'\$\{?(\w+)\}?', lambda m: assigns.get(m.group(1), m.group(0)), expr)
        if expr == before:
            break
    return expr


def test_apple_ffi_host_flavor_cargo_stamp_is_scoped_by_features_not_just_config():
    """A cargo freshness stamp keyed only on libs/Cargo.lock/rust-toolchain.toml
    would read "fresh" the moment a caller switches BACK to a flavor last built a
    while ago, even though the artifact on disk right now holds the OTHER
    flavor's build. The stamp path must therefore carry the features axis, not
    just $PROFILE_DIR — however it gets there: a suffix in the filename, or (as
    now) the per-flavor `--artifact-dir` the stamp lives in."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    stamp_lines = [ln for ln in body.splitlines() if "CARGO_STAMP=" in ln]
    assert stamp_lines, "_apple-ffi-host-flavor must define a CARGO_STAMP path"
    # Expansion bottoms out at the recipe's own `features` parameter, whichever
    # route the path took to it (`$FEATURES` directly, or via `$ARTIFACTS`).
    resolved = _expand_recipe_vars(body, stamp_lines[0])
    assert "{{features}}" in resolved, (
        f"CARGO_STAMP must be scoped by the features axis (not just "
        f"$PROFILE_DIR/$CONFIG), or a flavor switch silently skips a needed "
        f"rebuild: {stamp_lines[0]!r} resolves to {resolved!r}"
    )


def _flavor_guard_block(body: str) -> str:
    """The `.ffi-flavor` mismatch branch: from the marker test to the line
    that closes its `{ … }` group (a lone `}` at the recipe's indent)."""
    lines = body.splitlines()
    start = next(i for i, ln in enumerate(lines)
                 if ".ffi-flavor" in ln and "||" in ln and "{" in ln)
    indent = len(lines[start]) - len(lines[start].lstrip())
    for j in range(start + 1, len(lines)):
        ln = lines[j]
        if ln.strip() == "}" and len(ln) - len(ln.lstrip()) == indent:
            return "\n".join(lines[start:j + 1])
    raise AssertionError("flavor-mismatch guard block is not closed")


def test_apple_ffi_host_flavor_guard_keeps_the_per_flavor_cargo_stamp():
    """A flavor switch must not cost a slotted cargo no-op (2026-10-21). The
    .a and `$CARGO_STAMP` both live under the per-flavor `$ARTIFACTS`
    (`test_apple_ffi_host_flavor_cargo_stamp_is_scoped_by_features_not_just_config`),
    so a fresh-reading stamp vouches for exactly this flavor's .a and can never
    serve the other flavor's code (convention 15's seam separation). Dropping it
    on every switch only bought a `build` slot per switch — which, inside an e2e
    lane hold, the warm pass's refuse mode turns into a released lane."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    artifacts = next(ln for ln in body.splitlines() if ln.strip().startswith("ARTIFACTS="))
    assert "{{features}}" in _expand_recipe_vars(body, artifacts), (
        "the stamp may stay only while $ARTIFACTS is per-flavor"
    )
    guard = _flavor_guard_block(body)
    assert "FaunaFFI.xcframework" in guard, "sanity: this must be the flavor guard"
    assert "CARGO_STAMP" not in guard, (
        "the flavor guard must keep the per-flavor cargo stamp: removing it "
        "makes every apple flavor switch queue for a build slot"
    )


def test_apple_ffi_host_flavor_switch_back_restores_its_stash_not_a_rebuild():
    """A switch back to a host flavor restores the clone its last bindgen
    stashed, mtimes kept, instead of re-running the bindgen under a slot — but
    only while the stash is the generation the bindgen stamp vouches for, and
    with the wipe standing otherwise. Every bindgen re-stashes."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    guard = " ".join(_flavor_guard_block(body).split())
    assert '[ ! "$BINDGEN_STAMP" -nt "$STASH/.stashed" ]' in guard, (
        "the stash may be restored only when it is no older than the bindgen stamp"
    )
    assert 'cp -pRc "$STASH/FaunaFFI.xcframework"' in guard and 'cp -pRc "$STASH/generated"' in guard, (
        "the restore must keep mtimes (cp -p), or mac-debug's gate rebuilds the app"
    )
    assert guard.count("rm -rf apps/fauna-apple/FaunaFFI.xcframework") == 2, (
        "the wipe must stand when no usable stash exists (and after a failed restore)"
    )
    joined = " ".join(body.split())
    gate = joined.index('--stamp "$BINDGEN_STAMP"')
    restash = joined.index('[ "$BINDGEN_STAMP" -nt "$STASH/.stashed" ]', gate)
    assert restash > gate and 'touch "$STASH.tmp/.stashed"' in joined[restash:], (
        "the stash must be refreshed after the bindgen gate whenever it ran"
    )


def test_mac_debug_swift_build_is_freshness_gated_outside_the_slot():
    """`mac-debug`'s `swift build --product FaunaMacOS` line had no gate at
    all — same defect class as android-debug's gradlew gap. Must be gated
    build-if-stale (outermost) -> {{slot_build}} -> swift build (innermost)."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    assert body, "mac-debug recipe not found"
    joined = " ".join(body.split())
    swift_needle = "swift build --package-path apps/fauna-apple --product FaunaMacOS"
    assert swift_needle in joined, "mac-debug must build the FaunaMacOS product"
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "mac-debug's swift build step must be gated by build-if-stale --stamp "
        "(a warm tree must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    swift_pos = joined.index(swift_needle, gate_pos)
    assert gate_pos < slot_pos < swift_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} "
        "OUTSIDE swift build — a slot acquired before the freshness verdict "
        "queues a warm tree"
    )


def test_mac_debug_source_watch_excludes_its_own_build_output_and_flavor_marker():
    """Two self-referential-staleness traps a naive `--source apps/fauna-apple`
    would fall into, both caught live while building this gate:
    (1) swiftpm's own `.build/` output dir — watching your own build output
    means the gate looks stale right after building.
    (2) `.ffi-flavor` — the `apple-ffi-host-test` prerequisite rewrites this
    file's mtime on EVERY invocation regardless of whether it did any real
    work (it is the one apps/fauna-apple file with no byte-comparing write,
    unlike the generated i18n/providers files beside it), so without this
    exclude a truly warm re-run still reads "stale" every time."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    assert body, "mac-debug recipe not found"
    joined = " ".join(body.split())
    assert "--source apps/fauna-apple" in joined
    assert "--exclude '*/.build/*'" in joined, (
        "must exclude swiftpm's own .build/ output — a source watching its "
        "own build output always looks stale right after building"
    )
    assert "--exclude '*/.ffi-flavor'" in joined, (
        "must exclude .ffi-flavor — it is rewritten unconditionally by the "
        "apple-ffi-host-test prerequisite on every invocation, so without "
        "this exclude the gate never reads up-to-date"
    )


@pytest.mark.parametrize("output", [
    "apps/fauna-apple/.xcode-build/x",
    "apps/fauna-apple/.ffi-cache.stamp",
    "apps/fauna-apple/FaunaFFI.xcframework/Info.plist",
    "apps/fauna-apple/generated/FaunaFFI.swift",
    "apps/fauna-apple/Fauna.xcodeproj/project.xcworkspace/xcuserdata/u.xcuserdatad/x",
    "apps/fauna-apple/.swiftpm/xcode/package.xcworkspace/x",
])
def test_mac_debug_source_watch_excludes_what_other_recipes_write(output):
    """`mac-debug`'s gate watches the package dir, so every output another
    recipe writes there must be excluded or a warm re-check reads stale and
    takes a `build` slot to prove nothing changed — the reason macOS stayed out
    of the e2e warm pass until 2026-10-21. Matched with build-if-stale's own
    matcher (`fnmatch` over the posix path)."""
    import fnmatch

    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    excludes = re.findall(r"--exclude '([^']+)'", body)
    assert any(fnmatch.fnmatch(output, pat) for pat in excludes), (
        f"{output} must be excluded from mac-debug's sources: {excludes}"
    )


@pytest.mark.parametrize("source", [
    "apps/fauna-apple/Package.swift",
    "apps/fauna-apple/Fauna-macOS/App.swift",
    "apps/fauna-apple/FaunaKit/Sources/FaunaKit/X.swift",
    "apps/fauna-apple/FaunaFFISwift/Sources/FaunaFFI.swift",
])
def test_mac_debug_source_watch_still_sees_the_app_and_its_ffi(source):
    """The other half: the Swift sources and the bindings SwiftPM compiles stay
    sources — `FaunaFFISwift/Sources/` is what carries an FFI change to the app,
    since every bindgen rewrites it."""
    import fnmatch

    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    excludes = re.findall(r"--exclude '([^']+)'", body)
    assert not any(fnmatch.fnmatch(source, pat) for pat in excludes), (
        f"{source} is a real input of the FaunaMacOS product and must stay watched"
    )


# ── The full flavor's dirty-tree path (2026-10-21): the cross-checkout cache
# answers only on a clean libs/, so a dirty but warm tree read stamps nowhere
# and took the build slot just to find nothing to do — under an e2e run's
# in-hold refuse mode, a lane release per re-check.


def _assignments(body: str, names) -> dict:
    out = {}
    for name in names:
        found = set(re.findall(rf'^\s*{name}="([^"]*)"\s*$', body, re.MULTILINE))
        assert len(found) == 1, f"{name} must be assigned exactly one way: {found}"
        out[name] = found.pop()
    return out


def test_full_flavor_dirty_tree_reads_its_stamps_before_the_slot():
    """`_apple-ffi-flavor`, with no cache key (a dirty tree or the cache off),
    checks this flavor's cargo stamp and then its bindgen stamp with
    `build-if-stale --check` — no slot — and exits 0 when both are fresh,
    before `{{slot_build}} just _apple-ffi-build-impl` is reached."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-flavor")
    assert body, "_apple-ffi-flavor recipe not found"
    joined = " ".join(body.split())
    fast = joined.index('if [ -z "$CACHE_KEY" ]; then')
    cargo_check = joined.index('build-if-stale.py --check -q --stamp "$CARGO_STAMP"', fast)
    bindgen_check = joined.index('build-if-stale.py --check -q --stamp "$BINDGEN_STAMP"', cargo_check)
    done = joined.index("exit 0", bindgen_check)
    slot = joined.index("{{slot_build}} just _apple-ffi-build-impl")
    assert fast < cargo_check < bindgen_check < done < slot, (
        "the stamp reads come first and decide the exit, holding no slot"
    )
    code = [ln for ln in body.splitlines() if not ln.strip().startswith("#")]
    code_joined = " ".join(" ".join(code).split())
    assert code_joined.count("{{slot_build}}") == 1, "the impl call is the recipe's one slot"
    window = joined[cargo_check:bindgen_check]
    for source in ("--source libs", "--source Cargo.lock", "--source rust-toolchain.toml"):
        assert source in window, f"the cargo stamp must be read against {source}"


def test_full_flavor_fast_path_and_impl_name_one_cargo_stamp_bindgen_stamp_and_stash():
    """Two recipes, one meaning: the impl writes what the fast path reads."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    names = ("LOCAL_ID", "CARGO_STAMP", "BINDGEN_STAMP", "STASH")
    flavor = _assignments(_recipe_body(text, "_apple-ffi-flavor"), names)
    impl = _assignments(_recipe_body(text, "_apple-ffi-build-impl"), names)
    assert flavor == impl, (flavor, impl)
    assert all(axis in flavor["LOCAL_ID"] for axis in ("FEATURES", "$SHAPE", "$PROFILE")), (
        "the stamps are per flavor — every discriminator the bindgen label carries"
    )


def test_full_flavor_impl_commits_the_cargo_stamp_only_after_every_slice_built():
    """Dropped before the first slice build and committed after the last one,
    at the pre-build instant, listing the slices — so a failed build leaves no
    stamp and the fast path reads SLICE_TRIPLES' own list, never a copy."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-build-impl")
    joined = " ".join(body.split())
    drop = joined.index('rm -f "$CARGO_STAMP"')
    # The slice builds are one loop over SLICE_TRIPLES; `done` closes it.
    loop = joined.index('for triple in "${SLICE_TRIPLES[@]}"; do')
    cargo = joined.index("cargo build --locked -p fauna-ffi", loop)
    loop_end = joined.index("done", cargo)
    commit = joined.index('printf \'%s\\n\' "${SLICE_LIBS[@]}" > "$CARGO_STAMP"')
    dated = joined.index('touch -r "$CARGO_STAMP.t0.$$" "$CARGO_STAMP"')
    bindgen = joined.index('--stamp "$BINDGEN_STAMP"')
    assert drop < loop < cargo < loop_end < commit < dated < bindgen


def test_full_flavor_stash_and_cache_stamp_follow_a_dirty_build():
    """The impl stashes every new bindgen generation (the host flavor's
    mechanism), and a build with no cache key drops `.ffi-cache.stamp`, whose
    key no longer names what is staged; the fast path's stash restore drops it
    too and keeps mtimes."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    impl = " ".join(_recipe_body(text, "_apple-ffi-build-impl").split())
    assert 'else # A dirty-tree build staged a generation no cache key names' in impl.replace("    ", " ")
    assert 'touch "$STASH.tmp/.stashed"' in impl
    flavor = " ".join(_recipe_body(text, "_apple-ffi-flavor").split())
    restore = flavor.index('[ ! "$BINDGEN_STAMP" -nt "$STASH/.stashed" ]')
    tail = flavor[restore:]
    assert 'cp -pRc "$STASH/FaunaFFI.xcframework"' in tail
    assert tail.index("rm -f apps/fauna-apple/.ffi-cache.stamp") < tail.index("exit 0")

