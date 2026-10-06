# Android App — target state

Owns: android
Status: ratified
Authority: android-app architecture — Kotlin/Compose/Hilt/Room app structure, UniFFI binding consumption, foreground services + WorkManager scheduling, widgets, adaptive layout, and the android-specific halves of shared subsystems; cross-app behavior → [`common.md`](common.md); onboarding flow → [`../../behavior/onboarding.md`](../../behavior/onboarding.md); photo-backup model + controls placement → `docs/goal/ui/folders.md` (`photo-backup` row); transport → [`../transport.md`](../transport.md); packaging, store identity + release signing → [`../installers/android.md`](../installers/android.md).

## Implementation status today

Broadly implemented, including the handle-first onboarding wizard (landed: `core/OnboardingHost.kt`, `ui/viewmodel/AppLaunchVM.kt`, `core/LaunchObserverImpl.kt`, `di/LaunchModule.kt` (registry-backed launch persistence over `core/LogicalSecretStore.kt` (every logical key verbatim; `SecretKeyMap.kt` deleted 2026-09-28 with the retired single slot, the identity read through the read-only `core/SessionAccount.kt`) — the multi-account launch-rewire, `LaunchPersistenceImpl.kt` deleted 2026-07-18; `../long-term-store.md` § Multi-account evolution), and the full wizard screen set under `ui/screen/onboarding/` — the adoption the per-app matrix in `behavior/onboarding.md` tracks). Declared gaps:

