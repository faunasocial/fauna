"""The kids flavor's shared spine — structural pins.

`docs/goal/behavior/family-safety.md` § The account age band → the kids-app
bullet (the shape ratified 2026-09-25: item (3) the eligibility verdict, item (4)
the surface set and the floor, item (5) the shared-Rust mechanism), and
`docs/goal/architecture/dynamic-features.md` § Compile-time excision (the kids
floor as the second tier-0 application; one named complement per root).

The kids app is the android and ios apps built under a `kids` flavor. Its shared
half is three things, each pinned here so neither per-store leg re-derives it:

* **`kids-floor`** on the FFI root — composes `fauna_core::obligation::
  KIDS_CONTENT_FLOOR` (the four guardian categories at `block`) strictest-wins
  into every render face's guardian input. An addition, not an excision, so it
  sits in no `default`; Guardian Notify is never floored.
* **`kids-safe`** — the complement the kids build keeps, nested INSIDE
  `store-safe`, so there is still one list (the property
  `test_payments_excision_spine.py` pins for `store-safe` itself).
* **`kids_app_eligible`** — the one shared verdict the flavor keys sign-in on,
  exported on the UniFFI face and attached to the wasm `familyStatus` reply.

Pure text analysis of the manifest and sources — no build, no driver; the
behavior is pinned by the Rust tests beside each piece (`fauna-core`
obligation, `fauna-client-family` kids, `fauna-ffi` `kids_floor_tests`, the
last run with `--features kids-floor`). Shape copied from
`test_payments_excision_spine.py`.
"""

import re
import tomllib
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_FFI = _REPO / "libs" / "fauna-ffi"
_OBLIGATION = _REPO / "libs" / "fauna-core" / "src" / "obligation.rs"


def _features() -> dict[str, list[str]]:
    return tomllib.loads((_FFI / "Cargo.toml").read_text(encoding="utf-8"))["features"]


def _closure(features: dict[str, list[str]], root: str) -> set[str]:
    """Every entry `root` reaches through the manifest's own `[features]` graph."""
    reached: set[str] = set()
    stack = [root]
    while stack:
        for entry in features.get(stack.pop(), []):
            if entry not in reached:
                reached.add(entry)
                if entry in features:
                    stack.append(entry)
    return reached


def test_the_kids_floor_feature_exists_and_no_shipping_set_reaches_it():
    features = _features()
    assert "kids-floor" in features, "fauna-ffi lost its `kids-floor` feature"
    for shipping in ("default", "store-safe", "kids-safe"):
        assert "kids-floor" not in _closure(features, shipping), (
            f"fauna-ffi's `{shipping}` reaches `kids-floor`, so a non-kids build "
            "would render every labeled item at the kids floor"
        )


def test_the_kids_complement_is_named_once_inside_store_safe():
    """`store-safe = ["kids-safe", <the excised planes>]`: one list, two flavors.

    A surface the kids app keeps is added to `kids-safe` once and is in
    `store-safe` (and so `default`) by construction; an excised plane sits in
    `store-safe`'s own tail. A second copy of the kept set — on a recipe's
    command line, or a sibling feature — would rot invisibly, since both
    flavors still build.
    """
    features = _features()
    assert "kids-safe" in features, "fauna-ffi lost its `kids-safe` complement"
    kids = features["kids-safe"]
    store_safe = features["store-safe"]
    assert store_safe[0] == "kids-safe", (
        "`store-safe` must lead with `kids-safe` — the kids complement nests inside it"
    )
    tail = store_safe[1:]
    assert not set(tail) & set(kids), (
        f"a surface sits in both `kids-safe` and `store-safe`'s tail: {sorted(set(tail) & set(kids))}"
    )
    assert not set(tail) & _closure(features, "kids-safe"), (
        "`kids-safe` reaches an excised plane through another feature: "
        f"{sorted(set(tail) & _closure(features, 'kids-safe'))}"
    )
    for name, entries in features.items():
        if name != "kids-safe" and len(entries) > 3:
            assert sorted(entries) != sorted(kids), f"`{name}` is a second copy of `kids-safe`"


