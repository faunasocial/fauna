"""The iOS Simulator app's build shape — one spelling for the two builds of it.

`conftest._build_ios_app` builds HEAD's app and `helpers/prev_build.py` the pinned
previous one, and a difference between them would be a second variable in a grid whose
only variable is the version. So the arguments, the package workspace and the `.app`
wrapper live here. No imports from the harness: `prev_build.py` also runs as a plain
script on the bare interpreter.
"""
from __future__ import annotations

import shutil
from pathlib import Path

#: Everything an `xcodebuild` of the SPM app takes but its workspace and derived data.
#: The generic simulator destination, arm64 only (the one simulator slice the FFI
#: ships): a build needs no device, and a bare `name=` resolves against the NEWEST
#: runtime — which, beside the pinned one, may not carry `IOS_SIM_DEVICE_NAME` at all
#: (Xcode 27.1's iOS 27.1 runtime).
SIMULATOR_BUILD_ARGS = (
    "-scheme", "FaunaiOS",
    "-destination", "generic/platform=iOS Simulator",
    "ARCHS=arm64", "ONLY_ACTIVE_ARCH=NO",
    "-skipPackagePluginValidation", "-skipMacroValidation",
)


def write_package_workspace(ws_dir: Path, package_dir: Path) -> Path:
    """A synthetic `.xcworkspace` at `ws_dir` whose sole `FileRef` is `package_dir`
    (`conftest._ios_package_workspace` says why the build needs one)."""
    ws_dir.mkdir(parents=True, exist_ok=True)
    (ws_dir / "contents.xcworkspacedata").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<Workspace version = "1.0">\n'
        f'   <FileRef location = "group:{package_dir}">\n'
        '   </FileRef>\n'
        '</Workspace>\n'
    )
    return ws_dir


#: The Swift conditions the kids build compiles under, on the command line: a
#: target's `SWIFT_ACTIVE_COMPILATION_CONDITIONS` does not reach the FaunaKit
#: package, a command-line `OTHER_SWIFT_FLAGS` does — the same three
#: `just apple-kids-check` archives `Fauna-Kids-iOS` with (the kids build is a
#: store-safe build one complement further).
KIDS_SWIFT_FLAGS = "OTHER_SWIFT_FLAGS=$(inherited) -D FAUNA_KIDS -D FAUNA_EXCISE_PAYMENTS -D FAUNA_EXCISE_P2P_SHARE"


def write_app_bundle(app_dir: Path, exe: Path, bundle_id: str, *, kids: bool = False) -> None:
    """Wrap the bare SPM executable `exe` in the `.app` bundle `simctl install` takes.

    `kids` writes the Fauna Kids shape of the plist (`Fauna-iOS/Resources/
    Info-Kids.plist`'s rule): photo backup is not in the kids app's Kept set, so
    no Photos usage string."""
    app_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(str(exe), str(app_dir / "FaunaiOS"))

    # Written UNCONDITIONALLY, and the id comes from the driver (the caller's
    # `bundle_id`) rather than a second literal. Both halves matter, and the 2026-08-12 identifier sweep
    # proved why: `derived/` persists across runs, so an `if not exists` guard
    # kept serving a bundle stamped with the PREVIOUS id while the driver
    # launched the new one — `simctl install` succeeded (it installs whatever the
    # plist says) and `simctl launch` failed with FBSOpenApplicationServiceError
    # code=4 "The request to open ... failed", a message that names the app and
    # says nothing about a stale bundle. Deriving from `IOS_BUNDLE_ID` makes the
    # pair unable to disagree at all; `test_apple_identifier_pins.py` holds both
    # to the shipping `.xcodeproj` value.
    info_plist = app_dir / "Info.plist"
    name = "Fauna Kids" if kids else "Fauna"
    photos = "" if kids else (
        "    <key>NSPhotoLibraryUsageDescription</key>\n"
        "    <string>Fauna backs up your photos to your own nest.</string>\n"
    )
    info_plist.write_text(f"""\
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>FaunaiOS</string>
    <key>CFBundleIdentifier</key><string>{bundle_id}</string>
    <key>CFBundleName</key><string>{name}</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleSupportedPlatforms</key>
    <array><string>iPhoneSimulator</string></array>
    <key>MinimumOSVersion</key><string>17.0</string>
    <key>DTPlatformName</key><string>iphonesimulator</string>
    <key>UIDeviceFamily</key>
    <array><integer>1</integer><integer>2</integer></array>
    <key>UILaunchScreen</key><dict/>
{photos}    <key>NSAppTransportSecurity</key>
    <dict>
        <key>NSAllowsArbitraryLoads</key><true/>
    </dict>
</dict>
</plist>
""")
