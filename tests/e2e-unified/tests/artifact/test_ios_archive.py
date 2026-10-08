"""The iOS shipping artifact: `xcodebuild archive` of the real device target.

Subject: the `Fauna-iOS` target of `apps/fauna-apple/Fauna.xcodeproj` — the app
that goes to TestFlight and the App Store — built for a **real device**, in the
**Release** configuration, as an `.xcarchive`.

Nothing else in the fleet touches that path. iOS e2e runs simulator-only, off the
SPM package (scheme `FaunaiOS`), in Debug, and its `.app` is hand-assembled by
the harness (`conftest._build_ios_app`) rather than produced by Xcode. So every
device-only property — the iOS SDK and its arm64 slice, the Release compile, the
`.xcarchive` layout, the app's own entitlements, the extension targets embedding
into the app — is untested until this file runs. That is the same
artifact-vs-binary gap the macOS modules here close, one platform over.

**Two legs: unsigned everywhere, signed where the credential lives.** The four
iOS targets sign manually (`installers/ios.md` § Signing): their Release configs
carry the `Apple Distribution` identity, team `7457N3M72H` and one App Store
profile each, scoped `[sdk=iphoneos*]` so simulator and macOS builds keep the
project's ad-hoc `-`. The **unsigned** leg archives with `CODE_SIGNING_ALLOWED=NO`
and proves everything that needs no credential — the SDK, the arm64 compile,
the packaging, the layout. The **signed** leg archives with signing on and
`xcodebuild -exportArchive`s a signed `.ipa` through
`apps/fauna-apple/ExportOptions-AppStore.plist` (to disk, never uploaded), then
reads the signature, the embedded profiles and the entitlements back off it. The
profiles are machine-local — portal work done from the host, since the Apple
account stays off the dev VM — so a box without the identity or the four
profiles skips the signed leg as an environment gap; a real signing or export
error still fails.

**Neither leg talks to Apple's servers.** No `-allowProvisioningUpdates`, no
authentication-key flags, manual signing, `destination = export` and
`manageAppVersionAndBuildNumber = false` — the shapes that would let `xcodebuild`
reach the portal or App Store Connect are absent, and
`test_apple_identifier_pins.py` pins the tracked half of that everywhere.

**What the signed leg does not prove.** It signs the DEFAULT flavor. The archive
a store upload ships is the store-safe flavor (`just apple-ios-store-safe-check`
is its witness); signing it and the upload itself belong to the upload step, not
here. Nor is an `.ipa` an install: the install outcome waits for a channel that
serves the app.

**Cost.** A cold Release archive is minutes, and it needs the **production**
5-slice `FaunaFFI.xcframework` (`just apple-ffi`) — the host-only flavour left
behind by `just mac-debug` / `mac-app debug` / `swift-test` has no `ios-arm64`
slice and cannot link. The fixture below checks for the slice and says exactly
that rather than letting `xcodebuild` fail with an unattributed link error. It
also refuses a staged TEST flavor (`apple-ffi-test` shares the one staging slot
and carries the same device slice), so this archive can never link the
`test-helpers` seams.
"""

import plistlib
from datetime import datetime, timezone
from pathlib import Path

import pytest

from common import get_repo_root
from helpers.app_surface import skip_environment
from helpers.macos_artifact import FFI_FLAVOR_MARKER_RELPATH, ffi_flavor_refusal, run

pytestmark = [pytest.mark.tier_4, pytest.mark.ios]

#: The device slice the archive links against. `just apple-ffi-host*` (a dep of
#: mac-debug/mac-app/swift-test) reassembles the xcframework host-only, silently
#: dropping this — the clobber `apple-ffi-host-flavor-clobbers-full-xcframework`
#: describes. Re-run `just apple-ffi` LAST, after any macOS build.
_IOS_DEVICE_SLICE = "ios-arm64"

_SCHEME = "Fauna-iOS"