def test_the_kids_flavor_excises_the_ratified_planes():
    """The planes family-safety.md item (4) excises that have a feature today.

    Outbound link opening has none (links render as inert text in the kids
    flavor's own render conditions) and is the android/ios legs' to gate.
    """
    kids = _closure(_features(), "kids-safe") | set(_features()["kids-safe"])
    for plane in (
        "feed-manager", "feed-rules", "feed-badge",  # the feed
        "search-manager",  # discovery
        "mail-admin", "mail-import", "atproto-settings", "nostr-npub-confirm",  # bridges
        "web-content",  # web publishing
        "subscriptions-author", "payments", "zaps",  # monetization
        "connected-apps", "labeler-catalog",  # third-party anything
    ):
        assert plane not in kids, f"the kids complement keeps `{plane}`, an excised plane"


def test_the_floor_constant_is_the_four_guardian_categories_at_block():
    src = _OBLIGATION.read_text(encoding="utf-8")
    cats = re.search(r"pub const GUARDIAN_FLOOR_CATEGORIES: \[&str; 4\] = \[([^\]]*)\]", src)
    assert cats, "GUARDIAN_FLOOR_CATEGORIES moved or changed shape"
    names = re.findall(r'"([a-z]+)"', cats.group(1))
    assert sorted(names) == ["commercial", "nsfw", "phishing", "spam"]
    floor = re.search(
        r"pub const KIDS_CONTENT_FLOOR: ContentPolicy = ContentPolicy \{(.*?)\};", src, re.S
    )
    assert floor, "fauna-core lost KIDS_CONTENT_FLOOR"
    fields = dict(re.findall(r"(\w+): ContentFloor::(\w+)", floor.group(1)))
    assert fields == {name: "Block" for name in names}, (
        f"KIDS_CONTENT_FLOOR must floor every guardian category at Block: {fields}"
    )


def test_the_ffi_floor_is_keyed_on_the_feature_and_reaches_every_render_face():
    family = (_FFI / "src" / "family.rs").read_text(encoding="utf-8")
    assert re.search(
        r'#\[cfg\(all\(feature = "value-format", feature = "kids-floor"\)\)\]\s*'
        r"const COMPILED_CONTENT_FLOOR: Option<&ContentPolicy> =\s*"
        r"Some\(&fauna_core::obligation::KIDS_CONTENT_FLOOR\);",
        family,
    ), "fauna-ffi's compiled floor must be KIDS_CONTENT_FLOOR exactly under `kids-floor`"
    # Every function that composes a render verdict resolves its guardian input
    # through `render_guardian_policy` — a face that converted the policy itself
    # would render past the kids floor.
    for path in sorted((_FFI / "src").glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for body in re.split(r"\n\s*(?:pub(?:\([a-z]+\))? )?fn ", text)[1:]:
            if re.search(r"\brender_verdict_(?:composed|for_item)\(", body):
                assert "render_guardian_policy(" in body, (
                    f"{path.name}: `fn {body.split('(', 1)[0]}` composes a render "
                    "verdict without `render_guardian_policy`, skipping the kids floor"
                )
    # Guardian Notify is NOT floored (`content_notify` deliberately excluded).
    notify = re.search(r"pub fn guardian_enforced_categories\((.*?)\n\}", family, re.S)
    assert notify and "render_guardian_policy" not in notify.group(1)


# ── the ANDROID leg (2026-10-03): the build type, its FFI flavor, its staging ──
#
# `docs/goal/architecture/installers/android.md` § Goal (the fourth shipping
# build type) and § Store identity. These pin the STRUCTURE the android kids
# build stands on; the shell's own source split and the dex witness are pinned
# in the section after this one.

_ANDROID_GRADLE = _REPO / "apps/fauna-android/app/build.gradle.kts"
_JUSTFILE = _REPO / "justfile"


def _block(text: str, opener: str) -> str:
    """The brace-balanced block that `opener` starts (comments included)."""
    start = text.index(opener)
    depth = 0
    for i in range(text.index("{", start), len(text)):
        depth += {"{": 1, "}": -1}.get(text[i], 0)
        if depth == 0:
            return text[start : i + 1]
    raise AssertionError(f"unbalanced block after {opener!r}")


def _code(block: str) -> str:
    """`block` without its `//` comments, so a pin never matches prose."""
    return "\n".join(line.split("//", 1)[0] for line in block.splitlines())


def _recipe(name: str) -> str:
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:\n]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.M)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(0)


