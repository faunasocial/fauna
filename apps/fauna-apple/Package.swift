// swift-tools-version: 6.0

import PackageDescription

var targets: [Target] = [
    // Shared library: Core, Models, ViewModels, Utilities
    .target(
        name: "FaunaKit",
        dependencies: ["FaunaFFISwift", "FaunaDeepLink", "FaunaExtensionKit"],
        path: "FaunaKit/Sources/FaunaKit",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // The FFI-free slice of FaunaKit an app extension may link: the
    // platform-registered identifiers (`AppleIdentifiers`), the generated i18n
    // strings (`L`), and the home-screen widget's snapshot store, timeline logic
    // and view. FaunaKit re-exports it (`FFIImport.swift`), so app code reads
    // these exactly as before; the widget appex targets link ONLY this product —
    // a widget runs under a tight memory ceiling and must not carry the Rust FFI
    // (the same reason `FaunaDeepLink` exists for the FP UI action appex).
    .target(
        name: "FaunaExtensionKit",
        path: "FaunaKit/Sources/FaunaExtensionKit",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // Widget appex sources — typecheck under `swift build` (the FaunaFPUI
    // pattern); the shipping binaries are the `.xcodeproj`'s Fauna-Widget /
    // Fauna-iOS-Widget appex targets.
    .target(
        name: "FaunaWidget",
        dependencies: ["FaunaExtensionKit"],
        path: "Fauna-Widget",
        exclude: ["Info.plist", "Fauna-Widget.entitlements", "Fauna-Widget-iOS.entitlements"],
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // FFI-free `fauna://` deep-link vocabulary (FP context actions) — shared by
    // the app-side router (via FaunaKit) and the FP UI action appex, which must
    // NOT link the Rust xcframework (a menu action shipping the whole FFI
    // binary). See Fauna-FileProviderUI/ActionViewController.swift.
    .target(
        name: "FaunaDeepLink",
        path: "FaunaKit/Sources/FaunaDeepLink",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // FP UI action appex sources — typecheck under `swift build` (the FaunaNSE
    // pattern); the shipping binaries are the `.xcodeproj`'s
    // Fauna-FileProviderUI / Fauna-iOS-FileProviderUI appex targets.
    .target(
        name: "FaunaFPUI",
        dependencies: ["FaunaDeepLink"],
        path: "Fauna-FileProviderUI",
        exclude: ["Info.plist", "Fauna-FileProviderUI.entitlements"],
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // Swift wrapper around the UniFFI-generated bindings
    .target(
        name: "FaunaFFISwift",
        dependencies: ["FaunaFFI"],
        path: "FaunaFFISwift/Sources",
        swiftSettings: [.swiftLanguageMode(.v5)],
        // The Rust FFI static lib (FaunaFFI.xcframework) transitively links the
        // `system-configuration` crate (via hickory-resolver 0.26's macOS DNS/proxy
        // detection — pulled in by the hickory 0.25→0.26.1 bump), whose
        // symbols (`SCNetworkReachability*`, `SCNetworkInterface*`, …) live in the
        // SystemConfiguration framework. SwiftPM propagates this `.linkedFramework`
        // to every dependent final link (FaunaMacOS / FaunaiOS / FaunaKitTests), so
        // the framework is the right home here beside the FFI it belongs to.
        // Available on macOS/iOS/watchOS alike.
        linkerSettings: [.linkedFramework("SystemConfiguration")]
    ),
    // UniFFI XCFramework (built by `just apple-ffi`)
    .binaryTarget(
        name: "FaunaFFI",
        path: "FaunaFFI.xcframework"
    ),
    // Tests
    .testTarget(
        name: "FaunaKitTests",
        dependencies: ["FaunaKit", "FaunaDeepLink", "FaunaExtensionKit"],
        path: "FaunaKit/Tests/FaunaKitTests",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // macOS desktop app — ALL app code lives in this library so the SPM
    // executable below and the `.xcodeproj` `Fauna` app shell (which packages the
    // File Provider appex) compile the SAME code (M2 slice 3b, option (b):
    // "shells linking SwiftPM library products" — file-sync.md § On-Demand Files
    // → Apple File Provider binding).
    .target(
        name: "FaunaMacOSLib",
        dependencies: [
            "FaunaKit",
            "FaunaDeepLink",
        ],
        path: "Fauna-macOS",
        // `Resources/LaunchAgents` is bundle layout, not a SwiftPM resource: the
        // `.xcodeproj` app target copies it to `Contents/Library/LaunchAgents`
        // for `SMAppService.agent` (`AutoStart.swift`).
        exclude: ["Fauna-macOS.entitlements", "Resources/Info.plist", "Resources/LaunchAgents"],
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // Thin `@main` shell over FaunaMacOSLib. The single source file is also
    // compiled by the `.xcodeproj` app target, so the two entry points can never
    // drift.
    .executableTarget(
        name: "FaunaMacOS",
        dependencies: ["FaunaMacOSLib"],
        path: "Fauna-macOS-Main",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // Unit tests for macOS-app types that live in FaunaMacOSLib (e.g.
    // `LocationsModel`'s reconcile logic). Runs under `swift test` on the macOS
    // host only — FaunaMacOSLib is macOS-only (AppKit).
    .testTarget(
        name: "FaunaMacOSLibTests",
        dependencies: ["FaunaMacOSLib", "FaunaKit"],
        path: "Fauna-macOSTests",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // iOS app library. Built (never run) for the macOS host by `swift build` /
    // `swift test` — its `Fauna-iOS/` source typechecks for macOS via the shims
    // in FaunaKit/Utilities/CrossPlatformUI.swift. Real iOS device/simulator
    // builds go through `xcodebuild -scheme FaunaiOS` (e2e) or the `.xcodeproj`
    // `Fauna-iOS` app target (the appex-carrying bundle, M4) — the same
    // library-product + thin-shell split as FaunaMacOSLib, so the two build
    // worlds share one entry point and cannot drift.
    .target(
        name: "FaunaiOSLib",
        dependencies: ["FaunaKit", "FaunaDeepLink"],
        path: "Fauna-iOS",
        exclude: ["Resources/Info.plist"],
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // Thin `@main` shell over FaunaiOSLib (mirrors FaunaMacOS ← FaunaMacOSLib).
    .executableTarget(
        name: "FaunaiOS",
        dependencies: ["FaunaiOSLib"],
        path: "Fauna-iOS-Main",
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
    // NB: there is deliberately no `FaunaiOSTests` SwiftPM target. Its XCUITest
    // cases need an iOS Simulator + a host app + /tmp/fauna-e2e-ios-config.json,
    // so `swift test` could only ever crash on them; and the cross-app iOS
    // UI suite drives the in-process automation server (FaunaKit/Testing —
    // clients/apple-e2e-automation.md; the apple-bridge XCUITest project was
    // deleted at the 2026-06-16 cutover), not this package. The `FaunaiOSTests/`
    // sources are kept on disk for a future Xcode-project test target
    // (tracked internally).
    // Notification Service Extension (decrypts push payloads)
    .target(
        name: "FaunaNSE",
        path: "Fauna-NSE",
        exclude: ["Info.plist"],
        swiftSettings: [.swiftLanguageMode(.v5)]
    ),
]

let package = Package(
    name: "FaunaApple",
    platforms: [
        .macOS(.v15),
        .iOS(.v17),
        .watchOS(.v10),
    ],
    products: [
        .library(name: "FaunaKit", targets: ["FaunaKit"]),
        // Linked by the `.xcodeproj` `Fauna` app target (the appex-carrying macOS
        // app bundle) — the same code the SPM `FaunaMacOS` executable wraps.
        .library(name: "FaunaMacOSLib", targets: ["FaunaMacOSLib"]),
        // Linked by the `.xcodeproj` `Fauna-iOS` app target (the appex-carrying
        // iOS app bundle) — the same code the SPM `FaunaiOS` executable wraps.
        .library(name: "FaunaiOSLib", targets: ["FaunaiOSLib"]),
        // Linked by the `.xcodeproj` FP UI action appex targets (macOS + iOS) —
        // deliberately the ONLY product they link (FFI-free).
        .library(name: "FaunaDeepLink", targets: ["FaunaDeepLink"]),
        // Linked by the `.xcodeproj` widget appex targets (macOS + iOS) —
        // deliberately the ONLY product they link (FFI-free).
        .library(name: "FaunaExtensionKit", targets: ["FaunaExtensionKit"]),
    ],
    targets: targets
)
