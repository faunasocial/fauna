# fauna-apple — the macOS and iOS clients

SwiftUI apps for macOS and iOS built from one SwiftPM package. Both targets
share **FaunaKit** (views, view-models, utilities — most of the UI is written
once here) and consume the shared Rust core via UniFFI
(`FaunaFFISwift` wraps the generated bindings from `libs/fauna-ffi`).

## Build

Needs a Mac with Xcode (command-line tools included). From the repository
root:

```sh
just mac-debug        # macOS app (SwiftPM product FaunaMacOS)
just mac-release      # release build
just apple-ffi        # the 3-slice XCFramework (device + simulator + Mac) — PRODUCTION flavor
just apple-ffi-test   # same 3 slices WITH the e2e seams; what the iOS e2e suite needs
```

The iOS app builds from Xcode with the `Fauna-iOS` scheme of
`Fauna.xcodeproj`, in the simulator on a Mac with Apple silicon (the shared
core's only simulator slice is `aarch64-apple-ios-sim`). Device signing is set
up only for the App Store, with the project's own team, its bundle identifiers
and its app group, so a device build under another team needs those changed in
the project and the source (`docs/guides/install.md` § iOS). Xcode's
Run builds Debug, so run **`just apple-ffi-test`** first; `just apple-ffi` is
for a Release build. Every Debug build needs the test flavor, the e2e harness's
included — a Debug build compiles FaunaKit's `#if DEBUG` TestAgent, whose calls into the
`*ForTest` UniFFI seams only resolve against the test flavor
(`docs/goal/architecture/testing.md` § convention 15).

## Test

```sh
just swift-test                                      # Swift package tests (+ iOS target compile check)
pytest tests/e2e-unified/tests/ --client macos       # cross-app e2e (see tests/e2e-unified/README.md)
```

## Layout

- `FaunaKit/` — the shared Swift layer: nearly all views and view-models live here
- `Fauna-macOS/`, `Fauna-iOS/` — the thin per-platform shells
- `FaunaFFISwift/` — UniFFI bindings wrapper for the Rust core
- `Fauna-NSE/` — notification service extension (the legacy `Fauna-FinderSync` is deleted; Finder/Files integration is the File Provider extension — `docs/goal/behavior/on-demand-files-apple.md` § On-Demand Files → Apple File Provider binding)
- `Fauna-watchOS/` — watch companion (early)

UI element IDs come from `tests/e2e-unified/ui.yaml`
(`accessibilityIdentifier`) — the same IDs as every other app.
