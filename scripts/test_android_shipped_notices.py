"""The Android channel of shipped-notices: every shipping package carries the
three licence files in its `assets/`.

Owner: docs/goal/architecture/shipped-notices.md § Per channel (the Android
row) and § How the notices file is produced (*What holds it in place*: per
channel a tier_1 authoring test that the packaging source stages the three
files). Pins, by reading tracked sources only:

  * every shipping recipe stages the files (`_android-notices <buildtype>`)
    before its Gradle step and checks the built package's entry list after it;
  * each shipping build type's source set in `app/build.gradle.kts` names the
    staging directory as an assets directory;
  * the notices manifest names one artifact per shipping build type, its
    Gradle report where `_android-notices` writes it and its FFI features as
    `_android-ffi-flavor` builds them;
  * `scripts/android-package-notices.py` reads a package's entry list (APK
    `assets/`, App Bundle `base/assets/`) and fails on a missing file.

No generated notices file is opened (release-integrity.md § Reviewing
untrusted source without being subverted).
"""

from __future__ import annotations

import importlib.util
import re
import sys
import tomllib
import zipfile
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
JUSTFILE = (REPO / "justfile").read_text(encoding="utf-8")
GRADLE = (REPO / "apps" / "fauna-android" / "app" / "build.gradle.kts").read_text(encoding="utf-8")
MANIFEST = tomllib.loads((REPO / "installer" / "shipped-notices.toml").read_text(encoding="utf-8"))

_spec = importlib.util.spec_from_file_location("android_package_notices", REPO / "scripts" / "android-package-notices.py")
apn = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(apn)

# build type → (APK recipe, its gradle task, manifest artifact, FFI features as
# `_android-ffi-flavor` builds them, no-default-features?)
SHIPPING = {
    "release": ("android-release", "assembleRelease", "android-release", ["file-provider-host"], False),
    "foss": ("android-foss", "assembleFoss", "android-foss", ["file-provider-host"], False),
    "storeSafe": ("android-store-safe", "assembleStoreSafe", "android-store-safe", ["store-safe", "file-provider-host"], True),
    "storeNoPayments": ("android-store-no-payments", "assembleStoreNoPayments", "android-store-no-payments", ["store-safe", "p2p-share", "file-provider-host"], True),
    "kids": ("android-kids", "assembleKids", "android-kids", ["kids-safe", "kids-floor", "file-provider-host"], True),
}


def recipe_body(name: str) -> list[str]:
    lines = JUSTFILE.splitlines()
    head = re.compile(rf"^{re.escape(name)}(\s[^:]*)?:")
    for i, line in enumerate(lines):
        if head.match(line):
            body = []
            for nxt in lines[i + 1:]:
                if nxt and not nxt[0].isspace():
                    break
                body.append(nxt)
            return body
    raise AssertionError(f"no recipe {name} in the justfile")


def _index(body: list[str], needle: str) -> int:
    hits = [i for i, line in enumerate(body) if needle in line]
    assert hits, f"{needle!r} not in the recipe body"
    return hits[0]


@pytest.mark.parametrize("buildtype", sorted(SHIPPING))
def test_shipping_apk_recipe_stages_before_gradle_and_checks_after(buildtype):
    recipe, task, *_ = SHIPPING[buildtype]
    body = recipe_body(recipe)
    stage = _index(body, f"just _android-notices {buildtype}")
    gradle = _index(body, f"gradlew -p apps/fauna-android {task}")
    check = _index(body, f"just _android-notices-check {buildtype}")
    assert stage < gradle < check


def test_bundle_body_stages_before_gradle_and_checks_the_bundle():
    body = recipe_body("_android-bundle")
    stage = _index(body, 'just _android-notices "$VARIANT"')
    gradle = _index(body, 'gradlew -p apps/fauna-android "$TASK"')
    check = _index(body, 'scripts/android-package-notices.py "$AAB"')
    assert stage < gradle < check


def test_staging_recipe_maps_every_shipping_build_type_and_fails_closed():
    body = "\n".join(recipe_body("_android-notices"))
    for buildtype, (_, _, artifact, _, _) in SHIPPING.items():
        assert f"{buildtype}) ARTIFACT={artifact} ;;" in body
    assert "set -euo pipefail" in body
    assert 'rm -rf "$DIR"' in body  # a refused generator leaves nothing stale
    assert ':app:dependencies --configuration "${BUILDTYPE}RuntimeClasspath"' in body
    assert 'scripts/shipped-notices.py "$ARTIFACT" "$DIR/assets/THIRD-PARTY-NOTICES"' in body
    assert 'cp LICENSE-MIT LICENSE-APACHE "$DIR/assets/"' in body
    # the generator runs before the licence copy, so a refusal stops the recipe
    assert body.index("scripts/shipped-notices.py") < body.index("cp LICENSE-MIT")


