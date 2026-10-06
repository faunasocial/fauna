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

The iOS app builds from Xcode with the `FaunaiOS` scheme (simulator, or your
own device with your signing), after `just apple-ffi`. Use **`apple-ffi-test`**
instead whenever you build the app in Debug and drive it from the e2e harness —
a Debug build compiles FaunaKit's `#if DEBUG` TestAgent, whose calls into the
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
- `Fauna-NSE/` — notification service extension (the legacy `Fauna-FinderSync` is deleted; Finder/Files integration is the File Provider extension — `docs/goal/behavior/file-sync.md` § On-Demand Files → Apple File Provider binding)
- `Fauna-watchOS/` — watch companion (early)

UI element IDs come from `tests/e2e-unified/ui.yaml`
(`accessibilityIdentifier`) — the same IDs as every other app.
