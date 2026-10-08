# iOS App — target state

Owns: ios
Status: ratified
Authority: ios-app architecture — the iOS shell (tab-bar + More-hub navigation, scene-phase lifecycle, BGProcessingTask scheduling, APNs + NSE push path, simulator/packaging posture) and the FaunaKit shared-layer inventory (the single home; [`macos.md`](macos.md) mirrors by pointer) plus the one apple credential-storage mechanics section; cross-app behavior → [`common.md`](common.md); credential-storage contract → `common.md` § Credential storage; e2e driving → [`apple-e2e-automation.md`](apple-e2e-automation.md); transport → [`../transport.md`](../transport.md); the nav model → `docs/goal/ui/README.md` § Navigation.

## Implementation status today

Broadly implemented. Declared gaps: none on the transport — the legacy URLSession `WebSocketClient` is deleted outright (2026-07-17; `macos.md` § Implementation status today records the same, and a code check 2026-09-19 finds no `WebSocketClient` under `apps/fauna-apple` outside generated code), so FaunaKit rides the shared Rust WS-RPC client via `FfiNestClient` alone (owner of that story: `common.md` § WS-RPC client binding); the credential-storage target below (device-bound default + opt-in iCloud sync toggle) **landed 2026-07-19** — the store now writes `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` by default with an opt-in "Back up identity to iCloud Keychain" toggle (`settings-icloud-backup-toggle`; § Credential Storage); and the sync convergence has **landed** (2026-07-13, B1 + B2 — `behavior/sync-engine-deployments.md` § Apple apps — convergence design): the bespoke Swift `SyncEngine` is deleted, the shared `fauna-sync-engine` runs in-process over `FfiSyncEngineHost`, the hardcoded `"documents"` pull is gone (its `social.fauna.sync.pull` `BGProcessingTask` successor, a one-shot host pass, retired too 2026-09-25 — § BackgroundScheduler), and photo backup is the engine's **sealed** per-file ingest. **B3 also landed 2026-07-13** — the photo set is no longer the hardcoded `"photos"` literal; it resolves through the shared `fauna_folders_machine::photo_library` adopt-or-create resolver onto the wizard-created Photo-Library set (`docs/goal/ui/folders.md` § Photo backup owns the model). **B4 also landed 2026-07-14** (was stale here — the residual it closed: iOS now starts/stops the host's resident engines on scene phase; see § Scene Phase Handling below). The apple sync-convergence B-track (B1–B5) is now fully closed. **2026-07-19 (A4 cutover, `sync-agent.md` § Scope per platform):** macOS's resident sync engines moved out of FaunaKit into the separate always-on per-user `fauna-sync-agent` process — the in-app macOS host is now one-shot-only. **iOS is explicitly unaffected**: it keeps the full in-app resident host described in this doc (no daemon on iOS; `BGProcessingTask` + the File Provider extension unchanged). **Store-safe flavor (payments excision): apple joined 2026-08-15**, the first non-Rust shell — mechanism, recipes, and witness owned by [`../dynamic-features.md`](../dynamic-features.md) § Platform-family surface excision, not restated here.

## Goal

A SwiftUI iOS 17+ app that consumes the shared Rust core through `FaunaFFI.xcframework` (UniFFI), follows the cross-app behavior in `common.md`, shares FaunaKit with macOS, and adds iOS-specific subsystems — APNs push with a Notification Service Extension for end-to-end-encrypted previews, photo backup, and a `BGProcessingTask`-driven background scheduler. How a user obtains it — TestFlight, then the App Store — is owned by [`../installers/ios.md`](../installers/ios.md).

---

## Tech Stack

| Component | Choice |
|-----------|--------|
| Language | Swift 6.0 (strict concurrency) |
| UI framework | SwiftUI |
| Minimum deployment | iOS 17.0 |
| Package manager | SPM — `Package.swift`, no external dependencies (the package pins no remote dependency at all) |
| Persistence | SwiftData |
| State management | `@Observable` / `@MainActor` ViewModels |
| Credential storage | Keychain (§ Credential Storage below) |
| Rust bindings | `FaunaFFI.xcframework` via UniFFI |

---

## Workspace Layout

