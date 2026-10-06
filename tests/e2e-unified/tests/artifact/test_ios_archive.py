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

**What is deliberately NOT asserted, and why it is a real user gate rather than
a skipped test.** The target is configured `CODE_SIGN_STYLE = Manual` with
`AD_HOC_CODE_SIGNING_ALLOWED = NO`, so a *signed* device archive cannot be
produced without an Apple Developer identity and provisioning profile. The
identity now exists — enrollment was approved 2026-08-13 and `Apple
Distribution: Fauna Social (7457N3M72H)` has been live on macOS since
2026-08-22 — so what is missing is narrower than it was: a **provisioning
profile** for `social.fauna.fauna`, which is portal work the dev VM
deliberately cannot do (the Apple account stays off it by user ruling).
Building the archive with signing disabled is not a workaround for that; it is
the honest split. It proves everything about the archive path that does not
require the credential — the SDK, the arm64 compile, the packaging, the layout —
and leaves the credentialed step (`-exportArchive` to a signed `.ipa`, then an
install on a real device) as the one inch a human with the identity must do.
That inch is captured as a `NEEDS FROM USER:` item,
not improvised here.

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
from pathlib import Path

import pytest

from common import get_repo_root
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