#: Every extension the shipping iOS app must embed, under the app's `PlugIns/`.
#: A target that stops embedding still archives cleanly — the app just ships
#: without it — so only a list read off the archive catches it:
#:   * the File Provider pair — Fauna in the Files app and its context actions;
#:   * the widget — the home-screen unread count (apps/common.md § Home-screen
#:     widget). Absent, the widget gallery offers no Fauna widget.
_REQUIRED_PLUGINS = (
    "Fauna-iOS-FileProvider.appex",
    "Fauna-iOS-FileProviderUI.appex",
    "Fauna-iOS-Widget.appex",
)


@pytest.fixture(scope="module")
def ios_device_ffi(_require_macos) -> Path:
    """The xcframework, asserted to be PRODUCTION and to carry the iOS **device** slice.

    A guard, not a build: `just apple-ffi` is a 5-slice release compile with a
    real cost, and silently triggering one inside a test fixture is how a run
    blows its per-test timeout (the documented `apple-ffi` e2e race). Fail with
    the exact recipe to run instead.

    **Production, not just present.** `apple-ffi` and `apple-ffi-test` write ONE
    staging slot, and the test flavor carries the same `ios-arm64` slice, so the
    slice check below cannot tell them apart: an archive made after an e2e run
    would link the `test-helpers` seams into the shipping binary. The
    `.ffi-flavor` marker is the only record of which one is staged, so it is
    checked first (convention 15's flavor-staleness caveat).
    """
    xcframework = get_repo_root() / "apps" / "fauna-apple" / "FaunaFFI.xcframework"
    if not xcframework.is_dir():
        pytest.fail(
            f"no FaunaFFI.xcframework at {xcframework} — run `just apple-ffi` first.",
            pytrace=False,
        )
    marker_path = get_repo_root() / FFI_FLAVOR_MARKER_RELPATH
    refusal = ffi_flavor_refusal(
        marker_path.read_text(encoding="utf-8") if marker_path.is_file() else None
    )
    if refusal:
        pytest.fail(refusal, pytrace=False)
    slices = sorted(p.name for p in xcframework.iterdir() if p.is_dir())
    if _IOS_DEVICE_SLICE not in slices:
        pytest.fail(
            f"FaunaFFI.xcframework has no {_IOS_DEVICE_SLICE!r} slice (present: "
            f"{slices}), so the iOS device archive cannot link it.\n"
            f"Cause: `just apple-ffi-host*` — a dep of `just mac-debug`, "
            f"`just mac-app` and `just swift-test` — deletes a full xcframework and "
            f"reassembles it host-only.\n"
            f"Fix: run `just apple-ffi` LAST, after any macOS build.",
            pytrace=False,
        )
    return xcframework


@pytest.fixture(scope="module")
def ios_archive(ios_device_ffi, artifact_scratch) -> Path:
    """Build the unsigned Release `.xcarchive` for a real iOS device.

    `CODE_SIGNING_ALLOWED=NO` + `CODE_SIGNING_REQUIRED=NO` are what make this
    runnable with no Developer identity. They switch off signing ONLY — the SDK,
    the architecture, the configuration and the packaging are the shipping ones,
    which is the whole point of archiving rather than plain `build`.
    """
    archive = artifact_scratch / "Fauna-iOS.xcarchive"
    run("rm", "-rf", str(archive), check=False)
    run("xcodebuild", "archive",
        "-project", str(get_repo_root() / "apps" / "fauna-apple" / "Fauna.xcodeproj"),
        "-scheme", _SCHEME,
        "-destination", "generic/platform=iOS",
        "-archivePath", str(archive),
        "-skipPackagePluginValidation", "-skipMacroValidation",
        "CODE_SIGNING_ALLOWED=NO",
        "CODE_SIGNING_REQUIRED=NO",
        timeout=3600)
    return archive


