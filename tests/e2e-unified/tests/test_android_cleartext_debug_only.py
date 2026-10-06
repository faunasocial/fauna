"""tier_1: the android cleartext-to-loopback policy reaches `debug` and nothing else.

Authority: `docs/goal/architecture/e2e-conventions.md` convention 15 (the
automation surface is compiled out of release artifacts — runtime `FAUNA_E2E_*`
gates are never the security boundary) →
`docs/goal/architecture/e2e-automation-surface-gating.md`; and
`docs/goal/architecture/testing.md` § Default app and nest mode → *Android's run
venue*, constraint 3: "the device reaches the test nest as its own loopback
(`adb reverse`), with cleartext permitted to `127.0.0.1` in the debug source set
only — production's no-cleartext network policy is never widened".

WHY THIS IS A SOURCE-SET ASSERTION AND NOT AN ARTIFACT GREP. The module's other convention-15 witnesses — `just
android-store-safe-check`, `just android-foss-check` — unpack a built APK and
grep its dex, and their own recipe comments say why they must: those absences
are **the release shrinker's doing**, a constant-gated `if` it *should* fold, and
"should fold" is not the same claim as "is absent". This absence is a different
mechanism entirely. Android resource merging resolves a same-named resource from
the variant's own build-type source set, and a file under `src/debug/res/` is
not an input to a `release` / `storeSafe` / `foss` variant **by construction of
the build graph** — no optimizer decides it. The claim that can actually go
stale is therefore "which directories does each shipping variant read", so that
is what this file measures, deriving the answer from `build.gradle.kts` rather
than from a hand-kept list. It costs no build and runs on every machine,
including the ones that cannot assemble an android release at all.

The cost of being wrong is not subtle: a shipping APK that permitted cleartext
to loopback would let any local process on the user's phone MITM the app's nest
traffic. That is why the file also pins the *shape* of the debug overlay — the
`127.0.0.1`-only domain-config, the un-flipped `base-config` — rather than
merely its location. A debug build that trusted a LAN range would put an
unauthenticated plaintext nest on a network interface, which constraint 2 of the
same section forbids outright.
"""

import os
import re
import sys
import xml.etree.ElementTree as ET

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_APP = os.path.join(_REPO, "apps", "fauna-android", "app")
_SRC = os.path.join(_APP, "src")
_GRADLE = os.path.join(_APP, "build.gradle.kts")
_MANIFEST = os.path.join(_SRC, "main", "AndroidManifest.xml")

#: The build type whose source set MAY permit cleartext. Everything else
#: declared in `buildTypes` is a shipping artifact and may not.
_DEBUG = "debug"

#: The resource name the manifest points `android:networkSecurityConfig` at.
_CONFIG_BASENAME = "network_security_config.xml"