def test_the_android_kids_build_type_is_store_safe_under_its_own_identity():
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")
    kids = _code(_block(text, 'create("kids")'))
    assert 'initWith(getByName("storeSafe"))' in kids, (
        "the kids build type must initWith(storeSafe): the kids artifact is the "
        "store-safe one minus the kids-excised planes and nothing else"
    )
    assert 'buildConfigField("boolean", "KIDS", "true")' in kids
    assert "versionNameSuffix = null" in kids, (
        "the kids build type must clear the `-storesafe` suffix initWith copied — "
        "this artifact ships, on the train at the fleet version"
    )
    assert "applicationIdSuffix" not in kids, (
        "the kids listing is a different registry unit, never a suffix of the main id"
    )
    # A build type can only SUFFIX the application id, so the identity is set on
    # the variant.
    variant = _code(_block(text, 'onVariants(selector().withBuildType("kids"))'))
    assert 'applicationId.set("social.fauna.faunakids")' in variant, (
        "the kids variant lost its ratified store identity "
        "(installers/android.md § Store identity)"
    )
    # Every other build type states the constant false — a build type with no
    # KIDS field would not compile the shared render conditions at all.
    for name in ("debug", "release", 'create("storeSafe")', 'create("foss")'):
        block = _code(_block(text, name + " {" if "(" not in name else name))
        assert 'buildConfigField("boolean", "KIDS", "false")' in block, (
            f"build type {name} must set KIDS=false"
        )


def test_the_android_kids_source_set_takes_the_excised_twins():
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")
    kids = _code(_block(_block(text, "sourceSets {"), 'getByName("kids")'))
    for twin in ("src/noPayments/java", "src/noP2pShare/java", "src/noAgent/java"):
        assert f'java.srcDir("{twin}")' in kids, f"the kids source set lost `{twin}`"
    # Play-distributed (Families policy), so the REAL store-age twin.
    assert 'java.srcDir("src/storeAge/java")' in kids
    assert "noStoreAge" not in kids
    for lib in ("integrity", "age-signals"):
        assert re.search(rf'"kidsImplementation"\("com\.google\.android\.play:{lib}:', text), (
            f"the kids build type lost the Play `{lib}` library its store-age twin names"
        )


def test_the_android_kids_ffi_is_the_named_complement_plus_the_floor():
    assert '_android-ffi-flavor kids "kids-safe,kids-floor"' in _recipe("android-ffi-kids")
    flavor = _recipe("_android-ffi-flavor")
    assert (
        'FEATURE_FLAGS="--no-default-features --features kids-safe,kids-floor,file-provider-host"'
        in flavor
    ), (
        "_android-ffi-flavor no longer spells the kids flavor as the named complement "
        "plus the floor — a list on the command line would rot against `kids-safe`"
    )
    header = _recipe("android-kids").splitlines()[0]
    assert "android-ffi-kids" in header, "android-kids no longer takes the kids FFI flavor"
    assert "assembleKids" in _recipe("android-kids")


def test_the_android_kids_bindings_hold_the_shipping_seam_bar():
    """`kids` is a shipping flavor: no `*ForTest` export, production-tree witness."""
    bindgen = _recipe("_android-ffi-bindgen")
    assert "*/src/release/*|*/src/storeSafe/*|*/src/foss/*|*/src/kids/*)" in bindgen
    assert re.search(r"\*/src/kids/\*\)\s+FLAVOR_FLAG=--production-tree; SIBLING=\"\";", bindgen), (
        "the kids bindings must be witnessed as a production tree with no sibling — "
        "kids vs debug differ by whole planes, not by `test-helpers` alone"
    )


def test_the_android_kids_staging_is_ignored_and_its_twin_is_not():
    ignore = (_REPO / ".gitignore").read_text(encoding="utf-8").splitlines()
    base = "apps/fauna-android/app/src/kids/"
    for entry in ("jniLibs/", "java/com/fauna/ffi/", "java/uniffi/"):
        assert base + entry in ignore, (
            f"`{base}{entry}` is not ignored — a staged kids binding (or a 128 MB "
            "libfauna_ffi.so) would reach a commit"
        )
    for too_wide in (base, base + "java/", base + "java/com/", base + "java/com/fauna/"):
        assert too_wide not in ignore, (
            f"`{too_wide}` is ignored whole, which hides the hand-written kids twin "
            "under java/com/fauna/app/"
        )