def test_the_ios_device_archive_builds(ios_archive):
    """The gated check itself: the shipping iOS target archives for a real device.

    This is the assertion that would have caught a shared-Rust change whose
    regenerated bindings break only the iOS *device* slice, a Release-only
    compile error, or an extension target that stops embedding — none of which
    the simulator Debug e2e build can see.
    """
    assert ios_archive.is_dir(), f"xcodebuild reported success but {ios_archive} is absent"
    app = ios_archive / "Products" / "Applications" / "Fauna.app"
    if not app.is_dir():
        apps = list((ios_archive / "Products" / "Applications").glob("*.app"))
        assert apps, (
            f"the archive contains no .app at all under Products/Applications: "
            f"{sorted(p.name for p in ios_archive.rglob('*'))[:40]}"
        )
        app = apps[0]
    assert (app / "Info.plist").exists(), f"{app} has no Info.plist"


def test_the_archived_app_is_a_real_device_build(ios_archive):
    """It is an iPhoneOS arm64 binary, not a simulator build wearing an archive.

    Worth its own assertion because the failure is silent: a misconfigured
    destination yields a perfectly well-formed `.xcarchive` full of simulator
    slices, which would make this whole module a vacuous green.
    """
    apps = list((ios_archive / "Products" / "Applications").glob("*.app"))
    assert apps, "no app in the archive"
    app = apps[0]

    info = plistlib.loads((app / "Info.plist").read_bytes())
    platforms = info.get("CFBundleSupportedPlatforms", [])
    assert platforms == ["iPhoneOS"], (
        f"archived app declares CFBundleSupportedPlatforms={platforms!r}, not "
        f"['iPhoneOS'] — this is not a device build."
    )
    assert info.get("CFBundleIdentifier") == "social.fauna.fauna", info.get(
        "CFBundleIdentifier"
    )

    exe = app / info["CFBundleExecutable"]
    arches = run("lipo", "-archs", str(exe)).stdout.split()
    assert "arm64" in arches, f"{exe} carries {arches}, no arm64 device slice"


def test_the_archived_app_embeds_every_extension(ios_archive):
    """The archive's app carries each extension it must ship with."""
    apps = list((ios_archive / "Products" / "Applications").glob("*.app"))
    assert apps, "no app in the archive"
    plugins = apps[0] / "PlugIns"
    present = sorted(p.name for p in plugins.glob("*.appex")) if plugins.is_dir() else []
    missing = [name for name in _REQUIRED_PLUGINS if name not in present]
    assert not missing, (
        f"the archived app is missing {missing} under {plugins} (present: {present}) "
        f"— the target stopped embedding them, and nothing but this list notices."
    )


def test_the_archive_declares_the_apps_entitlements(ios_archive):
    """The entitlements the shipping app depends on survive into the archive.

    Unsigned, so they live in the archive's `Info.plist` rather than in a code
    signature — but their presence is still the check that matters: the app group
    is how the iOS app and its File Provider extension share state, and losing it
    breaks file access at runtime with no build-time symptom.
    """
    plist = plistlib.loads((ios_archive / "Info.plist").read_bytes())
    props = plist.get("ApplicationProperties", {})
    assert props.get("ApplicationPath"), f"archive Info.plist has no ApplicationProperties: {plist}"

    src = (get_repo_root() / "apps" / "fauna-apple" / "Fauna-iOS" / "Resources"
           / "Fauna-iOS.entitlements")
    assert src.exists(), f"the target's entitlements file is gone: {src}"
    ents = plistlib.loads(src.read_bytes())
    assert ents.get("com.apple.security.application-groups"), (
        f"the iOS target declares no app group; the app and its File Provider "
        f"extension share state through it. Got: {sorted(ents)}"
    )


# ---------------------------------------------------------------------------
# The signed leg: archive with signing on, export a signed `.ipa`, read it back.
# ---------------------------------------------------------------------------

