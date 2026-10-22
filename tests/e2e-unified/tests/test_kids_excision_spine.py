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
        "feed-manager", "feed-rules", "feed-badge", "feed-client",  # the feed
        "search-manager", "search-client",  # discovery
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
    # would render past the kids floor. `render_sources` is the one shared
    # helper that does it for the composed faces, so a face may go through it —
    # provided its own body is where the policy is resolved.
    sources = re.search(
        r"\nfn render_sources\((.*?)\n\}\n",
        (_FFI / "src" / "family.rs").read_text(encoding="utf-8"),
        re.S,
    )
    assert sources and "render_guardian_policy(" in sources.group(1), (
        "`render_sources` must resolve the guardian policy through `render_guardian_policy`"
    )
    for path in sorted((_FFI / "src").glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for body in re.split(r"\n\s*(?:pub(?:\([a-z]+\))? )?fn ", text)[1:]:
            if re.search(r"\brender_verdict_(?:composed|for_item)\(", body):
                assert "render_guardian_policy(" in body or "render_sources(" in body, (
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
        "the kids build type must state it carries no version-name suffix, not "
        "inherit whatever storeSafe has — this artifact ships, on the train at the "
        "fleet version"
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
    assert "*/src/release/*|*/src/storeSafe/*|*/src/storeNoPayments/*|*/src/foss/*|*/src/kids/*)" in bindgen
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


def test_the_android_kids_build_never_renders_or_calls_the_remote_image_reveal():
    """The Play kids data answers say the push distributor is the one other party the
    kids build talks to; `load-remote-content-button` would have the device fetch from
    whatever site hosts a message's picture (`dynamic-features.md`, the kids spine
    paragraph). The button's render and the manager call both carry the condition."""
    path = _ANDROID_MAIN / "com/fauna/app/ui/screen/conversations/ConversationDetailScreen.kt"
    # Line comments only: the block-comment stripper would eat from a `image/*` MIME string.
    code = re.sub(r"(?m)^\s*//[^\n]*\n", "", path.read_text(encoding="utf-8"))
    button = re.search(
        r"if \(([^\n{]*)\) \{\s*TextButton\([^)]*?onClick = onRevealRemoteImages,"
        r"[^)]*?Ids\.LOAD_REMOTE_CONTENT_BUTTON",
        code,
        flags=re.DOTALL,
    )
    assert button, "the reveal button's `if (...)` guard was not found — the render pin is vacuous"
    assert f"!{_KIDS_CONDITION}" in button.group(1), (
        "the kids build renders `load-remote-content-button`: a child's tap makes the device "
        "fetch an image from a third-party host"
    )
    assert f"if (!{_KIDS_CONDITION}) manager.revealRemoteImages(msgId)" in code, (
        "the manager's `revealRemoteImages` call lost its `!BuildConfig.KIDS` guard"
    )


def test_the_ios_kids_build_never_renders_or_calls_the_remote_image_reveal():
    """The iOS leg of the pin above: the shared bubble's `load-remote-content-button`
    and the thread view's `revealRemoteImages` call sit behind `#if !FAUNA_KIDS`, and
    the two feed files holding the other reveal buttons are compiled out wholesale."""
    kits = _APPLE / "FaunaKit/Sources/FaunaKit/Views"

    def code(path: Path) -> str:
        return re.sub(r"(?m)^\s*//[^\n]*\n", "", path.read_text(encoding="utf-8"))

    bubble = code(kits / "DmMessageBubble.swift")
    guarded = re.search(
        r"#if !FAUNA_KIDS\s*if hasBlockedRemote \{(.*?)\n        \}\n        #endif\n",
        bubble,
        flags=re.DOTALL,
    )
    assert guarded, "the bubble's reveal button lost its `#if !FAUNA_KIDS` guard"
    assert "Ids.loadRemoteContentButton" in guarded.group(1)
    assert bubble.count("Ids.loadRemoteContentButton") == 2 == guarded.group(1).count(
        "Ids.loadRemoteContentButton"
    ), "a `load-remote-content-button` reference sits outside the kids guard"

    thread = code(kits / "ThreadDetailView.swift")
    assert re.search(
        r"onRevealRemoteImages: \{ mid in\s*#if !FAUNA_KIDS\s*vm\.revealRemoteImages\(mid\)\s*#endif",
        thread,
    ), "the thread view's `revealRemoteImages` call lost its `#if !FAUNA_KIDS` guard"
    for whole_file in (kits / "PostCardBody.swift", _APPLE / "Fauna-iOS/Views/Feed/PostDetailView.swift"):
        text = code(whole_file)
        assert text.lstrip().startswith("#if !FAUNA_KIDS"), f"{whole_file.name} is no longer excised whole"
        assert text.rstrip().rsplit("\n", 1)[-1].startswith("#endif"), (
            f"{whole_file.name} is no longer excised whole"
        )


def test_the_android_kids_build_routes_into_no_nest_setup_or_nest_retirement():
    """The Play kids data answers name no hosting or DNS provider: a child joins a
    guardian's nest by invite, so the kids build registers no VPS/DNS/provisioning
    route and offers no nest retirement (`dynamic-features.md`, the kids spine
    paragraph). Each carries `!BuildConfig.KIDS`, which the release shrinker folds out of the kids APK."""
    ui = _ANDROID_MAIN / "com/fauna/app/ui"
    nav = _kotlin_code_only((ui / "navigation/FaunaNavHost.kt").read_text(encoding="utf-8"))
    setup = re.search(
        r"if \(!BuildConfig\.KIDS\) \{(.*?)\n        \}\n", nav, flags=re.DOTALL
    )
    assert setup, "the nest setup route block lost its `if (!BuildConfig.KIDS)` guard"
    for route in ("onboarding/dns-config", "onboarding/vps-config", "onboarding/nest-provisioning"):
        assert f'composable("{route}")' in setup.group(1), f"`{route}` is registered outside the kids guard"
        assert nav.count(f'composable("{route}")') == 1
    assert 'if (!BuildConfig.KIDS) composable("settings/admin-nest-retire")' in nav, (
        "the admin nest-retire route is registered on the kids build"
    )
    assert "if (!BuildConfig.KIDS && retiring)" in nav, (
        "the launch retry surface can host nest retirement on the kids build"
    )
    # The entry points: the wizard's step router and both retire buttons.
    handle = _kotlin_code_only((ui / "screen/onboarding/HandleEntryScreen.kt").read_text(encoding="utf-8"))
    for step, route in (
        ("DNS_CONFIG", "onboarding/dns-config"),
        ("VPS_CONFIG", "onboarding/vps-config"),
        ("NEST_PROVISIONING", "onboarding/nest-provisioning"),
    ):
        assert re.search(
            rf'OnboardingStep\.{step} ->\s*if \(BuildConfig\.KIDS\) "onboarding/handle-entry" else "{route}"',
            handle,
        ), f"routeForStep sends {step} to its provider screen on the kids build"
    retry = _kotlin_code_only((ui / "screen/LaunchRetryScreen.kt").read_text(encoding="utf-8"))
    assert re.search(r"if \(!BuildConfig\.KIDS\) \{\s*Spacer[^}]*?Ids\.LAUNCH_RETIRE_BUTTON", retry, re.S), (
        "`launch-retire-button` is rendered on the kids build"
    )
    admin = _kotlin_code_only((ui / "screen/settings/AdminNestScreen.kt").read_text(encoding="utf-8"))
    assert re.search(r"if \(!BuildConfig\.KIDS\) \{\s*RetireSection\(onRetire\)", admin), (
        "the admin page renders the nest-retire section on the kids build"
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


# The kind-string half of criterion 2 (dynamic-features.md § What "completely
# compiled away" means): the feed, search and web planes' wire senders AND
# their kind-catalog text. The catalog lives in `fauna-protocol`, which every
# build links, so each plane's entries sit behind a protocol feature forwarded
# only by the plane's own client crate; fauna-ffi reaches those crates only
# through features in `store-safe`'s tail.
_PROTOCOL = _REPO / "libs" / "fauna-protocol"
_KIND_PLANES = ("feed", "search", "web")
_KIND_PATS = r"KIND_PATS=\('fauna\\.feed\\.' 'fauna\\.search\\.' 'fauna\\.web\\.'\)"


def _manifest(crate: str) -> dict:
    return tomllib.loads((_REPO / "libs" / crate / "Cargo.toml").read_text(encoding="utf-8"))


def test_the_excised_planes_kind_catalog_is_gated_per_plane():
    protocol = _manifest("fauna-protocol")["features"]
    kind_rs = (_PROTOCOL / "src" / "kind.rs").read_text(encoding="utf-8")
    offline = (_PROTOCOL / "src" / "offline_class.rs").read_text(encoding="utf-8")
    for plane in _KIND_PLANES:
        assert plane in protocol, f"fauna-protocol lost its `{plane}` kind-catalog feature"
        assert plane not in protocol["default"], (
            f"`{plane}` is default-on in fauna-protocol — any consumer taking the crate "
            "with default features would re-acquire the excised plane's catalog"
        )
        fn = "register_search_kinds" if plane == "search" else f"register_{plane}_kinds"
        assert re.search(rf'#\[cfg\(feature = "{plane}"\)\]\s*pub fn {fn}\b', kind_rs), (
            f"`{fn}` is not behind fauna-protocol's `{plane}` feature"
        )
        assert re.search(rf'#\[cfg\(feature = "{plane}"\)\]\s*self\.{fn}\(\);', kind_rs), (
            f"`register_all_kinds` calls `{fn}` ungated"
        )
        # Every offline-class row of the plane sits inside a `cfg(feature)` block.
        for m in re.finditer(rf'm\.insert\("fauna\.{plane}\.', offline):
            block_start = offline.rfind("#[cfg(", 0, m.start())
            assert offline[block_start:].startswith(f'#[cfg(feature = "{plane}")]'), (
                f"an offline_class row of `fauna.{plane}.*` is outside the `{plane}` gate"
            )
            assert "}" not in offline[offline.index("{", block_start):m.start()], (
                f"an offline_class row of `fauna.{plane}.*` is outside the `{plane}` gate"
            )


def test_only_the_plane_crates_and_store_safe_reach_the_kind_catalog():
    # Each plane's client crate forwards its catalog — except search, whose crate
    # the kids flavor still links for the on-device index seam, so only its
    # opt-in `query` half (the nest sender) forwards it.
    for crate, plane in (("fauna-client-feed", "feed"), ("fauna-client-web", "web")):
        dep = _manifest(crate)["dependencies"]["fauna-protocol"]
        assert plane in dep.get("features", []), f"{crate} no longer carries the `{plane}` catalog"
    search = _manifest("fauna-client-search")
    assert "search" not in search["dependencies"]["fauna-protocol"].get("features", []), (
        "fauna-client-search names the `search` catalog on its dep line — the kids flavor "
        "links this crate for the local index seam, so that would re-acquire the catalog"
    )
    assert search["features"]["query"] == ["fauna-protocol/search"]
    assert "query" not in search["features"]["default"]

    features = _features()
    kids = _closure(features, "kids-safe") | set(features["kids-safe"])
    for entry in (
        "feed-client", "search-client", "dep:fauna-client-feed", "dep:fauna-client-search",
        "dep:fauna-feed", "dep:fauna-client-web", "fauna-client-search/query",
        "fauna-devices-machine?/web", "fauna-sync-engine/web",
    ):
        assert entry not in kids, f"the kids complement reaches `{entry}`, an excised plane's sender"
    store_safe = _closure(features, "store-safe")
    for entry in ("feed-client", "search-client", "fauna-devices-machine?/web", "fauna-sync-engine/web"):
        assert entry in store_safe, f"store-safe lost `{entry}` — the plane would vanish from the main app"
    deps = tomllib.loads((_FFI / "Cargo.toml").read_text(encoding="utf-8"))["dependencies"]
    for crate in ("fauna-client-feed", "fauna-client-search", "fauna-feed", "fauna-client-web"):
        assert deps[crate].get("optional"), f"fauna-ffi links `{crate}` unconditionally"
    # Rule (b): the devices machine's website half is forwarded from fauna-ffi's
    # own feature, never named on the dependency line.
    assert "web" not in deps["fauna-devices-machine"].get("features", [])


def test_both_kids_witnesses_grep_the_kind_strings_in_both_columns():
    for recipe, kids_file, default_file in (
        ("android-kids-check", '"$WORK/kids-all.txt"', '"$WORK/rel-all.txt"'),
        ("apple-kids-check", '"$SCRATCH/kids.strings"', '"$SCRATCH/default.strings"'),
    ):
        body = _recipe(recipe)
        assert re.search(_KIND_PATS, body), f"{recipe} lost its feed/search/web kind-string patterns"
        loops = re.findall(r'for pat in "\$\{KIND_PATS\[@\]\}"; do\n(.*?)\n    done', body, re.S)
        assert len(loops) == 2, f"{recipe} must check the kind strings in both columns"
        assert kids_file in loops[0] and default_file in loops[1], (
            f"{recipe}'s kind column reads the wrong artifact"
        )
        assert "vacuous" in loops[1]


def test_the_post_tip_resolver_twin_is_not_in_a_kids_compiled_directory():
    """`resolvePostTipsIfBuilt` extends the FEED manager, so its excised twin cannot
    live in `src/noPayments/` (which the kids build type also compiles and whose
    bindings carry no feed at all): it is in `src/noKidsNoPayments/`, taken by the
    two build types with a feed and no payments and never by `kids`."""
    built = _ANDROID_APP / "src/payments/java/com/fauna/app/payments/PostTipsGlue.kt"
    excised = _ANDROID_APP / "src/noKidsNoPayments/java/com/fauna/app/payments/PostTipsGlue.kt"
    for path in (built, excised):
        assert "fun FfiFeedManager.resolvePostTipsIfBuilt(" in path.read_text(encoding="utf-8"), path
    gradle = (_ANDROID_APP / "build.gradle.kts").read_text(encoding="utf-8")
    for build_type, takes in (("storeSafe", True), ("storeNoPayments", True), ("kids", False)):
        m = re.search(rf'getByName\("{build_type}"\)\s*\{{([^}}]*)\}}', gradle)
        assert m, f"no sourceSets entry for {build_type}"
        assert ('java.srcDir("src/noKidsNoPayments/java")' in m.group(1)) == takes, (
            f"{build_type} {'must' if takes else 'must NOT'} take src/noKidsNoPayments/java"
        )
    no_payments = _ANDROID_APP / "src/noPayments/java"
    for path in _hand_written(no_payments):
        assert "FfiFeedManager" not in _kotlin_code_only(path.read_text(encoding="utf-8")), (
            f"{path} names FfiFeedManager, which the kids bindings lack"
        )


# ── the iOS leg ──────────────────────────────────────────────────────────────
# family-safety.md § The account age band, the kids-app bullet, item (5), apple:
# a second app target sharing FaunaKit, built `-D FAUNA_KIDS` over the kids FFI
# flavor, the render conditioned, witnessed by `apple-kids-check` on the archive.

_APPLE = _REPO / "apps/fauna-apple"
_PBXPROJ = _APPLE / "Fauna.xcodeproj/project.pbxproj"
_UI_IDS_SWIFT = _APPLE / "FaunaKit/Sources/FaunaKit/Generated/UiIds.swift"
_UI_IDS_GENERATOR = _REPO / "scripts/ui-ids-generate.py"
_APPLE_KIDS_FEATURES = (
    'FEATURE_ARGS=(--no-default-features --features "kids-safe,kids-floor,file-provider-host")'
)


def test_the_apple_kids_ffi_is_the_named_complement_plus_the_floor():
    assert '_apple-ffi-flavor "kids-safe,kids-floor"' in _recipe("apple-ffi-kids")
    assert '_apple-ffi-host-flavor "{{config}}" "kids-safe,kids-floor"' in _recipe("apple-ffi-host-kids")
    for helper in ("_apple-ffi-build-impl", "_apple-ffi-host-flavor"):
        assert _APPLE_KIDS_FEATURES in _recipe(helper), (
            f"{helper} has no kids branch, so `kids-safe,kids-floor` falls through to the "
            "additive arm and builds WITH the default features — nothing excised"
        )


def test_the_swift_id_table_puts_the_kids_prefixes_behind_the_kids_condition():
    generator = _UI_IDS_GENERATOR.read_text(encoding="utf-8")
    m = re.search(r"KIDS_EXCISED_ID_PREFIXES = \(([^)]*)\)", generator)
    assert m, "ui-ids-generate.py lost its kids prefix list"
    assert tuple(re.findall(r'"([^"]+)"', m.group(1))) == _KIDS_EXCISED_ID_PREFIXES, (
        "the Swift table's kids prefixes drifted from the android witness's"
    )
    table = _UI_IDS_SWIFT.read_text(encoding="utf-8")
    shared = table.split("public enum Ids {", 1)[1].split("\n}\n", 1)[0]
    leaked = re.findall(rf'"((?:{"|".join(map(re.escape, _KIDS_EXCISED_ID_PREFIXES))})[^"]*)"', shared)
    assert not leaked, f"kids-excised ids in the ungated Ids table: {leaked[:5]!r}"
    assert "#if !FAUNA_KIDS\nextension Ids {" in table


def test_the_ios_kids_build_routes_into_no_nest_setup_and_gates_the_retire_ids():
    """The iOS leg of the excision of nest setup and nest retirement (`dynamic-features.md`,
    the kids spine paragraph): the wizard's DNS/VPS/provisioning steps stay on handle
    entry under `FAUNA_KIDS`, the three provider views are compiled out whole, and the
    retire ids sit in the kids-gated `Ids` extension so a future retirement render
    cannot compile into the kids build."""
    onboarding = _APPLE / "Fauna-iOS/Views/Onboarding"
    welcome = (onboarding / "WelcomeView.swift").read_text(encoding="utf-8")
    kids_arm, rest = welcome.split("#if FAUNA_KIDS", 1)[1].split("#else", 1)
    assert "case .dnsConfig, .vpsConfig, .nestProvisioning:" in kids_arm and "HandleEntryView(vm: vm)" in kids_arm
    full_arm = rest.split("#endif", 1)[0]
    for view in ("DnsConfigView", "VpsConfigView", "NestProvisioningView"):
        assert f"{view}(vm: vm)" in full_arm and f"{view}(vm: vm)" not in kids_arm, (
            f"{view} is reachable from the kids router arm"
        )
        text = (onboarding / f"{view}.swift").read_text(encoding="utf-8")
        assert text.startswith("#if !FAUNA_KIDS\n") and text.rstrip().endswith("#endif // !FAUNA_KIDS"), (
            f"{view}.swift is no longer compiled out of the kids build whole"
        )
    generator = _UI_IDS_GENERATOR.read_text(encoding="utf-8")
    m = re.search(r"SWIFT_KIDS_ONLY_ID_PREFIXES = \(([^)]*)\)", generator)
    assert m and set(re.findall(r'"([^"]+)"', m.group(1))) == {"admin-nest-retire-", "launch-retire-button"}
    table = _UI_IDS_SWIFT.read_text(encoding="utf-8")
    shared = table.split("public enum Ids {", 1)[1].split("\n}\n", 1)[0]
    gated = table.split("#if !FAUNA_KIDS\nextension Ids {", 1)[1]
    for name in ("adminNestRetireSection", "adminNestRetireButton", "launchRetireButton"):
        assert name not in shared, f"`{name}` is in the ungated Ids table"
        assert name in gated, f"`{name}` is missing from the kids-gated Ids extension"


def _pbx_target_configs(name: str) -> list[str]:
    text = _PBXPROJ.read_text(encoding="utf-8")
    target = re.search(
        rf'/\* {re.escape(name)} \*/ = {{\n\t\t\tisa = PBXNativeTarget;.*?buildConfigurationList = (\w+)',
        text, re.S)
    assert target, f"no PBXNativeTarget {name!r}"
    configs = re.search(rf"\t\t{target.group(1)} /\*.*?buildConfigurations = \(\n(.*?)\);", text, re.S)
    ids = re.findall(r"(\w+) /\*", configs.group(1))
    return [re.search(rf"\t\t{i} /\* \w+ \*/ = {{\n(.*?)\n\t\t}};", text, re.S).group(1) for i in ids]


def test_the_ios_kids_target_is_its_own_identity_and_the_main_target_is_not_kids():
    kids = _pbx_target_configs("Fauna-Kids-iOS")
    assert len(kids) == 2
    for config in kids:
        assert "PRODUCT_BUNDLE_IDENTIFIER = social.fauna.faunakids;" in config
        assert "FAUNA_KIDS FAUNA_EXCISE_PAYMENTS FAUNA_EXCISE_P2P_SHARE" in config
        assert "Fauna-iOS/Resources/Info.plist" in config, (
            "the kids target reads the main app's Info.plist so its version stays in lockstep"
        )
    for config in _pbx_target_configs("Fauna-iOS"):
        assert "FAUNA_KIDS" not in config and "faunakids" not in config


def test_the_apple_kids_witness_checks_both_columns():
    body = _recipe("apple-kids-check")
    assert "just apple-ffi-kids" in body and "just apple-ffi\n" in body, (
        "apple-kids-check must rebuild the one xcframework staging slot before each column"
    )
    assert "archive kids Fauna-Kids-iOS" in body and "archive default Fauna-iOS" in body
    assert "-D FAUNA_KIDS -D FAUNA_EXCISE_PAYMENTS -D FAUNA_EXCISE_P2P_SHARE" in body, (
        "the kids archive must carry its conditions on the command line — a target's own "
        "conditions do not reach the FaunaKit package"
    )
    assert "target/aarch64-apple-ios/release/libfauna_ffi.a" in body
    assert "KIDS_ID='social.fauna.faunakids'" in body
    ids = re.search(r"ID_PATS=\(([^)]*)\)", body)
    assert ids and tuple(p.strip("'^") for p in ids.group(1).split()) == _KIDS_EXCISED_ID_PREFIXES
    android_faces = re.search(r"FACE_PATS=\(([^)]*)\)", _recipe("android-kids-check")).group(1)
    assert re.search(r"FACE_PATS=\(([^)]*)\)", body).group(1) == android_faces
    assert "vacuous" in body


def test_the_kids_app_sends_a_declared_only_age_claim():
    """The armed iOS arm pins the MAIN App ID, and App Attest signs under the running
    bundle's — a kids attestation would fail the check and refuse the admission."""
    src = (_APPLE / "FaunaKit/Sources/FaunaKit/Core/StoreAgeClaim.swift").read_text(encoding="utf-8")
    body = src.split("private func attestOrNil(", 1)[1]
    gate = body.index("#if FAUNA_KIDS")
    assert body.index("return nil", gate) < body.index("signals.attest(", gate)


def test_the_kids_build_opens_no_outbound_link():
    opener = (_APPLE / "FaunaKit/Sources/FaunaKit/Utilities/CrossPlatformUI.swift").read_text(encoding="utf-8")
    body = opener.split("public enum OpenURL {", 1)[1]
    assert body.index("#if FAUNA_KIDS") < body.index("return") < body.index("UIApplication.shared.open")
    app = (_APPLE / "Fauna-iOS/App/FaunaApp.swift").read_text(encoding="utf-8")
    assert re.search(
        r"#if FAUNA_KIDS\n\s*\.environment\(\\\.openURL, OpenURLAction \{ _ in \.discarded \}\)\n\s*#endif",
        app,
    ), "the kids app root no longer refuses SwiftUI's own link taps"