```
fauna-apple/
├── Package.swift              # SPM manifest — FaunaKit + app/extension targets
├── FaunaKit/
│   └── Sources/FaunaKit/      # Shared library (all apple targets)
│       ├── Core/              # FaunaClient, APIClient, SyncStatesStore, WebSocketClient,
│       │                      #   PhotoBackupEngine, KeychainStore, SettingsPage, AdminPage
│       ├── Models/            # SwiftData model types
│       ├── ViewModels/        # Shared @Observable ViewModels
│       ├── Views/             # Shared SwiftUI views (the priority-#2 lift — most
│       │                      #   controls render from here on BOTH apple apps)
│       ├── Testing/           # AutomationRegistry + InProcessAutomationServer + TestAgent
│       │                      #   (owner: apple-e2e-automation.md)
│       ├── Generated/         # Generated sources (UiIds.swift, Providers.swift)
│       └── Utilities/         # NetworkMonitor, keychain wrappers, helpers
│   └── Sources/FaunaExtensionKit/  # The FFI-free slice an app extension may link, re-exported
│                              #   by FaunaKit: AppleIdentifiers, the generated i18n L.swift,
│                              #   and the home-screen widget's snapshot/timeline/view
├── FaunaFFISwift/             # FFICompat.swift compatibility layer
├── Fauna-iOS/                 # iOS app target (App/, Views/, Resources/)
├── Fauna-iOS-Main/            # Thin @main shell over FaunaiOSLib, compiled by BOTH the
│                              #   SwiftPM executable and the .xcodeproj app target so the
│                              #   two build worlds share one entry point (mirrors macOS)
├── Fauna-NSE/                 # Notification Service Extension
├── Fauna-macOS/               # macOS app target (shares FaunaKit)
├── Fauna-macOS-Main/          # Thin @main shell over FaunaMacOSLib (same split as iOS)
├── Fauna-macOSTests/          # SwiftPM `FaunaMacOSLibTests` target (sync-agent-health,
│                              #   folder-reconcile, sync-notification-forwarding tests)
├── Fauna-FileProvider/        # Shared macOS+iOS File Provider extension (Finder/Files
│                              #   on-demand — on-demand-files.md § Apple File Provider binding;
│                              #   replaces the deleted Fauna-FinderSync)
├── Fauna-FileProviderUI/      # File Provider UI action extension (Share / Version history
│                              #   context-menu actions; links only the FFI-free shared
│                              #   deep-link module, never the Rust xcframework)
├── Fauna-Widget/              # Shared macOS+iOS home-screen widget extension (a WidgetKit
│                              #   shell linking only FaunaExtensionKit — § Home-screen widget)
├── Fauna.xcodeproj/           # Thin hand-maintained project emitting Fauna.app + the
│                              #   embedded .appex (installers/macos.md; SwiftPM can't emit .appex)
└── Fauna-watchOS/ + Fauna-watchOS-Widgets/   # watchOS targets (platforms: .watchOS(.v10))
```

(`FaunaiOSTests/` also exists on disk — retired legacy XCUITest sources kept for a possible future Xcode-project test target, deliberately not a SwiftPM target; the current iOS e2e path is the in-process automation server below, not XCUITest.)

## FaunaKit: Shared Library

FaunaKit is the shared Swift package consumed by every apple target. No external Swift package dependencies in the iOS app target — third-party logic runs through FaunaFFI (Rust). **This section is the shared-layer inventory home; `macos.md` points here.**