- **Transport:** the conversations plane rides the shared UniFFI `ConversationsSession` (`core/conversations/ConversationsManagerHost.kt` — the apple-twin pattern), and machine-backed surfaces consume the shared FFI clients; `ApiClient.kt` keeps OkHttp only for the byte routes and WS upgrades the migration leaves on HTTP (blobs, snapshot/sync file bytes; no control-plane `/api/v1/` call remains in main source — `common.md` § WS-RPC client binding owns the story).
- **Photo backup reframe (ruled 2026-07-10) — COMPLETE:** android adopts the Photo-Library-as-an-ordinary-folder model. The set-model half landed 2026-07-16 (alongside the byte-sync engine convergence below): the hardcoded `"photos"` set retired in favor of the shared `photo_library_set` resolver's wizard-created folder. **The UI-restructuring half landed 2026-07-19**: the bespoke `settings/photo-backup` screen (`PhotoBackupScreen.kt`) is retired — its controls render as a `PhotoBackupSection` on the Folders page, identical to iOS. Owner: `docs/goal/ui/folders.md`.
- **Byte-sync engine (resolved 2026-07-16):** android was the last non-conforming byte-sync deployment (`behavior/sync-engine-deployments.md` § Control Plane Principle) — a bespoke `ChunkedSyncEngine.kt` that uploaded chunks unsealed over raw `/api/v1/chunks`+`/manifests` HTTP routes. Now deleted: both `PhotoBackupEngine` (MediaStore) and `WatchedDirectoryManager` (SAF) route through the shared `FfiSyncEngineHost` via `ApiClient.syncEngineHost()`, library-ingress-shaped (stage to a temp file, `ingestFile`, delete). Owner: `docs/goal/behavior/sync-engine-deployments.md` § Implementation status today (android byte-sync).
- **WorkManager jobs — built, never witnessed running on a device:** the three periodic workers of § WorkManager Jobs (`WidgetDataWorker`, `PhotoBackupWorker`, `CustodianHostWorker`) are all `@HiltWorker`s, constructed only through the `HiltWorkerFactory` that `FaunaApp.workManagerConfiguration` installs with WorkManager's eager `androidx.startup` initializer removed in the manifest (the on-demand recipe; landed 2026-07-22, before which none of them could construct). Headlessly pinned by `WorkerFactoryWiringTest` (every enqueued worker is a `@HiltWorker`, none is default-factory-constructible) — **no run has shown any of them construct and `doWork()` on a device, nor that the app's configuration is the one WorkManager uses**. That witness waits on the android e2e venue (`../testing.md` § Android's run venue: no android run has been recorded).
- **Peer-transfer plane — a declared absence (ruled 2026-09-25, advisory; re-examined against iOS 2026-09-26 and RE-PUT to the user — the recommendation is to re-classify it as owed and gated, like iOS's leg, behind an android on-demand replica over the SAF `DocumentsProvider`):** android runs no leg of the cross-user share plane today; its ingress-only engine host keeps no local replica for the plane to serve from or ingest into. Owner: `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer → Implementation status today, the android ruling and its 2026-09-26 re-examination.

## Goal

A Kotlin/Compose Android app that consumes the same shared Rust core (`fauna-ffi`) as iOS and macOS via UniFFI, follows the cross-app behavior in `common.md`, and presents Android-native idioms (Material3, Hilt DI, Room, WorkManager, foreground services) for the platform-specific surfaces.

---

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Language | Kotlin 2.0 |
| UI | Jetpack Compose (Material3) |
| DI | Dagger Hilt |
| Architecture | MVVM |
| Local DB | Room 2.6.1 |
| Networking | OkHttp3 4.12.0 (legacy clusters) + the shared UniFFI WS-RPC plane |
| Serialization | Kotlinx-Serialization |
| Images | Coil 2.6.0 |
| Widgets | Glance 1.1.0 |
| Background | WorkManager |
| Secure storage | EncryptedSharedPreferences + DataStore |
| On-device ML | none — the ONNX Runtime 1.18.0 dependency is retired (2026-10-02, § On-Device Spam Classification) |
| Min SDK | 26 (Android 8.0) |
| Target SDK | 35 |

---

## App Structure

```
com.fauna.app/
├── core/           ApiClient, OnboardingHost, CredentialStore, SecureStorage,
│                   MlsManager, WatchedDirectoryManager,
│                   PhotoBackupEngine, ExifStripper,
│                   LaunchObserverImpl, LogicalSecretStore, SessionAccount, ResolveService,
│                   NetworkMonitor, FfiCryptoOps,
│                   conversations/ (ConversationsManagerHost), feed/
├── data/api/       ApiTypes.kt
├── data/db/        Room entities + DAOs
├── service/        SyncService (foreground service);
│                   PhotoBackupWorker (photo-library backup — periodic, constant 15-min floor);
│                   CustodianHostWorker, CustodianPushKick (client-device backup
│                   custodian pull — periodic + foreground push-debounce)
├── provider/       Content providers
├── testing/        Test support
├── ui/navigation/  Compose NavHost, drawer navigation
├── ui/screen/      Screen composables (incl. onboarding/ wizard screens)
├── ui/viewmodel/   Hilt-injected ViewModels (incl. AppLaunchVM)
├── di/             Dagger modules
└── widget/         Glance widgets + WorkManager periodic jobs
```

---

## UniFFI Rust Bindings

The Android app uses UniFFI to call into the same `fauna-ffi` Rust crate used by iOS/macOS/windows.

| Aspect | Detail |
|--------|--------|
| Generated binding | `app/src/<buildType>/java/uniffi/` — **gitignored**, regenerated per flavor (below) |
| Native library | `libfauna_ffi.so` in `app/src/<buildType>/jniLibs/` |
| ABI targets | `arm64-v8a`, `armeabi-v7a`, `x86_64`, `x86` |
| JNI mechanism | JNA library interface |

The binding is **not checked in**: after rebasing onto a `libs/fauna-ffi` change, run **`just android-ffi-test`** (dev/e2e) or **`just android-ffi`** (production) — each runs `cargo run -p fauna-ffi --bin uniffi-bindgen generate --language kotlin` + builds/copies the `.so`s — or the app won't compile. (`wasm-pack` is the web toolchain and has no android role.)

**Four flavors, staged per buildType** (`e2e-automation-surface-gating.md` § Cross-app e2e conventions point 15 — the automation surface is compiled out of release artifacts). The third, `storeSafe`, is android's leg of the App-Store escape hatch (payments excision) — the split's rationale, the shared `store-safe` cargo-feature convention, and the sibling apps' equivalents are owned by [`../dynamic-features.md`](../dynamic-features.md) § Platform-family surface excision, not restated here. The fourth, `foss` (landed 2026-08-24), is the F-Droid / direct-download artifact — its rationale (excising the two closed-source Play IPC shims behind the store-age arm) is owned by [`../installers/android.md`](../installers/android.md) § Release channels; it stages the same production `fauna-ffi` build as `android-release` (no extra cargo features) since the exclusion is a Play-client-library concern, not an FFI one:

| Recipe | Features | Stages into | Consumed by |
|---|---|---|---|
| `just android-ffi` | *(none — production defaults)* | `app/src/release/` | `android-release` — the shipped APK |
| `just android-ffi-test` | `test-helpers` | `app/src/debug/` | `android-debug` (e2e), `android-host-test` |
| `just android-ffi-store-safe` | `--no-default-features --features store-safe` | `app/src/storeSafe/` | `android-store-safe` — the payments-excised APK (`storeSafe` buildType, `initWith(release)`) |
| `just android-ffi-foss` | *(none — same as `android-ffi`)* | `app/src/foss/` | `android-foss` — the F-Droid / direct-download APK (`foss` buildType, `initWith(release)`, no `com.google.android.play` class; witness `just android-foss-check`) |

(The android-only `tunnel` feature was dropped 2026-08-25 along with the dead `P2PTunnelService` — see § Background Services.)

Gradle picks the source set by buildType, so `assembleRelease` structurally cannot see the `test-helpers` seams, `assembleStoreSafe` structurally cannot see any payments FFI symbol, and `assembleDebug` always can — correctness does not depend on which recipe ran last. The production bindgen asserts zero `*ForTest` exports in the tree it just generated, so a newly added ungated seam fails the build rather than shipping. `just android-store-safe-check` is the two-column witness (unpacks the APK and greps `classes*.dex` + `lib/*/*.so`, since a `strings` scan over the compressed archive finds nothing).

Kotlin has no inline compile-time exclusion, so the `payments`-plane glue (`FfiPaymentsClient`/`FfiProviderItem`/`FfiClaimItem`/`paymentsKnownKinds`/`paymentsWebhookUrl`/`FfiFeedManager.resolvePostTips`) lives in per-build-type twins — `app/src/payments/` (debug+release+foss) and `app/src/noPayments/` (storeSafe) — rather than a runtime branch, since a store-safe build's generated bindings have nothing for such a branch's body to typecheck against; the render side (§4/§5 provider/claim UI, the claim-redeem input, the feed tip surface) is gated by the `BuildConfig.PAYMENTS` constant, folded by R8. The same twin shape now also covers the store-age arm's two Play IPC shims (Play Integrity + Play Age Signals — behavior owner: [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band): `app/src/storeAge/` (real, on the three Play-distributed build types) and `app/src/noStoreAge/` (inert, `foss` only).

Three hand-written source sets follow from the buildType split:

| Path | Holds |
|---|---|
| `app/src/debug/java/…/testing/TestAgent.kt` | the real e2e agent — the only hand-written code that names a `*ForTest` seam |
| `app/src/noAgent/java/…/testing/TestAgent.kt` | an inert same-signature twin taken by all three shipping flavors (`release`, `storeSafe`, `foss`), so the handful of `src/main` files that mention the agent still type-check there |
| `app/src/testDebug/java/…/testing/` | the agent's own Robolectric tests — `testDebug`, not `test`, because `testReleaseUnitTest` compiles against the twin |

`src/main` must never name a seam directly: `ConversationsManagerHost` reaches its one install through `TestAgent.installMockBackendsIfE2E()`, real in debug and a no-op in release. This is the same gated-real-plus-no-op-twin shape linux and tui use for `start_test_agent_if_enabled` / `start_if_enabled`; adding a `src/main` call into the debug agent without mirroring it on the twin breaks the release build, which is the intended feedback.

Capabilities exposed: crypto (Ed25519 keypair/signing), the conversations session (MLS engine + receive loop), file-sync chunking, email signing, launch/onboarding machines, and the typed feature clients.

---

## Room Database

Room is the single source of truth for local state; migrations are versioned alongside the entities. DAOs cover conversations, groups, synced files, sync anchors, watched directories, a cached-account profile row (for offline display), photo backup, contacts, knocks, muted conversations, and MLS channel state. All DB access goes through DAOs — no raw SQL in ViewModels or UI. **Snapshots are not a Room table:** the `snapshots` table was dropped — the Backups page's snapshot list is the shared `BackupsMachine`'s live state now (`ui/backups.md` § Snapshot-list shape), re-read from the nest on every page load rather than cached locally.

---

## Background Services

### SyncService (foreground service)

The manual "Back up now" trigger for photo backup only (`FoldersScreen.kt`'s photo-backup controls start it) — a one-shot `START_NOT_STICKY` foreground service wrapping `PhotoBackupEngine.syncNewPhotos()` (itself routed through the shared `FfiSyncEngineHost`, § Photo Backup Engine below) so Android doesn't kill the upload mid-transfer. Persistent notification while active. `WatchedDirectoryManager`'s directory-watch sync runs off `WatchedDirectoryVM` directly, not through this service.

Android hosts no P2P peer node today — the dead, never-started `P2PTunnelService` was deleted (2026-08-25), along with the android-only `tunnel` cargo feature it depended on. Per-app P2P status is owned by `docs/goal/behavior/p2p.md`.

### WorkManager Jobs

| Job | Trigger |
|-----|---------|
| App widget refresh (`widget/WidgetDataWorker.kt`) | Periodic (15-minute Android minimum) |
| MLS key-package replenishment | Session-owned (the shared `ConversationsSession` tops up — owner: `behavior/direct-messages.md`) |
| ~~Mail-segment backup~~ | **RETIRED 2026-08-15 at the slice-5 flip** — android drives no segment-backup *upload*; the source nest is the writer. The start-up cancel of the retired worker's unique work left with the compat-remnant sweep (2026-10-03: no installation that enqueued it exists) — owner: `behavior/backup-restore.md` § Background Tasks |
| Photo-library backup (`service/PhotoBackupWorker.kt`) | Periodic on WorkManager's 15-minute floor — the platform's clamp of the hard-coded reconcile constant (phase 5's de-knob; it used to re-periodize itself from the set's `rescan_interval_secs`, which the floor swallowed anyway); `NetworkType.UNMETERED`-constrained — owner: `ui/folders.md` § Photo backup / `behavior/file-sync.md` § Config |
| Client-device backup custodian pull (`service/CustodianHostWorker.kt`) | Periodic, 15-minute Android minimum, `NetworkType.CONNECTED`, construct-`runAllKinds()`-drop per pass; the foreground-session sibling `service/CustodianPushKick.kt` (`ProcessLifecycleOwner`-gated) holds one host per foreground session and wakes it on the destination's push subscriptions within the shared debounce window instead of waiting for the next period; both build through `ApiClient.buildCustodianHost`, so neither trigger can state its own cloud-backup exclusion. Since the row above retired, this is the only backup direction android drives — owner: `behavior/backup-destinations.md` § Third destination kind |

**Photo backup gained a periodic WorkManager job (landed 2026-07-22).** Before this, photo backup ran **only** on the manual "Back up now" tap (`SyncService`, above), despite `photo-backup-enable-toggle`'s automatic-sounding label — `PhotoBackupWorker` is the periodic sibling that closes that gap: enqueued at app start on WorkManager's 15-minute floor (it used to re-periodize itself after every pass from the set's nest row through `FfiFoldersClient::rescan_interval_secs_for`; phase 5 of the folders re-model retired that face and the per-folder choice — the floor had swallowed every pickable value anyway). `NetworkMonitor.kt`'s `shouldSyncPhotos()` unmetered gate still runs *inside* the pass (mid-transfer Wi-Fi drop), a second layer under the scheduling-time `NetworkType.UNMETERED` constraint. Android now matches apple's `BackgroundScheduler.swift` `BGProcessingTaskRequest` shape. Detail + per-platform trigger table: `docs/goal/ui/folders.md` § Photo backup.

---

## Photo Backup Engine

`PhotoBackupEngine` (`core/`) scans the device media store for new photos/videos, runs the shared metadata strip (`core/ExifStripper.kt`, which delegates to `stripMediaMetadata`; per-container coverage — incl. which containers are still declared residuals — is owned by `../../behavior/sync-engine-deployments.md` § Ingress metadata-strip convergence) before upload for privacy, resolves its target folder via the shared `photo_library_set` resolver, and uploads via the shared **`FfiSyncEngineHost`**'s sealed per-file ingest (stage the stripped bytes to a temp file, `ingestFile`, delete the temp; mechanism owner: `behavior/file-sync.md`). Invoked both by the manual "Back up now" button (`SyncService`, § Background Services) and, since 2026-07-22, automatically by the periodic `service/PhotoBackupWorker.kt` (§ WorkManager Jobs).

**Target (ruled 2026-07-10) — COMPLETE:** the engine feeds a wizard-created **"Photo Library" folder** with controls on the Folders page — identical UX to iOS (`docs/goal/ui/folders.md` owns the model + placement). The set-model half landed 2026-07-16; the UI-restructuring half (retiring the bespoke `settings/photo-backup` screen) landed 2026-07-19.

---

## Secret Storage

| Data | Storage mechanism |
|------|------------------|
| Ed25519 secret key | `EncryptedSharedPreferences` (AES-256-GCM, master key in Android Keystore — `core/SecureStorage.kt`) |
| Bearer token | In-process memory only (`ApiClient.token`) — minted fresh over `fauna.auth.handshake`, never written to `EncryptedSharedPreferences` or any disk store; cleared in `clearAuth()` |
| Shared Rust's credential slots — the W3 (account-data-plane.md § Workstreams) account-store writer key (T10) first | The SAME `EncryptedSharedPreferences`, reached by Rust over the `FfiSecretStore` seam: `LaunchModule.provideSecretStore` lends `LogicalSecretStore` to `installPlatformCredentialStore` (android has no `fauna-credential-store` arm), and Rust writes namespace-prefixed rows (`fauna-account-store/<actor hex>`). Landed 2026-08-26, **compile- and Robolectric-unverified from macOS** — the emulator run is owed on the emulator host. Contract owner: `common.md` § Credential storage → *The shared Rust credential slots on the phones* |
| Install device secret (the sync device id's derivation input) | A SECOND `EncryptedSharedPreferences` file, `fauna_install_prefs` (`core/InstallSecretStore.kt`), under the same master key — sign-out resets `fauna_secure_prefs` wholesale, and the secret must survive it. It names no account; the per-account id it derives is cached in the account's own slot in `fauna_secure_prefs` and erased with it. Rule: [`sync-agent-credentials.md`](sync-agent-credentials.md) § Credential model, the 2026-09-20 ruling |
| UI / sync preferences | Jetpack DataStore |

The secret never leaves `EncryptedSharedPreferences`; UniFFI calls receive key material in-process. Cross-app contract: `common.md` § Credential storage. `allowBackup="false"` keeps the whole app-private tree out of **cloud backup**, so there the account store dir and the writer key beside it are restored together or not at all. **It does not keep the tree out of a device-to-device transfer (corrected 2026-10-01):** the app targets API 35, and for apps targeting API 31 or higher the platform documents that on some manufacturers' devices the attribute disables cloud backup but not the transfer; the manifest declares no `android:dataExtractionRules`. What such a transfer carries is the two `EncryptedSharedPreferences` files — unreadable on the new device, because their master key is in the Android Keystore and never leaves the old one — and the rest of the tree, the account store dir included, without a readable writer key (the shape `common.md` § Credential storage says self-heals). The rule this falls short of, and the manifest change owed (exclude every domain from both `<cloud-backup>` and `<device-transfer>`): `common.md` § Credential storage → *What a device's own backup carries*. Since 2026-08-26 the account runtime *states* that: `ApiClient.startAccountRuntime` hands the store its container paired with `CloudBackupPosture.exclusion()` (the manifest arm the custodian host already states), so the posture is auditable at the call site rather than assumed (`common.md` § Credential storage → *the store dir follows the row*).

---

## Navigation

`ui/navigation/` hosts a single Compose `NavHost` with drawer navigation (`ModalNavigationDrawer`). Deep links route through the same `NavHost`. Screen composables are stateless (`StateFlow` in, events out). The cross-page nav model is owned by `docs/goal/ui/README.md` § Navigation.

---

## The page error surface

Scope note only — the cross-app contract and the three shapes that satisfy it are owned by [`../e2e-conventions.md`](../e2e-conventions.md) convention 2 and its rider. Android implements **shape (ii)**, the same one web ratified: `AppMessages` is the app-wide funnel, and `TestAgent.messagesJson` serializes the state protocol's `messages` as **`null` when that funnel has nothing to say**, so the shared `error_text()`/`has_error()` fall back to reading the page's own `error-message` element.

Two consequences a screen author should know:

- **A page-scoped `StateFlow` error is a legitimate error surface here** — `AdminDnsVM.error`, `AdminNestVM.error`, `MutedWordsVM.errorMessage` render straight into the page's `error-message` element and never publish to `AppMessages`. That reads correctly *because* of the null gate; before it (fixed 2026-08-11) `messages` was a present-but-all-null object, which resolves to `""` in the shared helper and made every one of those errors invisible to the harness while looking perfectly fine to a human.
- **Never render a placeholder under the `error-message` tag.** The id must leave the semantics tree when there is no error — the bridge resolves visibility as bare existence (`ElementOps.isVisible` = `findAll(id).isNotEmpty()`), so an empty `Box` makes `is_visible("error-message")` structurally true and the negative assertion every such test opens with can never fail. `AdminNestScreen` and `AdminDnsScreen` each shipped one until 2026-08-11; both are pinned now by an `errorMessageIsAbsentUntilThereIsAnError` test.

---

## On-Device Spam Classification

**RETIRED (ruled 2026-10-02 — `content-scoring.md` § The placement matrix → *Deployment-wide content models at the client position*).** `OnnxSpamScorer` (`core/`, direct `ai.onnxruntime` Kotlin calls, the model downloaded from `GET /api/v1/moderation/model`) scored nothing a user ever saw: its only caller was `ModerationManager.classify()`, whose only consumer was the Settings diagnostic's classify-this-text box (a `spam_ml` label above a hard-coded `0.1f`, coloured at `0.4f`/`0.7f`), and the label was never attached to content; nothing on the feed or conversations paths called it (`classifyAndReport` was removed 2026-07-19). The user's spam preference (`fauna.spam.{get,set}_preferences`; owner `ui/settings.md` § Spam) is applied on android as on every app by the shared `content_render_verdict` over the labels that arrive *with* content (`ContentPolicyStore` → `verdictFor`), which the shared text heuristic and the per-user Bayesian model feed. The scorer, the `onnxruntime-android` dependency and the manager's ONNX arm are removed; the diagnostic itself is ruled in the next paragraph.

**The Settings → Content Moderation diagnostic is RETIRED as drift (ruled 2026-10-02; the user approved the deletion and it landed 2026-10-03).** The `settings/moderation` sub-page (`ModerationScreen.kt`: a classifier-status card, a classify-this-text card whose per-label percentages are coloured at hard-coded `0.4f`/`0.7f` Kotlin cut-offs, and an About card; `ModerationVM`; `ModerationManager.classify()` — the shared text heuristic plus the per-user Bayesian above a hard-coded `0.1f` — fed by `ApiClient.fetchUnwrappedSpamModel`) is an android-only surface no spec names: `pages.moderation` is queue-only on every app, `pages.settings` has no moderation row, no other app has a classifier probe, no e2e drives it, and [`../../behavior/moderation.md`](../../behavior/moderation.md) § User actions lists only the per-row training correction — per-app drift (priorities #1/#4), not a declared platform absence. It is deleted, not spec'd for all 7 apps, because it is a developer tool rather than a user action: the queue already shows every real detection with its confidence (`moderation.md` § Per-row badge data path), the user's filtering runs through the shared `content_render_verdict` over the labels that arrive *with* content, and a box that scores arbitrary pasted text verifies nothing the user can act on — and a probe no app has would otherwise cost six lifts plus a shared-Rust band lift to carry thresholds nobody tunes. Deleted with it, as sole consumers: the UniFFI `fetch_unwrapped_spam_model` façade (`libs/fauna-ffi/src/spam_scorer.rs` — the native mail scorer fetches its model in-Rust, `mail-spam.md` § Implementation status item 6, and never used the façade) and the `settings.moderation_page.*` strings other than `title` (the page heading all apps share). The Moderation queue (`moderation-tab`, `ModerationQueueScreen`) and its drawer row are untouched; the sub-page's Settings row and route go with it. The deletion has landed: `ModerationScreen`, `ModerationVM`, `ModerationManager`, `ApiClient.fetchUnwrappedSpamModel`, the façade and the strings are gone, and android scores nothing on-device outside the shared mail scorer.

---

## Adaptive Layout

`WindowSizeClass` adapts phone (compact: single-pane + drawer), foldable (medium), and tablet (expanded: two-pane + rail) layouts.

---

## App Widgets

`widget/` contains Glance-based home-screen widgets. The periodic `WidgetDataWorker` (`widget/WidgetDataWorker.kt`) fetches the unread count live over the shared WS-RPC plane (`ApiClient.fetchInbox()` → `inboxRpc().fetch`) — not a Room query — and writes it into Glance's own per-widget DataStore state (`updateAppWidgetState`); `FaunaWidget` (`widget/FaunaWidget.kt`) renders from that cached state. The cross-app promise this mechanism fulfils — and which apps owe or lack a widget — is owned by [`common.md`](common.md) § Home-screen widget; android was the first app to have it (linux and the two apple apps followed 2026-09-26), and its witness (a Glance render from seeded state, or the widget host read through `adb`) is still unwritten. **Gap (found 2026-09-26 by linux's design pass): the number the worker writes is `fetchInbox().size` — the size of one page of the sealed mail `INBOX`, not the unread count the promise names.** The promised number is the shared `fauna_conversations` fold (`common.md` § Home-screen widget, the reworded mechanism sentence; linux sums it in `state.rs::sum_unread`), and android's worker must read that fold — over the same UniFFI `ConversationsManager` the app renders from, or a shared getter lifted for the purpose — rather than a mailbox page count.

---

## Key Differences from iOS

| Aspect | Android | iOS |
|--------|---------|-----|
| Language / UI | Kotlin 2.0 + Compose | Swift + SwiftUI |
| DI | Dagger Hilt | Manual injection |
| Local DB | Room 2.6.1 | SwiftData |
| FFI mechanism | JNA + gitignored `uniffi/` bindings (`just android-ffi`) | XCFramework (`just apple-ffi`) |
| Background work | WorkManager + foreground Services | BackgroundTasks / BGScheduler |
| Directory watching | `core/WatchedDirectoryManager.kt` (comparison owner: `behavior/file-sync.md`) | No (pull-based) |
| Widgets | Glance | N/A |
| Secret storage | EncryptedSharedPreferences + Keystore | Keychain ([`ios.md`](ios.md) § Credential Storage) |

**No in-app "a newer version is out" notice, by design** — the Play store delivers updates and owns the notice, so the check-for-updates door the desktops have is absent here as a behaviour, not a mechanism (owner [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out; the catalog's `app-version-and-updates` outcome 2, user-approved absent 2026-09-26).