def test_kids_app_eligible_is_one_shared_verdict_on_both_faces():
    kids = (_REPO / "libs/fauna-client-family/src/kids.rs").read_text(encoding="utf-8")
    assert re.search(
        r"pub fn kids_app_eligible\(status: &FamilyStatusReply\) -> bool \{\s*"
        r"status\.supervised_by\.is_some\(\)\s*\}",
        kids,
    ), "the verdict is `status.supervised_by.is_some()` — never the band"
    lib = (_REPO / "libs/fauna-client-family/src/lib.rs").read_text(encoding="utf-8")
    assert "pub use kids::kids_app_eligible;" in lib
    family = (_FFI / "src" / "family.rs").read_text(encoding="utf-8")
    face = re.search(
        r"#\[uniffi::export\]\npub fn kids_app_eligible\(status: FfiFamilyStatus\) -> bool \{(.*?)\n\}",
        family,
        re.S,
    )
    assert face, "the UniFFI `kids_app_eligible` face is gone (or no longer exported)"
    assert "fauna_client_family::kids_app_eligible(" in face.group(1)
    wasm = (_REPO / "libs/fauna-wasm/src/rpc.rs").read_text(encoding="utf-8")
    assert "fauna_client_family::kids_app_eligible(&reply)" in wasm
    assert '"kidsAppEligible"' in wasm, "wasm `familyStatus` no longer attaches `kidsAppEligible`"


# ── the ANDROID shell split (2026-10-05): src/noKids + the src/kids twin ─────
#
# `dynamic-features.md` § Compile-time excision (the android paragraph: a
# source-set twin for the glue, a `BuildConfig` constant for the render) and
# `family-safety.md` § The account age band, the kids-app bullet, item (4).
# Kotlin has no inline compile-time exclusion, so every shell file that names a
# declaration the kids `fauna-ffi` flavor lacks lives in `app/src/noKids/`
# (compiled by `debug`, `release`, `storeSafe`, `foss`), and `app/src/kids/`
# holds an inert twin of only what `src/main` still names. The compiler enforces
# both halves — but only when someone builds the kids variant, the heaviest gate
# android has; these pins say so on every machine, for free.

_ANDROID_APP = _REPO / "apps/fauna-android/app"
_ANDROID_MAIN = _ANDROID_APP / "src/main/java"
_ANDROID_NO_KIDS = _ANDROID_APP / "src/noKids/java"
_ANDROID_KIDS_TWIN = _ANDROID_APP / "src/kids/java/com/fauna/app"
# The OTHER source dirs the kids build type compiles (its `sourceSets` entry);
# hand-written product code that must name nothing the kids bindings lack.
_ANDROID_KIDS_EXTRA_SRCS = ("src/noPayments/java", "src/noP2pShare/java", "src/noAgent/java", "src/storeAge/java")
# Binding modules the kids flavor does not generate at all (measured 2026-10-05:
# the debug and kids `uniffi/` trees differ by exactly these seven).
_KIDS_ABSENT_MODULES = (
    "fauna_feed", "fauna_client_search", "fauna_atproto_settings_machine",
    "fauna_labeler_catalog_machine", "fauna_client_web", "fauna_client_alerts",
    "fauna_client_connected_apps",
)
# `com.fauna.ffi` types the kids face lacks that the shell reached for.
_KIDS_ABSENT_TYPES = ("FfiFeedManager", "FfiSearchManager", "FfiWebClient", "FfiMailImportClient", "FfiSourceBadge")
# `com.fauna.ffi` free functions the kids face lacks — the set `compileKidsKotlin`
# failed on before the split (2026-10-05). Matched QUALIFIED or IMPORTED, which
# is how shell code reaches a generated free function; a bare name would also
# match the app-side wrappers that kept the same names on purpose.
_KIDS_ABSENT_FUNCTIONS = (
    "buildMailSettingsMachine", "buildMailAliasesMachine", "buildMailListsMachine",
    "buildMailListMembersMachine", "buildMailExportMachine", "buildMailExportMachineWithKeyCustody",
    "buildMailImportMachine", "buildMailSpamMachine", "buildMailPolicyMachine",
    "buildCaldavPolicyMachine", "buildCarddavPolicyMachine", "buildWebdavPolicyMachine",
    "buildForwardersMachine", "buildBridgeApprovalMachine", "buildLocalDomainsMachine",
    "buildDnsManagementMachineWithCredentials", "buildLinkedNestsMachineWithMailRelayAndTrust",
    "buildConnectedAppsMachine", "buildAtprotoSettingsMachine", "buildLabelerCatalogMachineWithGrants",
    "buildWebClient", "subscriptionsCreateTier", "subscriptionsApproveSubscriber",
    "subscriptionsRemoveSubscriber", "subscriptionsSubscribePublishingEk",
    "subscriptionsReconcileOnce", "subscriptionsAuthorPollSecs", "runCriticalAlertSweep",
    "runCriticalAlertSweepLoop", "npubConfirmationOwed", "confirmNpub", "classifySources",
)
# Element-id prefixes of the surfaces the kids flavor excises — the same list the
# `android-kids-check` witness greps the dex for (a test below keeps them equal).
_KIDS_EXCISED_ID_PREFIXES = (
    "feed-", "search-", "atproto-", "personalization-", "nostr-", "labeler-catalog",
    "connected-app", "mail-settings-", "web-settings-", "admin-mail-", "admin-dns-",
    "bridge-", "profile-follow-button", "profile-tiers-tab", "link-preview-card",
    "folder-paywall-tier-select",
)
_KIDS_CONDITION = "BuildConfig.KIDS"