| Module | Contents |
|--------|----------|
| `Core` | `FaunaClient`, `APIClient`, `SyncStatesStore` (per-file badge state off the engine host), `PhotoBackupEngine`, `KeychainStore`, the shared `SettingsPage`/`AdminPage` enums |
| `Models` | SwiftData model definitions |
| `ViewModels` | `@Observable` ViewModels shared between iOS and macOS (`ProvisioningVM`, `AdminVM`, `ConversationsVM`, machine-backed VMs over `FfiNestClient`, …) |
| `Views` | Shared SwiftUI views — most page content renders from here on both apple apps (priority #2) |
| `Feed` | Feed engagement-cue capture (`CueTracker`, `CueViewportObserver`) + the shared compose-attach button |
| `FileProvider` | The shared macOS+iOS File Provider extension's Swift-side pieces (path mapping, eviction diffing, domain ownership, the app-group credential store) — owner: `../../behavior/on-demand-files.md` § On-Demand Files → Apple File Provider binding |
| `Testing` | The in-process automation registry/server (owner: [`apple-e2e-automation.md`](apple-e2e-automation.md)) |
| `Generated` | Generated sources (`UiIds.swift`, `Providers.swift`) |
| `FaunaExtensionKit` (its own SwiftPM target, re-exported by FaunaKit) | The FFI-free slice an app extension may link: `AppleIdentifiers` (the platform-registered ids), the generated i18n `L.swift` (the ONE Swift i18n target, shared macOS+iOS), and the home-screen widget's snapshot store, timeline and view (§ Home-screen widget). An extension that links FaunaKit carries the whole Rust FFI; the widget must not |
| `Utilities` | `NetworkMonitor`, keychain wrappers, date/encoding helpers |

### FaunaClient

`FaunaClient` is the central coordinator: `APIClient` (nest calls), the `FfiNestClient` WS-RPC plane (reconnect supervisor + subscriptions — `common.md` § WS-RPC client binding owns the story), the in-process **sync engine host** (`syncHost`, built in `start()` and held for the session — dropping it stops every engine), `NetworkMonitor`. Initialized on launch from Keychain credentials; suspended/resumed with scene phase. (The legacy standalone `MlsManager` singleton — an idle, zero-caller MLS engine — was retired 2026-07-19; see § FFICompat.swift below for where MLS actually rides.)

### Platform Branches in FaunaKit

| Feature | iOS | macOS |
|---------|-----|-------|
| `PhotoBackupEngine` | Yes — PhotoKit export, scheduled via `BackgroundScheduler` | Yes — same shared engine, driven from the Folders surface (no `BackgroundScheduler`) |
| `BackgroundScheduler` | Yes — wraps the background tasks (§ BackgroundScheduler) | No |
| Directory watcher | No | No — moved out of FaunaKit into the separate always-on `fauna-sync-agent` process (2026-07-19 A4 cutover); the in-app host (`FaunaClient.startSyncHost()`) runs construct-run-drop work only and holds no watcher (comparison owner: [`sync-agent.md`](sync-agent.md) § Scope per platform) |
| Menu bar | No | Yes |
| Sync daemon | No | Yes — but as of the A4 cutover this is the external per-user `fauna-sync-agent` process FaunaKit provisions/spawns (`LaunchdSyncAgentSpawner`), not an in-app resident engine ([`sync-agent.md`](sync-agent.md)) |

One `Package.swift`; `#if os(iOS)` gates the platform-specific paths.

---

## UniFFI: Rust → Swift Bindings

`FaunaFFI.xcframework` is **not** git-tracked — it is built locally by `just apple-ffi` (with a prebuilt-artifact cache), and the e2e harness builds it automatically on `--app ios`/`--app macos`. Which slices it carries is owned by [`../build-target-layout-macos.md`](../build-target-layout-macos.md) § *apple-ffi slice set — the watch slices are opt-in*.

`FaunaFFI.swift` is auto-generated by UniFFI — never edit by hand; regenerate via `just apple-ffi`.

### FFICompat.swift

`FFICompat.swift` (`FaunaFFISwift/Sources/FFICompat.swift`) is the hand-written compatibility layer between the generated bindings and FaunaKit: hex-string / Data helpers so callers never touch raw `RustBuffer` values (keypair/QR/identity helpers, `build_auth_request(secretHex)`, `build_register_request`, signed-email build/decode, `chunk_file` / `chunk_file_at_path` / `extract_chunks`, content hashes, `install_nest_identity_pin_store`). **See the file for the current surface — an enumerated export table re-rots on every FFI change.** MLS rides the typed fauna-ffi conversations session (its own per-session `MlsEngine` on `conv-mls.db`), not FFICompat — the legacy standalone `MlsManager` singleton (an idle, zero-caller second MLS engine on its own `mls.db`) was retired 2026-07-19 as dead code (never wired on iOS; macOS-only vestige).

---

## App Entry

`FaunaApp.swift` is the `@main` entry point: creates the SwiftData `ModelContainer`, reads the actor secret from Keychain, and enters the main tab view (credentials present) or onboarding (absent).

### In-app routes (`fauna://`) — one FaunaKit door fed by the OS's URL open (ratified 2026-10-02, the Apple leg of the same-device handoff; owner for macOS and iOS)

The grammar is the one shared `fauna_core::app_route` (navigation only; the consent route's mechanism is [`../../behavior/authorization-server.md`](../../behavior/authorization-server.md) § Consent → *How the same-device handoff is built*). Both Apple apps already register the `fauna` URL scheme (`CFBundleURLTypes` in each `Info.plist`), so a route reaches the running app, or launches it, as a SwiftUI `onOpenURL` — a real OS handler, unlike tui's launch argument ([`tui.md`](tui.md) § System integration → *In-app routes*). `onOpenURL` hands the URL to one FaunaKit door, `ConsentHandoff.receive`, which parses only through the exported `parseAppRoute` — never a second parser in Swift. An unparseable URI is dropped, never an error surface, and a route that is not the consent route (the two share routes have no producer on Apple) is not this door's: it falls through to the File Provider context actions' `FaunaDeepLink`. A consent route that arrives signed out is held in the door and applied when the session becomes authenticated. Applying it navigates to Settings → Connected apps (macOS: the Settings rail slot; iOS: the More hub's Settings push) and stages the `request_uri`; the page's view-model then calls the shared machine's `open_handoff` once its machine is built and clears the staged request only after the call returns, which reveals the card in the requests tray — a miss is the page's one `connected_apps.error_handoff_expired` message. The e2e seam is the `open_route` automation command (`#if DEBUG`, shared FaunaKit handler), which feeds the same door and waits for the open to finish, so the card is painted when the command acks. [`macos.md`](macos.md) § App Entry points here.

### Scene Phase Handling

| Phase | Action |
|-------|--------|
| `.active` | Resume `FaunaClient` — reconnect, restart sync polling, clear badge |
| `.background` | Suspend `FaunaClient` — pause active work; hand off to `BackgroundScheduler` |
| `.inactive` | No action |

**Sync engine across scene phases:** nothing to pause. The in-app `FfiSyncEngineHost` holds no
resident engine — every call builds, uses and drops the engine it needs, so no watcher or worker
thread outlives a call into a background slice (`../../behavior/sync-engine-deployments.md` § Apple
apps — convergence design). `suspend()` keeps the host; `resume()` only retries building it if
`start()` failed. (The B4 resident pause/resume this replaced retired with the resident half,
2026-09-25.)

**Mail-backup foreground push-kick — DELETED 2026-08-15.** `resume()`/`suspend()` used to also
start/stop `MailBackupPushKick`, the low-latency foreground sibling of the periodic
`social.fauna.sync.upload` `BGProcessingTask` leg below. Both went at the segment-backup slice-5
flip: the **source nest** is the writer, so iOS drives no segment-backup upload at all now (owner:
`backup-restore.md` § Background Tasks → *Flip status (slice 5)*; android's peer went the same day).
⚠ The `social.fauna.sync.upload` task id itself is **retained** — it is shared with photo backup,
whose leg still runs — so the flip narrowed `handleUploadTask` rather than cancelling the task.

---

## Persistence: SwiftData

SwiftData for all structured local storage (`@Model` macros, shared `ModelContainer`, `@MainActor` access; background writers use a background context).

---

## iOS-Specific Features

### Photo Backup

`PhotoBackupEngine` (FaunaKit Core) exports photos from the device Photos library into the user's "Photo Library" folder — the folder model + controls placement are owned by `docs/goal/ui/folders.md` (`photo-backup` registry row); upload mechanics ride the chunked sync plane (`behavior/file-sync.md`). Triggered by `BackgroundScheduler` when on Wi-Fi. **The sealed ingest landed 2026-07-13 (B2):** each asset is exported to a temp file, run through the shared metadata strip, and pushed through `FfiSyncEngineHost.ingestFile` — the shared engine's sealed per-file ingest (stage → sealed chunk pipeline + `changes.record` → delete the temp; no watch dir, so deleting the staged copy cannot tombstone the ingested file). Stripping happens *here*, on the ingress copy, and never inside the engine — a folder-sync engine must keep the user's files byte-exact. **HEIC — what PhotoKit exports by default — is covered as of 2026-08-01** (it was a declared residual until then, which bit hardest on iOS). Per-container coverage, including which containers are still residuals, is owned by `../../behavior/sync-engine-deployments.md` § Ingress metadata-strip convergence — read the table there; do not restate coverage here. **The target-set fix also landed 2026-07-13 (B3):** the hardcoded `"photos"` literal is retired — the ingress resolves its set through the shared `fauna_folders_machine::photo_library` adopt-or-create resolver onto the wizard-created Photo-Library set, honoring the legacy-set adoption rule (`behavior/sync-engine-deployments.md` § Apple apps — convergence design). The `PhotoBackupRecord` PhotoKit-dedup ledger stays.

### Files App (File Provider) — ratified 2026-07-18, landed 2026-07-19 (M0–M5)

The Files app has a **Fauna location** — one File Provider domain per folder this device accepts, placeholders by default, hydrate-on-open, open-in-place — via the **shared macOS+iOS File Provider extension** (the same extension + engine host as Finder on macOS; the first file-level set access on iOS, which today is otherwise Media-page-only). Everything load-bearing is owned elsewhere: behavior + the extension-hosts-the-engine model → `../../behavior/on-demand-files.md` § On-Demand Files → Apple File Provider binding (its implementation-status prose is authoritative for what remains — the mechanism is headlessly proven through M5; only the signed end-to-end appex run stays gated on the org's Apple Developer cert); the `folder-on-demand-toggle` UI → `../../ui/folders.md` § Binding; packaging (thin `.xcodeproj` → `.appex`) → `../installers/macos.md`; milestones (iOS is the fourth) → the 2026-07-18 design record (tracked internally). iOS-specific here: extension state lives in the existing App Group container (the same shared-Keychain pattern `Fauna-NSE` already uses), and the extension runs engines on-demand under `fileproviderd` — no daemon, consistent with § BackgroundScheduler's no-always-on rule.

### Push Notifications (APNs)

`PushManager` (FaunaKit Core) handles APNs registration, per-device key exchange, and notification settings.

- Registers with APNs on launch when permission is authorized; the device token registers with the nest over the WS-RPC push kinds — `fauna.push.{vapid_key,subscribe,unsubscribe}` via the typed FFI push client (`APIClient.pushSubscribe` → `pushClient().subscribe(...)`). The push transport/dispatch/content-encryption contract is owned by `common.md` (`push-notifications` registry row).
- Generates a push-specific P-256 key pair + auth secret on first registration, stored in shared Keychain (App Group `group.social.fauna.shared`) for NSE access.

**Notification Service Extension (`Fauna-NSE`):** decrypts the `encrypted_payload` before display, falling back to a generic "New message" / "You have a new notification." when decryption fails (`NotificationService.swift`); reads the push private key from shared Keychain. The payload-crypto contract itself (RFC 8291 aes128gcm) is the push owner's (`common.md` § Push notifications) — this target implements the decrypt side with CryptoKit.

**Badge count:** the nest includes the unread count in each push; cleared on `.active`.

### BackgroundScheduler

| Task ID | Type | Work |
|---------|------|------|
| `social.fauna.sync.upload` | `BGProcessingTask` | Photo backup only (`syncNewPhotos()`) since the 2026-08-15 slice-5 flip — the task previously also drove a mail-segment backup coordinator pass (`runAllTuples()`) first, but that leg is gone (§ Scene Phase Handling above, *Mail-backup foreground push-kick*); the task id itself stays registered/rescheduled, shared with photo backup's leg. Needs `UIBackgroundModes` `processing`. |
| `social.fauna.sync.custodian` | `BGProcessingTask` | The client-device backup custodian's periodic pull pass — `CustodianBackupEngine.runPass()` (driven through `APIClient.custodianHost`, not the file-sync engine host). Added with the iOS custodian leg; owner: `../behavior/backup-destinations.md` § Third destination kind. Needs `UIBackgroundModes` `processing`. |
| `social.fauna.widget.refresh` | `BGAppRefreshTask` | The home-screen widget's count while the app is suspended — one conversations receive pass (`ConversationsVM.receivePass`, through `runWidgetRefreshPass()`, the handler's body), whose ingest republishes the count (§ Home-screen widget). Needs `UIBackgroundModes` `fetch`. |

The two processing tasks require network connectivity, neither requires external power (`requiresExternalPower = false`; battery posture is enforced via `setAcPower(false)` on the backup lease instead). Tasks re-schedule from their own completion handlers, registered together in `BackgroundScheduler.registerTasks()`; `start()` schedules the custodian and widget tasks and every `.background` `suspend()` schedules all three (`FaunaClient.swift`). The widget refresh is an app-refresh task instead — the short, network-only kind — resubmitted from its own handler no earlier than 15 minutes out. The Info.plist `BGTaskSchedulerPermittedIdentifiers` lists exactly these three ids — no fewer (an undeclared id cannot register) and no more (a declared id with no handler lets a request an older build left pending launch into nothing); `tests/e2e-unified/tests/test_apple_identifier_pins.py` pins both directions. **The background modes follow the task kinds.** The Info.plist `UIBackgroundModes` declares `processing` (the two `BGProcessingTaskRequest`s) and `fetch` (the `BGAppRefreshTaskRequest`) — `BGTaskScheduler.submit` refuses a request whose kind's mode is undeclared (`BGTaskSchedulerErrorCodeNotPermitted`, Apple's documented Background-processing / Background-fetch capability requirement), and a task-kind mode declared with no task of that kind is an unused capability; the same pin file derives each id's kind from the type `registerTasks()` casts its handler to and checks the plist against it. **A refused submit is logged, never swallowed** (`BackgroundScheduler.submit`, the `background-scheduler` `os.Logger` category; the pin file also forbids a bare `try?` submit): a task that was never scheduled looks exactly like one waiting for the OS to wake it, and until this was fixed the plist declared only `fetch` while both submits discarded their error. The refusal itself has not been observed on a device — it rests on Apple's documented rule — so the first real-device run of a build carrying the fix confirms the two processing requests are accepted by the *absence* of a `BGTaskScheduler refused` line. *(Superseded IDs: `com.fauna.photo-backup` / `social.fauna.sync-refresh` and the `BGAppRefreshTask` variant; and `social.fauna.sync.pull`, a one-shot pass over in-process location bindings, retired 2026-09-25 with the host's resident half — no app binds a location in-process.)*

---

## Home-screen widget

The apple mechanism behind `common.md` § Home-screen widget (the cross-app promise, which owns *which* number: the conversations list's own unread total), one implementation for both apps. **The widget never computes the count.** A WidgetKit widget is a separate, sandboxed extension process, and a sandboxed extension must never reach the identity seed (`../installers/macos.md` § Identifier domain) — so it holds no credential and could not open the sealed messages the count is folded from. The app publishes, the widget renders what the app last wrote: android's shape (`android.md` § App Widgets), and linux's one-computation rule (`linux.md` § Home-screen widget — the badge fed by the tray's `sum_unread`).

- **The count** — `ConversationsVM`'s manager observer, the one place every render of the conversations list reads through, calls `WidgetUnreadPublisher.publish(unread:)` with the shared `fauna_conversations` snapshot's per-thread `unreadCount` summed, on every snapshot tick. The publisher writes an `UnreadSnapshot` (`{count, updatedAt}`, `unread.json`) into `<shared app-group container>/Widget` only when the total changed, then asks WidgetKit to reload the widget's timeline. `ActorScope.resetSharedState` clears it, so a switch or sign-out never leaves the outgoing account's count on the home screen.
- **Background currency — nothing polls.** The snapshot moves whenever the conversations snapshot does. On macOS that is whenever the app runs: its receive path keeps the manager current with the window closed (a macOS window close is never a quit, `macos.md` § App Lifecycle → *Window close*); a *quit* app publishes nothing, which is why macOS's background currency rests on the app being resident from sign-in — auto-start at sign-in, with the residual an explicit Quit leaves declared there (`macos.md` § App Lifecycle → *Auto-start at sign-in*). iOS suspends a backgrounded app, so there the `social.fauna.widget.refresh` `BGAppRefreshTask` (§ BackgroundScheduler) runs one receive pass — `ConversationsVM.receivePass`, the session's own `pollConversations` + `pollMail` backstop — whose ingest ticks the same observer.
- **The widget** — `Fauna-Widget/` is a shell: one small-family `StaticConfiguration` whose kind is `AppleIdentifiers.unreadWidgetKind` (the OS keys every placed widget on it, so it never changes), a provider that reads the snapshot and hands WidgetKit one entry with a `.never` reload policy (reloads come from the app's writes; waking a widget that cannot compute the count would only re-read the same file), and `UnreadWidgetView` — the brand line, the count, the shared `widget.unread_label` and the compose line, tap-to-open. It links only `FaunaExtensionKit`. Two appex targets build it, `Fauna-Widget` (macOS) and `Fauna-iOS-Widget`, each with its own entitlements naming only the shared app group — sandboxed, no network, never the account keychain group.
- **Testing** — `HomeScreenWidgetTests` (`just swift-test`) pins the store, the entry the widget shows for a seeded snapshot, and what the publisher writes; the e2e witness `test_home_screen_widget.py` reads the snapshot from outside the app against the app's own thread list, and on macOS again with the window closed. iOS's background leg is witnessed by `test_home_screen_widget_background_refresh.py`: the `social.fauna.widget.refresh` handler's body is the public `BackgroundScheduler.runWidgetRefreshPass()` (a `BGAppRefreshTask` has no public initializer), which the e2e agent's widget-refresh poke drives — convention 14's `run_now`, the photo-backup scheduled pass's shape — and whose own pass counters (bumped by that method alone, with the unread total the pass left, `null` when it had no session to poll) attribute the pass to the poke. The pass is wired by the same seam install on the production and e2e real-session login paths. Under e2e the publisher writes only to the harness-named `FAUNA_E2E_WIDGET_DIR` (the app-group container resolves to the real home even under `CFFIXED_USER_HOME` — measured 2026-09-26), a DEBUG-only read through `E2eEnv`.

---

## Views

Feature folders under `Fauna-iOS/Views/` mirror the macOS view set: `Admin`, `Backups`, `Contacts`, `Conversations` (groups live here — no separate Groups folder), `Devices`, `Events`, `Feed`, `Media`, `Notifications`, `Onboarding` (provisioning lives here), `Search`, `Settings`. Shared/reusable view components live in `FaunaKit/Sources/FaunaKit/Views/` (flat, no `Components` subfolder), not here. See the tree — an enumerated folder table re-rots.

**Navigation:** the root `TabView` is **Conversations, Feed, Contacts, More** (`Fauna-iOS/App/ContentView.swift`), with the **More hub** (`MoreView`) carrying the remaining destinations. Sub-navigation uses `NavigationStack`. The canonical per-form-factor navigation model is owned by `docs/goal/ui/README.md` § Navigation; tab membership by ui.yaml `navigation.*`.

---

## Credential Storage

**This is the one apple credential-storage mechanics section** (macos.md points here; the cross-app contract is `common.md` § Credential storage). One shared FaunaKit `KeychainStore` serves both apple apps.

- **Default — device-bound (landed 2026-07-19):** every credential row is written `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, non-synchronizable — readable by background tasks after first unlock, and (being `ThisDeviceOnly`) excluded from iCloud Keychain sync *and* from encrypted-device-backup restore. The identity secret never silently migrates off the device. **On macOS the class is applied on the data-protection plane only (stated 2026-10-01):** the platform honours `kSecAttrAccessible` on a macOS item only when the query also sets `kSecUseDataProtectionKeychain` or `kSecAttrSynchronizable` true, and the legacy plane's device-bound write sets neither (`KeychainStore.planeAttributes`, *Keychain plane* below) — so on a legacy-plane build the row is an ordinary login-keychain item, never synchronizable (iCloud Keychain still never sees it) but carried by a home-folder backup or migration, sealed under the login keychain's password. What that means for the cross-app backup rule: `common.md` § Credential storage → *What a device's own backup carries*.
- **Opt-in iCloud backup (landed 2026-07-19; copy-not-move hardened 2026-07-19):** an apple-only Settings toggle "Back up identity to iCloud Keychain" (`settings-icloud-backup-toggle`, on the Account sub-page beside Identity Export) adds a `synchronizable` + `AfterFirstUnlock` **backup copy** of each row. iCloud sync requires `kSecAttrSynchronizable`, which is incompatible with the `ThisDeviceOnly` classes, so the backup copy is a distinct keychain primary key, not a flag flip.
  - **Copy-not-move (the invariant that makes "per-device choice" true at the keychain layer).** `KeychainStore.setICloudBackup(enabled:)` **keeps** each row's device-bound `ThisDeviceOnly` *working* copy and, when enabled, adds the synchronizable backup copy; opting out deletes **only** the synchronizable copy. A `synchronizable` item is one shared row per (service, account) across the whole Apple-ID iCloud-Keychain circle, and its **deletion propagates circle-wide** — so if opt-in *moved* the secret (deleted the device-bound source), an opted-in device's only copy would be that shared row, and one device toggling off (or any circle-wide delete) would destroy every other opted-in device's secret remotely (lockout, recoverable only via device-add / Identity Export). Keeping a `ThisDeviceOnly` working copy on every device means no device's working copy is ever the shared row, so deleting the circle (backup) copy can never take a working copy with it. A save made while backup is on lands both copies immediately.
  - **`load` prefers the device-bound copy** over the circle copy (falling back to the synchronizable copy only on a freshly-restored device that holds nothing else), so under a shared Apple ID with *different* fauna identities — where fixed-key legacy rows collide on one circle row and clobber each other — each device still reads its own device-bound truth, never a sibling identity's clobbered value. The per-actor account-registry rows are already identity-namespaced and never collide; only the legacy single-slot rows' *backup* copy is best-effort under that narrow config (a fresh-device iCloud restore could surface a clobbered backup value for those rows) — the working copy is always this device's own.
  - Every read/delete matches `kSecAttrSynchronizableAny`, so both copies stay findable and deletable (and a factory-reset namespace sweep wipes both). A launch-time `reconcileCredentialAccessibility()` converges each row's copy-set to the preference (device-bound-only when off; device-bound + synchronizable when on), re-lands a device-bound working copy on a restored device holding only the synced copy, and self-heals a row a crash mid-toggle left behind; it is idempotent. The policy applies to the whole service namespace, not just the secret row: the account-registry seam (`KeychainSecretStore`) is deliberately policy-free — a key→value map that must not learn which logical keys are identity material — so the store cannot single one row out. The device-local preference (`icloud_backup_enabled`) is itself always device-bound and never synced. Fauna's own device-add + box-recovery flows remain the first-class multi-device story; iCloud is a convenience redundancy.
- **Keychain plane (macOS) — one plane per build (BUILT 2026-08-25; its read-fallback removed 2026-09-25).** `KeychainStore` has two planes on macOS (`KeychainStore.Plane`): it **probes once per process** whether the running signature reaches the data-protection keychain (one `SecItemCopyMatching` addressed to the app-only access group `7457N3M72H.group.social.fauna.account` — `errSecItemNotFound`/`errSecSuccess` = reachable, anything else, `−34018` above all, = not; a read never prompts, so the probe is silent and logs `[keychain] write plane: …`), then **reads, writes and deletes that plane only**. The legacy `login.keychain-db` plane is the write plane of a build the probe refuses (an ad-hoc dev build, and today every Developer-ID build — below); a build on the data-protection plane never reads it. The read-legacy → write-data-protection copy-forward that shipped with the plane was removed by the compat-remnant sweep ([`../version-compatibility.md`](../version-compatibility.md) § Dimension 2, the fourth ratified exception — no row predating the plane exists), and with it the both-plane delete sweep it needed. iOS has one keychain and none of it runs there (both planes decorate a query identically). **Why an app-only group and not the File Provider's shared one:** rows in the shared group are readable by the sandboxed extension, which must never hold the identity seed (`on-demand-files.md` § Apple File Provider binding — `BackupKey` + bearer, never the seed); an app group is the one entitlement class ad-hoc builds tolerate and Developer ID honours without a profile (`installers/macos.md` § Identifier domain owns the id). Pinned headlessly by `KeychainPlaneTests` against the plane-modelling `FakeKeychainBackend` (the ad-hoc build lives on the legacy plane, the signed build writes DP only, a signed build never reads or copies a legacy row — the refusal pin — and the probe never writes). **Owed — the last inch: MEASURED 2026-08-28, and the pre-registered fallback IS the behaviour.** A supervised Developer-ID-signed install reads `write plane: legacy (probe status -34018)`, `secd` reporting the app's `com.apple.security.application-groups` *"ignored because of invalid application signature or incorrect provisioning profile"* — so the group is **not** an access group under Developer ID after all, exactly as this hedge pre-registered. It is not a one-line entitlement gap: reaching the DP plane under Developer ID needs an embedded provisioning profile (and `keychain-access-groups` outright kills any build without one). **RATIFIED 2026-08-28 — path A** (`installers/macos.md` § Identifier domain → *Path A*): this store (Swift `KeychainStore`, service `social.fauna.account`) moves to the DP plane once the app bundle embeds a Developer-ID provisioning profile authorizing its app-only access group — **no entitlement change**, a build-pipeline + portal change. The *shared Rust* credential slot (`fauna-account-store`, read by the agent + fauna-tui) stays legacy by necessity (tui is ad-hoc; the agent is a bare launchd binary), so the agent's one sticky prompt is accepted — owned by `sync-agent.md` § Packaging + lifecycle. History, kept because it bounds the design: `KeychainStore`'s queries used to set no `kSecUseDataProtectionKeychain`, so on macOS every row landed in `login.keychain-db` — the keychain whose per-item ACLs bind to a *code identity*, which is why an ad-hoc dev build re-prompted *"Fauna wants to access key …"* per item and "Always Allow" could not stick across rebuilds (measured 2026-07-20: one startup read burst → 5–6 prompts; evidence tracked internally). The sibling `FileProviderCredentialStore` already opted in to the data-protection keychain and called the flag load-bearing — an inconsistency inside one codebase, but **not a one-line fix**, for three reasons that bounded the adoption: (1) the data-protection keychain is flatly **unavailable to an ad-hoc-signed process** (`SecItemAdd` → `errSecMissingEntitlement` −34018 even carrying the app-group entitlement; probed 2026-07-20), so dev builds need a working legacy path regardless — the flip cannot be unconditional; (2) switching keychains **orphans every existing legacy row** — a stored identity seed going invisible is credential loss under the no-user-data-loss invariant, so adoption means a read-legacy → write-data-protection migration with copy-never-destroy semantics, never a flag flip; (3) a stable Developer ID signature collapses most of the observed UX damage on its own — **measured 2026-08-23 with the first signed builds** ([`../installers/macos.md`](../installers/macos.md) § Identifier domain's matrix session): the signed agent ACL-prompted **once** for its legacy item (the item's ACL was bound to the old ad-hoc identity), and after a single "Always Allow" it stayed silent across many agent restarts **and across a binary swap** (`/usr/local/bin` exec → the bundled copy — same Developer ID identity, so the ACL match held where ad-hoc cdhashes never did). So the residual fork is smaller than the 5–6-prompt ad-hoc experience suggested: an upgrading legacy-item user sees at most one prompt per item-identity transition, and a fresh signed install's own items never prompt their creator. A third, independent datum (2026-08-24) sharpens the case: the legacy plane's *writes* are also **launch-context-fragile** — a production agent instance bootstrapped into `gui/$UID` by a tool-driven `launchctl bootstrap` read credentials fine for a day (reads ride the search list) and then failed its first *write* with a GUI-modal "Keychain 'login' cannot be found … Reset To Default" dialog (writes need the session's default keychain, which that spawn context lacked) — a fragility class the data-protection keychain simply does not have (no session/default-keychain semantics). The data-protection plane was the prescribed fix and is what landed above (its copy-forward half since removed) ([`sync-agent.md`](sync-agent.md) § Implementation status today → A4 item 2 owns the TCC half it shared a root cause with — resolved the same day by moving the agent's state out of the container). iOS is untouched by the fork: iOS has only the data-protection keychain, so the flag is a macOS-plane question.
- **Keychain service — one current service, no retired spelling read.** `KeychainStore` addresses only `social.fauna.account` for every read, write, delete and converge. The retired-spelling read-forward (built 2026-08-27) and the parked-row merge it fed (`fauna-parked/<logical key>` + shared Rust's `AccountRegistry::adopt_parked_index`, and the launch's `FaunaAccounts.adoptParkedRows()`) were removed by the compat-remnant sweep (2026-09-25); which spellings are retired and why none is read are owned by [`../installers/macos.md`](../installers/macos.md) § Identifier domain. Pinned headlessly by `KeychainRetiredServiceTests` (a row under any retired spelling is never read, copied, parked or swept).
- **Shared Rust's slots ride the same store (landed 2026-08-26; contract owner `common.md` § Credential storage → *The shared Rust credential slots on the phones*).** iOS has no `fauna-credential-store` arm, so `FaunaAccounts.installPlatformCredentialStore()` — called from both shells' launch beside `NestTrust.installPinStore()` — lends the SAME `KeychainSecretStore` the registry rides to the crate's foreign arm, and the W3 (account-data-plane.md § Workstreams) account-store writer key lands as one more row of this service, `account` = `fauna-account-store/<actor hex>` (the T10 namespace as a prefix). Being a row of the service it inherits everything above with no code of its own: the `ThisDeviceOnly` default, the iCloud-backup copy-set when the toggle is on, the launch reconcile, and `deleteAll`'s whole-service sweep at factory reset. On macOS the call is inert — the Rust crate's login-Keychain arm is the slot the sync agent shares — which is why it lives in shared FaunaKit rather than an `#if os(iOS)`. **The store dir follows the row (2026-08-26):** because the key is `ThisDeviceOnly`, `FaunaClient.startAccountRuntime()` hands the account store its container **paired with** the same `CloudBackupExcluder` the custodian store uses (`FfiStoreContainer`; `isExcludedFromBackup` on the per-actor store dir, applied on every assembly), so a restored phone never holds a store whose writer key did not travel — ruled at the contract owner, mechanism owned by `behavior/backup-destinations.md` § Third destination kind → *Durability + labeling*. Pinned headlessly by `CloudBackupExcluderTests` (the real resource-value flip) and on the simulator by `IosInProcessDriver.account_store_cloud_backup_excluded` (the `com_apple_backup_excludeItem` xattr on the assembled store dir, asserted by `test_account_runtime_pump.py`).
- **E2E carve-out:** under `FaunaE2E.isActive` the store runs in-memory (owner: [`apple-e2e-automation.md`](apple-e2e-automation.md)); the preference round-trips there, but no real SecItem accessibility class is exercised (an unsigned test process cannot create a `synchronizable` item). The copy-not-move op-plan mechanism (which copies exist per policy, opt-out deletes only the circle copy, `load` prefers the device-bound copy) is pinned headlessly by `KeychainCopyNotMoveTests` against a `KeychainBackend` fake that models the `(service, account, synchronizable)` primary key; only the real cross-device iCloud sync/propagation is verified manually.

`FaunaClient` reads the secret once on init; token refresh signs via `FFICompat.build_auth_request(secretHex)` with the secret its caller holds (the re-read discipline, where needed, lives in the caller).

---

## E2E Testing

iOS is tested through the unified e2e framework, driven **in-process** — the app hosts its own automation HTTP server, exactly like macOS and Linux. No Appium, no XCUITest, no AutomationMode. **Authority:** [`apple-e2e-automation.md`](apple-e2e-automation.md) owns the apple-app e2e driving architecture; this section is a pointer. Dependencies: Xcode with an iOS simulator (`simctl`); the harness auto-builds (`just apple-ffi` + `xcodebuild`) and boots a per-session ephemeral simulator.

---

## Key Differences from macOS

| Aspect | iOS | macOS |
|--------|-----|-------|
| Navigation | Tab bar (Conversations/Feed/Contacts/More) + More hub | Sidebar (NavigationSplitView) |
| Background work | `BGProcessingTask` (§ BackgroundScheduler) | Resident engines in the separate always-on `fauna-sync-agent` process (survives app quit, 2026-07-19 A4 cutover — [`sync-agent.md`](sync-agent.md)); the in-app segment-backup upload driver was **deleted 2026-08-15** at the slice-5 flip — the nest-side segment-backup coordinator is now the sole writer, so macOS hosts no segment-backup upload at all (`sync-agent.md` § Scope per platform, `../../behavior/backup-restore.md` § Background Tasks → *Flip status (slice 5)*) |
| File sync trigger | None — iOS binds no folder in-process (remote files are read on demand through Media and the Files-app File Provider) | The shared engine's own watcher (FSEvents backend), now hosted by `fauna-sync-agent` rather than in-app |
| Photo backup | Shared engine + `BackgroundScheduler` | Shared engine, Folders surface |
| Push notifications | APNs via `PushManager` + NSE | Registration attempted through the same shared `PushManager` (`NSApplicationDelegate` token callbacks); the `aps-environment` entitlement and an AppKit analog of the NSE remain — owner `common.md` § Push Notifications |
| Menu bar | No | Yes |
| WebSocket lifetime | Paused in background | Persistent |
| Updates | App Store | notice-only check (you install the newer `.dmg`/`.pkg`) |

**No in-app "a newer version is out" notice, by design** — the App Store delivers updates and owns the notice, so the check-for-updates door the desktops have is absent here as a behaviour, not a mechanism (owner [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out; the catalog's `app-version-and-updates` outcome 2, user-approved absent 2026-09-26). The home-screen widget is the opposite case: iOS **owes** one, on WidgetKit — owner [`common.md`](common.md) § Home-screen widget.