def _read(path):
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def _block(text: str, header: str) -> str:
    """The brace-balanced body of `header { ... }` in a Kotlin DSL file.

    A regex cannot do this (the bodies nest), and a wrong answer here would
    silently shrink every set below to nothing — so callers assert the result is
    non-empty rather than letting a miss read as a pass.
    """
    start = text.index(header) + len(header)
    depth = 1
    for i in range(start, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[start:i]
    raise AssertionError(f"unbalanced braces after {header!r} in {_GRADLE}")


def _declared_build_types() -> set[str]:
    """Every build type `build.gradle.kts` declares.

    AGP predefines exactly `debug` and `release`; any other build type must be
    `create("name")`. So the shipping set is derived, never listed here — a
    fourth shipping variant added tomorrow is covered on the day it lands rather
    than on the day someone remembers this file.
    """
    body = _block(_read(_GRADLE), "buildTypes {")
    types = set(re.findall(r'create\("(\w+)"\)', body))
    for predefined in ("debug", "release"):
        if re.search(rf"^\s*{predefined}\s*\{{", body, re.M):
            types.add(predefined)
    return types


def _shipping_build_types() -> set[str]:
    return _declared_build_types() - {_DEBUG}


def _source_set_extra_dirs() -> dict[str, list[str]]:
    """`getByName("X") { java.srcDir("p") ... }` → {"X": ["p", ...]}.

    The `sourceSets` block is the one place a shipping variant could be given a
    directory it does not own by name — which is exactly how the automation
    surface's own twin (`src/noAgent/java`) is shared between `release`,
    `storeSafe` and `foss`. A future `java.srcDir("src/debug/...")` on a shipping
    type is the mutant this mapping exists to catch.
    """
    body = _block(_read(_GRADLE), "sourceSets {")
    out: dict[str, list[str]] = {}
    for match in re.finditer(r'getByName\("(\w+)"\)\s*\{', body):
        name = match.group(1)
        inner = _block(body[match.start():], match.group(0))
        out[name] = re.findall(r'srcDir\("([^"]+)"\)', inner)
    return out


def _configs_on_disk() -> dict[str, str]:
    """Every network-security-config resource under `app/src/`, path → text."""
    found = {}
    for root, _dirs, files in os.walk(_SRC):
        for name in files:
            if name == _CONFIG_BASENAME:
                path = os.path.join(root, name)
                found[os.path.relpath(path, _APP).replace(os.sep, "/")] = _read(path)
    return found


def _permits_cleartext(text: str) -> bool:
    return 'cleartextTrafficPermitted="true"' in text


# ── the parse itself is not allowed to be vacuous ───────────────────────────


def test_the_three_known_shipping_build_types_are_seen():
    # Guards every assertion below: a parse that silently returned an empty set
    # would make each of them pass over nothing. Named explicitly so a renamed
    # or removed shipping variant reds here, where the message says what broke,
    # rather than quietly widening the thing being measured.
    assert {"release", "storeSafe", "foss"} <= _shipping_build_types()
    assert _DEBUG in _declared_build_types()


def test_at_least_one_config_is_found_on_disk():
    configs = _configs_on_disk()
    assert configs, f"no {_CONFIG_BASENAME} found under {_SRC}"
    assert "src/main/res/xml/" + _CONFIG_BASENAME in configs


@pytest.mark.parametrize("variant", ["main", _DEBUG])
def test_every_config_is_well_formed_xml(variant):
    # aapt2 is the only other thing that ever parses these files, and it runs
    # on a machine that can assemble android -- which most of the fleet cannot.
    # A malformed overlay would therefore reach `main` green and break the
    # android build for whoever next has a device. The classic way in is an
    # XML comment containing `--`, which is illegal and which prose comments
    # like this file's own attract.
    path = os.path.join(_SRC, variant, "res", "xml", _CONFIG_BASENAME)
    ET.parse(path)


def test_the_manifest_still_points_at_the_merged_resource():
    # The overlay works by RESOURCE NAME. If the manifest stopped naming
    # @xml/network_security_config, both files would become dead weight and the
    # debug build would silently lose its cleartext permission — a failure that
    # looks like a product bug on the first android e2e run.
    assert 'android:networkSecurityConfig="@xml/network_security_config"' in _read(_MANIFEST)


# ── the claim: cleartext lives in `debug` and nowhere else ──────────────────


def test_only_the_debug_source_set_permits_cleartext():
    offenders = [
        path
        for path, text in _configs_on_disk().items()
        if _permits_cleartext(text) and not path.startswith(f"src/{_DEBUG}/")
    ]
    assert not offenders, (
        "a network security config outside the debug source set permits "
        f"cleartext: {offenders}. Production's no-cleartext policy is never "
        "widened (testing.md § Default app and nest mode → Android's run venue, "
        "constraint 3)."
    )


def test_no_shipping_build_type_owns_a_network_security_config():
    configs = _configs_on_disk()
    for build_type in sorted(_shipping_build_types()):
        owned = [p for p in configs if p.startswith(f"src/{build_type}/")]
        assert not owned, (
            f"the {build_type} source set carries its own {_CONFIG_BASENAME} "
            f"({owned}); a shipping variant must inherit src/main's policy "
            "unchanged so there is exactly one production posture to review."
        )


def test_no_shipping_build_type_is_handed_a_debug_directory():
    extras = _source_set_extra_dirs()
    assert extras, f"no sourceSets entries parsed out of {_GRADLE}"
    for build_type in sorted(_shipping_build_types()):
        for directory in extras.get(build_type, []):
            assert not directory.startswith(f"src/{_DEBUG}"), (
                f"the {build_type} source set adds {directory!r}, which would "
                "pull debug-only resources into a shipping artifact "
                "(convention 15)."
            )


def test_main_keeps_the_production_posture():
    main = _configs_on_disk()["src/main/res/xml/" + _CONFIG_BASENAME]
    assert 'cleartextTrafficPermitted="false"' in main
    assert not _permits_cleartext(main)


# ── the debug overlay's own shape ───────────────────────────────────────────


def test_the_debug_overlay_exists_and_permits_loopback():
    debug = _configs_on_disk().get(f"src/{_DEBUG}/res/xml/" + _CONFIG_BASENAME)
    assert debug, (
        "the debug cleartext overlay is missing; without it an android e2e run "
        "cannot reach the standalone nest at http://127.0.0.1:<port> at all "
        "(testing.md § Default app and nest mode → Android's run venue)."
    )
    assert _permits_cleartext(debug)
    assert "127.0.0.1" in debug


def test_the_debug_overlay_permits_loopback_and_nothing_else():
    debug = _configs_on_disk()[f"src/{_DEBUG}/res/xml/" + _CONFIG_BASENAME]
    domains = re.findall(r"<domain[^>]*>([^<]+)</domain>", debug)
    assert [d.strip() for d in domains] == ["127.0.0.1"], (
        f"the debug overlay trusts {domains}; constraint 3 permits cleartext to "
        "127.0.0.1 only — no LAN range, and no 10.0.2.2 (the reverse tunnel is "
        "what carries the device to the nest, so its own loopback is enough)."
    )


def test_the_debug_overlay_does_not_flip_the_base_config():
    # The narrow grant is the whole point: a debug build that flipped
    # base-config would permit cleartext to EVERY host, which is a different and
    # much larger claim than "the harness's loopback nest".
    debug = _configs_on_disk()[f"src/{_DEBUG}/res/xml/" + _CONFIG_BASENAME]
    base = re.search(r"<base-config[^>]*>", debug)
    assert base, "the debug overlay dropped its base-config"
    assert 'cleartextTrafficPermitted="false"' in base.group(0)