_TEAM_ID = "7457N3M72H"
_DIST_IDENTITY = f"Apple Distribution: Fauna Social ({_TEAM_ID})"
_APP_GROUP = "group.social.fauna.shared"
#: Every bundle in the `.ipa`, by its path under `Payload/`, with its bundle id
#: and whether its signature must claim the app group. The iOS File Provider UI
#: extension declares no entitlements at all (it opens a `fauna://` URL and
#: touches no shared state), so its signature carries no group.
_SIGNED_BUNDLES = {
    "Fauna-iOS.app": ("social.fauna.fauna", True),
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProvider.appex":
        ("social.fauna.fauna.FileProvider", True),
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProviderUI.appex":
        ("social.fauna.fauna.FileProviderUI", False),
    "Fauna-iOS.app/PlugIns/Fauna-iOS-Widget.appex":
        ("social.fauna.fauna.Widget", True),
}
_EXPORT_OPTIONS = Path("apps") / "fauna-apple" / "ExportOptions-AppStore.plist"


def _profile_name(bundle_id: str) -> str:
    return f"Fauna AppStore {bundle_id}"


def _decode_profile(path: Path) -> dict:
    return plistlib.loads(run("security", "cms", "-D", "-i", str(path)).stdout.encode())


def _installed_profiles() -> dict[str, dict]:
    """Every unexpired profile of this team installed for Xcode, by Name.

    Matched by NAME, never UUID: a profile's UUID changes every time it is
    regenerated (yearly, and whenever its App ID gains a capability), while the
    name is what the project and the export options reference.
    """
    root = Path.home() / "Library" / "Developer" / "Xcode" / "UserData" / "Provisioning Profiles"
    now = datetime.now(timezone.utc).replace(tzinfo=None)
    found: dict[str, list[dict]] = {}
    for path in sorted(root.glob("*.mobileprovision")) if root.is_dir() else []:
        prof = _decode_profile(path)
        if _TEAM_ID in prof.get("TeamIdentifier", []) and prof["ExpirationDate"] > now:
            found.setdefault(prof["Name"], []).append(prof)
    dupes = {name: len(v) for name, v in found.items() if len(v) > 1}
    if dupes:
        pytest.fail(f"more than one unexpired profile per name: {dupes} — Xcode's "
                    f"pick between them is undefined; remove the stale copies.",
                    pytrace=False)
    return {name: v[0] for name, v in found.items()}


@pytest.fixture(scope="module")
def signing_credentials() -> dict[str, dict]:
    """The distribution identity and the four App Store profiles, or a skip."""
    identities = run("security", "find-identity", "-v", "-p", "codesigning").stdout
    if f'"{_DIST_IDENTITY}"' not in identities:
        skip_environment(f"no {_DIST_IDENTITY!r} signing identity in this keychain")
    profiles = _installed_profiles()
    missing = sorted({_profile_name(b) for b, _ in _SIGNED_BUNDLES.values()} - set(profiles))
    if missing:
        skip_environment(f"App Store provisioning profiles not installed: {missing}")
    return profiles


@pytest.fixture(scope="module")
def signed_ios_archive(ios_device_ffi, signing_credentials, artifact_scratch) -> Path:
    """The Release device archive with signing ON — manual, profiles from disk.

    Deliberately no `-allowProvisioningUpdates` and no authentication-key flags:
    without them xcodebuild resolves the identity and the profiles locally and
    treats a miss as an error, never as a reason to contact Apple.
    """
    archive = artifact_scratch / "Fauna-iOS-signed.xcarchive"
    run("rm", "-rf", str(archive), check=False)
    run("xcodebuild", "archive",
        "-project", str(get_repo_root() / "apps" / "fauna-apple" / "Fauna.xcodeproj"),
        "-scheme", _SCHEME,
        "-configuration", "Release",
        "-destination", "generic/platform=iOS",
        "-archivePath", str(archive),
        "-skipPackagePluginValidation", "-skipMacroValidation",
        timeout=3600)
    return archive