def _kotlin_code_only(text: str) -> str:
    """Kotlin with comments removed — a KDoc naming a symbol never reaches the dex
    (`test_payments_excision_spine.py`'s helper of the same name explains)."""
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return re.sub(r"(?<!:)//[^\n]*", "", text)


def _hand_written(root: Path) -> list[Path]:
    """Kotlin under `root`, minus the gitignored UniFFI output staged into it."""
    generated = (root / "com/fauna/ffi", root / "uniffi")
    return sorted(p for p in root.rglob("*.kt") if not any(g in p.parents for g in generated))


def _kids_compiled_shared_sources() -> list[Path]:
    """Every hand-written source the kids build type compiles besides its twin."""
    paths = _hand_written(_ANDROID_MAIN)
    for rel in _ANDROID_KIDS_EXTRA_SRCS:
        paths += _hand_written(_ANDROID_APP / rel)
    return paths


def _fun_and_class_names(path: Path) -> set[str]:
    code = _kotlin_code_only(path.read_text(encoding="utf-8"))
    return set(re.findall(r"\bfun (?:<[^>]*> )?(?:[\w.]+\.)?(\w+)\s*\(", code)) | set(
        re.findall(r"\bclass (\w+)", code)
    )


def test_the_android_no_kids_source_set_is_taken_by_every_other_build_type():
    text = _ANDROID_GRADLE.read_text(encoding="utf-8")
    sets = _block(text, "sourceSets {")
    for build_type in ("debug", "release", "storeSafe", "foss"):
        body = _code(_block(sets, f'getByName("{build_type}")'))
        assert 'java.srcDir("src/noKids/java")' in body, (
            f"the `{build_type}` build type no longer takes src/noKids — it would stop "
            "compiling every surface the kids flavor excises"
        )
    kids = _code(_block(sets, 'getByName("kids")'))
    assert "noKids" not in kids, (
        "the kids build type takes src/noKids: every excised surface is back in the "
        "kids artifact, and the twin under src/kids duplicate-declares against it"
    )
    assert _ANDROID_NO_KIDS.is_dir() and _hand_written(_ANDROID_NO_KIDS), "src/noKids holds no source"


def test_every_android_kids_twin_shadows_a_built_half():
    """Each hand-written kids twin has a same-path built half, and declares nothing
    the built half lacks — so `src/main`, written against the built half, also
    compiles against the twin (names only; the kids compile checks the types)."""
    twins = sorted(_ANDROID_KIDS_TWIN.rglob("*.kt"))
    assert twins, f"no hand-written kids twin under {_ANDROID_KIDS_TWIN} — the pin is vacuous"
    for twin in twins:
        rel = twin.relative_to(_ANDROID_KIDS_TWIN)
        built = _ANDROID_NO_KIDS / "com/fauna/app" / rel
        assert built.is_file(), f"kids twin {rel} has no built half at {built}"
        extra = _fun_and_class_names(twin) - _fun_and_class_names(built)
        extra.discard("excised")  # the twin's own private refusal helper
        assert not extra, f"kids twin {rel} declares names its built half lacks: {sorted(extra)!r}"