@pytest.mark.parametrize("buildtype", sorted(SHIPPING))
def test_build_type_source_set_names_the_staging_directory(buildtype):
    block = re.search(rf'getByName\("{buildtype}"\) \{{(.*?)\n        \}}', GRADLE, re.S)
    assert block, f"no {buildtype} source set"
    assert f'assets.srcDir("build/shipped-notices/{buildtype}/assets")' in block.group(1)


def test_debug_source_set_takes_no_notices_directory():
    block = re.search(r'getByName\("debug"\) \{(.*?)\n        \}', GRADLE, re.S)
    assert block and "shipped-notices" not in block.group(1)


@pytest.mark.parametrize("buildtype", sorted(SHIPPING))
def test_manifest_artifact_matches_the_build_type(buildtype):
    _, _, artifact, features, no_default = SHIPPING[buildtype]
    art = MANIFEST["artifact"][artifact]
    assert art["channel"] == "Android app (every channel)"
    assert art["gradle"]["report"] == f"apps/fauna-android/app/build/shipped-notices/{buildtype}/runtime-classpath.txt"
    rust = art["rust"]
    assert rust["packages"] == ["fauna-ffi"]
    assert rust["features"] == features
    assert bool(rust.get("no_default_features")) is no_default
    assert sorted(rust["targets"]) == sorted(
        ["aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android", "i686-linux-android"]
    )


def test_no_android_artifact_outside_the_shipping_build_types():
    android = {n for n, a in MANIFEST["artifact"].items() if a["channel"] == "Android app (every channel)"}
    assert android == {v[2] for v in SHIPPING.values()}


# --- the package entry-list check ----------------------------------------------------


def _pkg(path: Path, names: list[str]) -> Path:
    with zipfile.ZipFile(path, "w") as z:
        for n in names:
            z.writestr(n, "x")
    return path


def test_package_check_passes_an_apk_with_the_three_assets(tmp_path, capsys):
    apk = _pkg(tmp_path / "app-release.apk", ["classes.dex", "assets/LICENSE-MIT", "assets/LICENSE-APACHE", "assets/THIRD-PARTY-NOTICES"])
    assert apn.main([str(apk)]) == 0
    assert "android-package-notices: OK" in capsys.readouterr().out


def test_package_check_fails_an_apk_missing_one(tmp_path, capsys):
    apk = _pkg(tmp_path / "app-release.apk", ["classes.dex", "assets/LICENSE-MIT", "assets/LICENSE-APACHE"])
    assert apn.main([str(apk)]) == 1
    assert "lacks assets/THIRD-PARTY-NOTICES" in capsys.readouterr().out


def test_package_check_reads_a_bundle_at_its_base_module(tmp_path):
    flat = ["assets/LICENSE-MIT", "assets/LICENSE-APACHE", "assets/THIRD-PARTY-NOTICES"]
    assert apn.main([str(_pkg(tmp_path / "a.aab", flat))]) == 1
    assert apn.main([str(_pkg(tmp_path / "b.aab", ["base/" + n for n in flat]))]) == 0


def test_package_check_refuses_an_unreadable_package(tmp_path):
    bad = tmp_path / "x.apk"
    bad.write_bytes(b"not a zip")
    assert apn.main([str(bad)]) == 2


def test_the_generator_finds_the_ndk_through_the_drivers_android_ffi_links_with():
    """The NDK sysroot entry is found through the compiler driver each triple's
    link runs; the generator's map is the one `android-ffi` checks on PATH
    (shipped-notices.md, the toolchain bullet)."""
    _g = importlib.util.spec_from_file_location("shipped_notices", REPO / "scripts" / "shipped-notices.py")
    sn = importlib.util.module_from_spec(_g)
    # Registered before it runs: its dataclasses resolve their string
    # annotations through `sys.modules[cls.__module__]`.
    sys.modules[_g.name] = sn
    _g.loader.exec_module(sn)
    block = re.search(r"declare -A NDK_CLANG=\((.*?)\)\n", JUSTFILE, re.S)
    assert block, "android-ffi's NDK_CLANG map is gone"
    recipe = dict(re.findall(r"\[([\w-]+)\]=([\w-]+)", block.group(1)))
    assert recipe == sn.NDK_CLANG