@pytest.fixture(scope="module")
def exported_ipa(signed_ios_archive, artifact_scratch) -> Path:
    """`-exportArchive` to a signed `.ipa` on disk; returns its unzipped `Payload/`."""
    out = artifact_scratch / "Fauna-iOS-export"
    run("rm", "-rf", str(out), check=False)
    run("xcodebuild", "-exportArchive",
        "-archivePath", str(signed_ios_archive),
        "-exportPath", str(out),
        "-exportOptionsPlist", str(get_repo_root() / _EXPORT_OPTIONS),
        timeout=1800)
    ipas = sorted(out.glob("*.ipa"))
    assert len(ipas) == 1, (
        f"expected exactly one .ipa in {out}, got {sorted(p.name for p in out.iterdir())}"
    )
    unzipped = artifact_scratch / "Fauna-iOS-ipa"
    run("rm", "-rf", str(unzipped), check=False)
    run("ditto", "-x", "-k", str(ipas[0]), str(unzipped))
    payload = unzipped / "Payload"
    assert payload.is_dir(), f"{ipas[0]} has no Payload/"
    return payload


def _signed_entitlements(bundle: Path) -> dict:
    out = run("codesign", "-d", "--entitlements", "-", "--xml", str(bundle)).stdout
    return plistlib.loads(out.encode()) if out.strip() else {}


def test_the_exported_ipa_is_signed_by_the_distribution_identity(exported_ipa):
    """Every bundle in the `.ipa` verifies, signed by Apple Distribution for the team."""
    run("codesign", "--verify", "--deep", "--strict", str(exported_ipa / "Fauna-iOS.app"))
    for rel in _SIGNED_BUNDLES:
        info = run("codesign", "-dvv", str(exported_ipa / rel)).stderr
        assert f"Authority={_DIST_IDENTITY}" in info, (
            f"{rel} is not signed by {_DIST_IDENTITY}:\n{info}"
        )
        assert f"TeamIdentifier={_TEAM_ID}" in info, f"{rel}:\n{info}"


def test_every_bundle_embeds_its_app_store_profile(exported_ipa):
    """Each bundle carries the App Store profile for its own bundle id — no device list."""
    for rel, (bundle_id, _) in _SIGNED_BUNDLES.items():
        embedded = exported_ipa / rel / "embedded.mobileprovision"
        assert embedded.is_file(), f"{rel} embeds no provisioning profile"
        prof = _decode_profile(embedded)
        assert prof["Name"] == _profile_name(bundle_id), (rel, prof["Name"])
        assert prof["TeamIdentifier"] == [_TEAM_ID], (rel, prof["TeamIdentifier"])
        assert prof["Entitlements"]["application-identifier"] == f"{_TEAM_ID}.{bundle_id}", rel
        assert "ProvisionedDevices" not in prof, (
            f"{rel} embeds a device-list profile — not an App Store one"
        )


def test_the_signed_entitlements_carry_the_app_group(exported_ipa):
    """The app group survives into the SIGNATURE, where the OS actually reads it.

    The unsigned leg can only check the source file; this is the check that the
    app and its File Provider extension will really share a container on device.
    """
    for rel, (bundle_id, wants_group) in _SIGNED_BUNDLES.items():
        ents = _signed_entitlements(exported_ipa / rel)
        assert ents.get("application-identifier") == f"{_TEAM_ID}.{bundle_id}", (rel, ents)
        assert ents.get("get-task-allow") in (None, False), f"{rel} is debuggable: {ents}"
        groups = ents.get("com.apple.security.application-groups", [])
        if wants_group:
            assert groups == [_APP_GROUP], f"{rel} signed groups {groups}, want [{_APP_GROUP!r}]"
        else:
            assert not groups, (
                f"{rel} declares no entitlements, yet its signature claims {groups}"
            )