def test_no_kids_compiled_android_source_names_a_kids_absent_ffi_symbol():
    offenders = {}
    scanned = 0
    for path in _kids_compiled_shared_sources():
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        scanned += 1
        hits = [f"uniffi.{m}" for m in _KIDS_ABSENT_MODULES if re.search(rf"\buniffi\.{m}\b", code)]
        hits += [t for t in _KIDS_ABSENT_TYPES if re.search(rf"\b{t}\b", code)]
        hits += [
            f"com.fauna.ffi.{f}"
            for f in _KIDS_ABSENT_FUNCTIONS
            if re.search(rf"com\.fauna\.ffi\.{f}\b", code)
        ]
        if hits:
            offenders[path.relative_to(_REPO).as_posix()] = hits
    assert scanned, "no android source scanned — the pin is vacuous"
    assert not offenders, (
        "these sources are compiled by the kids build type but name FFI symbols its "
        "kids-safe bindings do not export, so `just android-kids` cannot compile. Move "
        "the file to app/src/noKids/ (a twin in app/src/kids/ only if src/main still "
        f"names it): {offenders!r}"
    )


def _paint_tokens() -> list[str]:
    tokens = []
    for prefix in _KIDS_EXCISED_ID_PREFIXES:
        tokens.append(f'"{prefix}')
        tokens.append("Ids." + prefix.upper().replace("-", "_"))
    return tokens


def test_every_shared_android_render_of_a_kids_excised_id_carries_the_condition():
    """The render half: a shared surface painting an excised id compiles fine in the
    kids flavor and ships every id it would paint unless `BuildConfig.KIDS` lets the release shrinker
    fold it (`dynamic-features.md` § What "completely compiled away" means,
    criterion 1) — dead is not absent."""
    tokens = _paint_tokens()
    carriers, offenders = [], []
    for path in _hand_written(_ANDROID_MAIN):
        code = _kotlin_code_only(path.read_text(encoding="utf-8"))
        if not any(token in code for token in tokens):
            continue
        rel = path.relative_to(_REPO).as_posix()
        if rel.endswith("social/fauna/generated/UiIds.kt"):
            continue
        carriers.append(rel)
        if _KIDS_CONDITION not in code:
            offenders.append(rel)
    assert carriers, "no src/main source paints a kids-excised id at all — the pin is vacuous"
    assert not offenders, (
        "these shared sources paint kids-excised element ids with no "
        f"`{_KIDS_CONDITION}` anywhere in the file, so the kids APK ships them: {offenders!r}"
    )


def test_the_android_kids_witness_checks_both_columns():
    body = _recipe("android-kids-check")
    assert '_android-ffi-flavor kids "kids-safe,kids-floor" arm64' in body
    assert "assembleKids" in body and "assembleRelease" in body, (
        "android-kids-check lost a column — without the release APK the kids "
        "absences are indistinguishable from greps that match nothing"
    )
    assert "classes*.dex" in body and "lib/*/*.so" in body and "AndroidManifest.xml" in body, (
        "android-kids-check no longer UNPACKS the APK members it greps (dex strings are "
        "deflate-compressed inside the archive)"
    )
    assert "social\\.fauna\\.faunakids" in body, "android-kids-check lost the identity check"
    assert "FACE_PATS=(" in body and "'^uniffi_fauna_feed_fn_'" in body, (
        "android-kids-check no longer checks the packaged library's exported UniFFI "
        "faces — the .so half of the excision would go unwitnessed"
    )
    ids = re.search(r"ID_PATS=\(([^)]*)\)", body)
    assert ids, "android-kids-check lost its element-id pattern list"
    witnessed = tuple(p.strip("'^") for p in ids.group(1).split())
    assert witnessed == _KIDS_EXCISED_ID_PREFIXES, (
        "the witness's element-id prefixes drifted from the render pin's: "
        f"{witnessed!r} != {_KIDS_EXCISED_ID_PREFIXES!r}"
    )
    assert "vacuous" in body


def test_the_post_tip_resolver_twin_is_not_in_a_kids_compiled_directory():
    """`resolvePostTipsIfBuilt` extends the FEED manager, so its excised twin cannot
    live in `src/noPayments/` (which the kids build type also compiles and whose
    bindings carry no feed at all): it is `storeSafe`'s own."""
    built = _ANDROID_APP / "src/payments/java/com/fauna/app/payments/PostTipsGlue.kt"
    excised = _ANDROID_APP / "src/storeSafe/java/com/fauna/app/payments/PostTipsGlue.kt"
    for path in (built, excised):
        assert "fun FfiFeedManager.resolvePostTipsIfBuilt(" in path.read_text(encoding="utf-8"), path
    no_payments = _ANDROID_APP / "src/noPayments/java"
    for path in _hand_written(no_payments):
        assert "FfiFeedManager" not in _kotlin_code_only(path.read_text(encoding="utf-8")), (
            f"{path} names FfiFeedManager, which the kids bindings lack"
        )
