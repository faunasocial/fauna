# macOS App — target state

Owns: macos
Status: ratified
Authority: macos-app architecture — the desktop shell (windows, menu bar, quick switcher, the newer-version check), the macOS half of the sync deployment (the one-shot-only engine host's lifecycle, the sync-agent provisioner, and the location-binding UI — resident engines moved to the per-user `fauna-sync-agent`, owner `sync-agent.md` § Implementation status today milestone A4), macOS entitlements/packaging posture, and the macOS-specific halves of shared FaunaKit subsystems; cross-app behavior → [`common.md`](common.md); the FaunaKit shared-layer inventory → [`ios.md`](ios.md) § FaunaKit (the single inventory home — this doc points); credential storage → `common.md` § Credential storage (contract) + [`ios.md`](ios.md) § Credential Storage (the one apple mechanics section); e2e driving → [`apple-e2e-automation.md`](apple-e2e-automation.md); sync control plane rules → [`../app-guidelines.md`](../app-guidelines.md) rule 9; transport → [`../transport.md`](../transport.md); packaging artifacts → [`../installers/macos.md`](../installers/macos.md).

## Implementation status today

Broadly implemented. One declared gap remains open (Sync); the other (legacy event stream) is now closed:

- **Home-screen widget — DECIDED 2026-09-26: app residency via auto-start at sign-in.** The widget renders the snapshot the app publishes from its conversations list ([`ios.md`](ios.md) § Home-screen widget), so the count is current exactly as long as the app is resident: across a window close (a close is never a quit), and from every login because auto-start registers by default and the login launch is hidden-resident. The mechanism, the reasoning against the sync agent publishing, and the residual an explicit Quit leaves are owned by § App Lifecycle → *Auto-start at sign-in*.

- **Declared absences (owner [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band):** no store age signal and therefore no `invite-request-age-notice` (D3 — at most the two store-distributed mobile apps ever receive one; the id is `platform_elements: {android, ios}` in ui.yaml), and no kids flavor (the kids-app bullet: the kids door is a store construct, android and ios only). The e2e gate for both is `declared_absence`, never `skip_unbuilt`.

- **Sync subsystem: CONVERGED 2026-07-13, then cut over to the per-user agent 2026-07-19** (`../../behavior/sync-engine-deployments.md` § Apple apps — convergence design; B1 + B2; agent cutover: `sync-agent.md` § Implementation status today, milestone A4). The bespoke Swift `SyncEngine` + FSEvents `DirectoryWatcher` are **deleted**, replaced by the shared `fauna-sync-engine`; **`SyncDaemonManager` retired outright** — its `launchctl` shell-outs were exactly the control-plane violation this gap declared (`app-guidelines.md` rule 9: manage sync by calling the nest, never by controlling a daemon process), and the LaunchAgent path it managed was inert in practice (the SwiftPM build never bundled the `fauna-sync` binary). **As of the A4 cutover (2026-07-19), the app no longer hosts resident location engines in-process** — those moved to the external per-user `fauna-sync-agent`, provisioned by the app via `FfiSyncAgentProvisioner`; the in-app `FfiSyncEngineHost` is now one-shot-only (photo ingress, restore, per-file state reads). The standalone `bins/fauna-sync` daemon remains available for headless Macs — self-installed, not app-managed. Details: § Sync below. **The hardcoded `"photos"` set name also retired 2026-07-13 (B3)** — photo backup now resolves its target set through the shared `fauna_folders_machine::photo_library` adopt-or-create resolver (`../../ui/folders.md` § Photo backup owns the model). One apple-wide residual remains: the re-seal migration for records the old engine wrote, tracked in `sync-engine-deployments.md` § Apple apps — convergence design.
- **Legacy event stream: RETIRED 2026-07-17** ("apple push pump — consume `fauna.notification`; delete the dead second socket"). FaunaKit rides the shared Rust WS-RPC client exclusively via `FfiNestClient` (reconnect supervisor + subscriptions in `FaunaKit/Core/FaunaClient.swift`; the machine-backed VMs consume it — see `common.md` § WS-RPC client binding, the owner of that story). The URLSession-backed `WebSocketClient` (the second, legacy event-stream socket) is deleted outright — there is no remaining apple-side transport gap of this kind.
- **Store-safe flavor (payments excision): apple joined 2026-08-15**, the first non-Rust shell — mechanism, recipes (`just apple-ffi-store-safe`, `just mac-store-safe`), and the two-column witness (`just apple-store-safe-check`) are owned by [`../dynamic-features.md`](../dynamic-features.md) § Platform-family surface excision, not restated here.

## Goal

A SwiftUI macOS desktop app that consumes the shared Rust core through `FaunaFFI.xcframework` (UniFFI), shares FaunaKit with iOS, and adds desktop-class subsystems — multi-window layout, persistent menu bar, always-on sync via FSEvents-driven directory watching, a notice-only newer-version check — while delegating sync control to the nest API (WS-RPC kinds) per the cross-app guidelines.

---

## Tech Stack

| Component | Detail |
|-----------|--------|
| Language | Swift 6.0 |
| UI framework | SwiftUI |
| Minimum deployment | macOS 15.0 |
| Shared library | FaunaKit (same `Package.swift` as iOS; module inventory → [`ios.md`](ios.md) § FaunaKit) |
| Native bindings | `FaunaFFI.xcframework` via UniFFI, built by `just apple-ffi` (not arch-limited; the slice set is the framework's shape, owned by [`../build-target-layout-macos.md`](../build-target-layout-macos.md) § *apple-ffi slice set*) |
| Update check | Notice-only, over the shared `fauna-ffi` face of `fauna_client::update_look` (`checkForNewerRelease`, `lookAtSignIn`, `releaseFeedOrigin`) — `Fauna-macOS/App/UpdateCheck.swift`, one instance on `MacAppState`. The door is Settings → General's About block (`settings-app-version`, `settings-check-updates-button`, `update-available-notice`, linux's shape), with the app menu's *Check for Updates…* as a second door onto the same check; the once-per-sign-in look runs from both session-building paths (`completeAuthenticatedLaunch`, the e2e session patch). Under e2e, a DEBUG build reads the stub feed from `FAUNA_E2E_RELEASE_FEED_URL` (`E2eEnv.releaseFeedUrl`, convention 15). No download, no in-place replacement, no toggle. The promise and its desktop-only scope → [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out |

## What Is Shared with iOS

The macOS app pulls in FaunaKit as a local Swift package — the same package used by the iOS target (and the `Fauna-watchOS` / `Fauna-watchOS-Widgets` targets; the legacy `Fauna-FinderSync` extension is deleted, superseded by the shared File Provider extension — `../../behavior/on-demand-files.md` § On-Demand Files → Apple File Provider binding). Nothing is forked or duplicated. **The FaunaKit module inventory (Core / Models / ViewModels / Views / Utilities / Testing / Generated) lives once, in [`ios.md`](ios.md) § FaunaKit** — this doc describes only the macOS-specific subsystems below.

---

## App Entry

`FaunaMacApp.swift` is the top-level entry point.

- Uses `@NSApplicationDelegateAdaptor(AppDelegate.self)` for native macOS lifecycle integration.
- Initializes on launch: the in-process **one-shot-only sync engine host** (`FaunaClient.syncHost`, built in `start()` once WS-RPC is up — photo ingress, restore walks, and per-file state reads; no resident engines since the A4 agent cutover), the **sync-agent provisioner** (`FfiSyncAgentProvisioner`, started post-auth via `LaunchdSyncAgentSpawner` and handed to `LocationsModel` — owner `sync-agent.md` § Control plane split, milestone A4), `PhotoBackupEngine` (shared FaunaKit; configured via `PhotoBackupControlsView` on the Folders surface — owner `docs/goal/ui/folders.md`; uploads through the host's sealed ingest). (The standalone `MlsManager` process-global MLS engine that used to be initialized here was retired 2026-07-19 as dead code — see § MLS Integration below.)

### Windows

The app manages three separate windows rather than a single full-screen view:

| Window | Purpose |
|--------|---------|
| Main (`minWidth: 800, minHeight: 500`; `defaultSize` 1100×700) | Primary content: feed, conversations, contacts, calendar |
| Settings | Preferences panel (replaces iOS bottom sheet) |
| Onboarding | First-run registration and nest connection |

### In-app routes (`fauna://`)

The `fauna` URL scheme is registered in the app's `Info.plist`; a route arrives as `onOpenURL` on the main window and takes the one FaunaKit door shared with iOS, held until the session is authenticated. The mechanism — including the consent route's handoff to Settings → Connected apps and the `open_route` e2e seam — is owned by [`ios.md`](ios.md) § App Entry → *In-app routes*.

### Menu bar + shortcuts

`MenuBarController` (`Fauna-macOS/MenuBar/`) provides the persistent menu-bar presence. Keyboard shortcuts are SwiftUI **menu commands** on the app scene, not MenuBarController registrations: Cmd+N (compose) and Cmd+K (quick switcher) in `FaunaMacApp.swift`, plus Cmd+1..5 tab shortcuts in `Commands/KeyboardShortcuts.swift`.

---

## App Lifecycle

### Window close

**Closing the main window is never a quit** — the app stays resident (menu-bar presence, the Dock icon per the dock-icon preference) with its WS-RPC subscription and the conversations observer that feeds the home-screen widget ([`ios.md`](ios.md) § Home-screen widget) alive; the sync agent additionally outlives an explicit Quit (§ Sync). The promise this serves — closing a window is never a silent stop — is owned by [`common.md`](common.md) § Desktop Residency; the witnesses are `test_sync_agent_survives_macos_window_close.py` and the widget's `test_the_count_keeps_itself_current_without_opening_the_app` (`macos` leg). There is no close-to-tray setting on macOS: with nothing to close *to*, that half of the settings feature is absent here by construction (`close-to-tray-toggle` stays the linux+windows `platform_element`).

### Auto-start at sign-in

**Target state (ratified 2026-09-26) — the macOS leg of the cross-app auto-start feature.** The rationale and the cross-app intent are owned by [`windows.md`](windows.md) § App Lifecycle → *Auto-start at sign-in*, the linux leg by [`linux.md`](linux.md) § Auto-start at sign-in; this section keeps macOS's mechanism. Fauna registers itself to start at macOS login **by default**, at the universal post-auth hook (`completeAuthenticatedLaunch` in `FaunaMacApp.swift`, the production authenticated entry point every login and returning-user relaunch funnels through), so after the *first* successful sign-in the app is present at every later login with no manual step. The same three properties as the two sibling legs are load-bearing:

- **The choice is tri-state, never a registration-existence check.** `fauna.launchAtLogin` in `UserDefaults` (`MacAppState.launchAtLogin`: unset ⇒ register by default; explicitly `false` ⇒ never re-register; explicitly `true` ⇒ register), rendered by the shared `settings-autostart-toggle` on Settings → General (`GeneralSettingsView`). An explicit opt-out is never overridden — the app is the only configuration surface ([`../../principles.md`](../../principles.md)) — and a System Settings → General → Login Items opt-out is the user's choice too: the toggle reflects it and never silently re-registers over it.
- **Registration is gated off under e2e** (`FaunaE2E.isActive`), so harness runs never write the shared dev machine's real login items; the toggle's witness is the persisted choice surviving a relaunch, the mechanism's the unit-tested composition (linux's split, [`linux.md`](linux.md) § Auto-start at sign-in → *Not covered by an e2e test, by construction*).
- **The auto-start launch is hidden** — resident with no main window (menu-bar presence and the Dock icon as on any launch), decided by the `--autostart` argument the registered job carries, the flag linux and windows carry; a launch that routes to *onboarding* shows the window regardless — a signed-out auto-start must be loud, never a silently dead agent. Precisely: the decision waits for the launch to settle, and only a launch that lands **authenticated** (`launchGate == .ready` with an account) stays hidden; every other settled surface — onboarding, a transient `retrying`, a terminal `needsUpdate`, `identityChanged`, `accountIndexUnreadable` — needs the user and shows the window, failing safe to visible exactly as linux does. The window is hidden by mounting and ordering it out, then closing it once the launch lands authenticated — the same state as a window close (§ Window close); SwiftUI's `defaultLaunchBehavior(.suppressed)` cannot serve, because the launch itself runs from the main window's content (`ContentView`'s `.task`), so a suppressed window never signs in. A Dock or Finder re-open raises the window (`applicationShouldHandleReopen`).

**Mechanism: `SMAppService.agent(plistName:)` over a LaunchAgent plist shipped inside the signed bundle** (`Fauna.app/Contents/Library/LaunchAgents/social.fauna.FaunaMacOS.plist` — the bundle executable as `BundleProgram`, `--autostart` in its arguments, `RunAtLoad`; the label is registered in `AppleIdentifiers`), registered and unregistered through `SMAppService` (macOS 13+; the deployment target is 15). Decided over the two alternatives: **(i) `SMAppService.mainApp`** carries no arguments and gives the app no reliable signal that it was launched at login, so the hidden launch could not be decided — its `keyAELaunchedAsLogInItem` open-event check is the recorded fallback if an agent job proves unable to host the GUI app, not the first choice; **(ii) the hand-written `~/Library/LaunchAgents` plist** the app writes today survives an app move or uninstall as an orphan launchd errors on at every login, and the app cannot read back whether the user turned the item off in Login Items — `SMAppService.status` reports exactly that (`.requiresApproval`), so the toggle can tell the truth and `openSystemSettingsLoginItems()` can route the user to the OS switch. A bundle-shipped plist moves and uninstalls with the app. The sync agent's own LaunchAgent (`LaunchdSyncAgentSpawner`, § Sync) is a different job with different needs — `KeepAlive`, a binary outside the bundle, `launchctl bootstrap` — and is not part of this mechanism.

**Why residency is the home-screen widget's answer on macOS (the design pass, 2026-09-26).** The widget renders what the running app publishes and a quit app publishes nothing ([`ios.md`](ios.md) § Home-screen widget → *Background currency*). The count is a fold over the account's read state on **sealed** messages, so whatever publishes must hold the conversations state: a sandboxed widget cannot (it holds no credential, by design), and the per-user sync agent — the only other resident process — holds no conversations state and, since its 2026-08-25 state move, no claim on the app-group container the widget reads, whose re-adoption would bring back the per-instance TCC prompt that move removed ([`../installers/macos.md`](../installers/macos.md) § Identifier domain, record items 5–6). Accepting the gap was the third candidate; it lost to the answer linux already ratified ([`linux.md`](linux.md) § Home-screen widget → *Background currency*), which macOS gets almost for free. So the promise's second half rests on the app being resident: from sign-in by auto-start, and across every window close because a close is never a quit (§ Window close) — a smaller residual than linux's, whose window close quits where no tray host exists. **The residual, declared:** an explicit Quit (⌘Q, the menu-bar Quit) freezes the count at its last value until the next login or launch — the same limit linux states for a quit — and no mechanism is owed for it: a quit is a deliberate user act, and the only process that could publish through it is the sync agent, ruled out above. [`common.md`](common.md) § Home-screen widget carries the one-line scope note; the catalog page ([`../../../features/home-screen-widget.md`](../../../features/home-screen-widget.md)) measures the app, not the quit, exactly as linux's per-desktop limit is recorded in its mechanism section rather than as a catalog absence.

**Implementation status today (2026-09-26): IMPLEMENTED.** The pure decisions live in `Fauna-macOS/App/AutoStart.swift` (`shouldRegister`, `route`, `shouldStartHidden`, `displayedChoice`, `enableAction`, `postAuthAction`), pinned by `AutoStartTests` under `just swift-test`; `completeAuthenticatedLaunch` calls `AutoStart.registerAtPostAuth()`; the plist ships through the `Fauna` target's *Embed Launch Agent* copy phase (asserted by `mac-app` and `test_apple_identifier_pins.py`); the hidden launch is witnessed by `test_an_autostart_launch_is_resident_with_no_window_and_keeps_the_count_current` (`test_home_screen_widget.py`, `macos`). The app no longer writes `~/Library/LaunchAgents/social.fauna.FaunaMacOS.plist` and owes no read-forward (the 2026-09-24 baseline reset) — a dev machine's leftover file is removed by hand. **Not covered by an e2e test, by construction:** the `SMAppService` registration itself (gated off under e2e, like linux's and windows'); the human's last inch is a real sign-out → sign-in showing Fauna in System Settings → General → Login Items and resident with no window.

---

## Sync

**LANDED 2026-07-13 (B2 cutover).** macOS converged onto the shared `fauna-sync-engine` — the Linux deployment shape, multiplexed over the same shared `EngineHost` driver, reached through the UniFFI `FfiSyncEngineHost` — with sealed chunks and location↔set bindings in the device-local location map. One byte-sync path; no app-managed daemon. Authority for the whole design: `../../behavior/sync-engine-deployments.md` § Apple apps — convergence design (read its § Implementation status today for what remains).

**LANDED 2026-07-19 (A4 atomic cutover) — resident bound-location engines moved into the per-user sync+backup agent.** The app no longer hosts resident, watcher-driven location engines in-process: `FaunaClient.start()` now builds a **one-shot-only** `FfiSyncEngineHost` (photo ingress, restore walks, per-file state reads — `startEngine`/`stopEngine` fail closed) and, post-auth, starts an `FfiSyncAgentProvisioner` (spawned/installed via `LaunchdSyncAgentSpawner` — launchctl bootstrap/kickstart, with `.dmg`-install LaunchAgent self-install) that keeps the external `fauna-sync-agent` process provisioned and syncing **with the app closed**. Binding a location on the **Settings → Folders** page (`MacFolderBindingSection`) now calls `LocationsModel.add`, which routes through the provisioner's `bindFolder`/`unbindFolder`/`listFolders` — the agent starts/stops the engine on its own side and reconciles the bound-location list as the UI's rendered truth; the device-local location map is now legacy-read-only (a one-time migration source + pre-login render seed, `Fauna-macOS/Sync/LocationMap.swift`). Per-file state still lives in the engine's per-set `SyncDb`, surfaced to the UI through `file_states` (the `sync-state-badge` on the Media page). Owner: [`sync-agent.md`](sync-agent.md) § Implementation status today (milestone A4) — the segment-backup `BackupUploadDriver` that stayed in-app through this cutover was **DELETED 2026-08-15** at the slice-5 flip — the nest-side coordinator it was waiting for took over, so macOS now hosts no segment-backup upload at all (D6's agent-hosting plan was withdrawn 2026-07-23 in favor of that nest-side model — `sync-agent.md` § Scope per platform, `../../behavior/backup-restore.md` § Background Tasks → *Flip status (slice 5)*). **E2E spawner selection (landed 2026-07-24):** production launches use `LaunchdSyncAgentSpawner`; an e2e launch uses `FfiChildAgentSpawner` — a private per-launch child agent, opt-in via an env flag — only when a test asks for a real agent, and no spawner (folder rows seeded by the `sync_inject_locations` test seam, no agent behind them) otherwise. The machine-global launchd LaunchAgent/plist stay untouched either way (`apple-e2e-automation.md` / e2e-launch-isolation.md § point 10); isolation for the opt-in child holds because the driver already gives each e2e launch its own home directory, and the agent socket is home-derived.

**Finder integration (ratified 2026-07-18; landed 2026-07-19, M0–M5 — `on-demand-files.md` § On-Demand Files → Apple File Provider binding is authoritative and updates fastest; the signed end-to-end appex tier_3 run stays gated on the org's Apple Developer cert).** The on-demand surface is a shared macOS+iOS **File Provider extension** — one domain per folder this device accepts under a Fauna location in the Finder sidebar, the extension hosting per-set engines itself (app-group state; `BackupKey` + bearer via the shared Keychain). Behavior, the one-presence-per-set rule, and the `folder-on-demand-toggle` UI are owned by `../../behavior/on-demand-files.md` § On-Demand Files → Apple File Provider binding + `../../ui/folders.md` § Binding; packaging (the thin `.xcodeproj` emitting the `.appex`) by `../installers/macos.md`; milestones: the 2026-07-18 design record (tracked internally).

**Deleted with the B2 cutover:** the bespoke Swift `SyncEngine` (FaunaKit Core), the `FSEventStream`-based `DirectoryWatcher` (replaced by the shared engine's own `notify` watcher, which uses FSEvents on macOS), and `SyncDaemonManager` + `SyncDaemonConfig` — the `launchctl` LaunchAgent glue for a bundled `fauna-sync` binary the SwiftPM build never actually shipped (its path was inert and its state stayed `.notInstalled`). Its shell-outs were the control-plane violation declared in § Implementation status today; `bins/fauna-sync` remains available as the standalone headless daemon, self-installed, simply no longer app-managed. (The per-platform sync-trigger comparison — FSEvents here, inotify on linux, `ReadDirectoryChangesW` on windows, `WatchedDirectoryManager` on android — is owned by `docs/goal/behavior/file-sync.md`.)

Since the A4 cutover, sync is **always-on even with the app closed** — the per-user `fauna-sync-agent` outlives the app session (vs iOS's pull-based `BackgroundScheduler` under OS control, which is unchanged and unaffected by A4 since iOS keeps the full in-process host). See `sync-agent.md` § Packaging + lifecycle for the LaunchAgent mechanics.

---

## UI Patterns

### Navigation

macOS uses a persistent sidebar with master-detail split views instead of the iOS tab bar (`FeedSplitView`, `ContactSplitView`, `EventSplitView`, conversations split). The cross-page navigation model (per-form-factor patterns, tab membership) is owned by `docs/goal/ui/README.md` § Navigation + ui.yaml `navigation.*`.

### Settings shell

Settings are presented in-window as a **sidebar-swap rail shell** (`SettingsShellView` + `SettingsNavRail`), one sub-page visible at a time — the same shell concept as the admin shell. Authority for the page set, navigation model, and per-sub-page behavior is `docs/goal/ui/settings.md` § Navigation model; the shared sub-page enum is `FaunaKit/Core/SettingsPage.swift`.

### Quick Switcher

Cmd+K opens a floating quick switcher overlay (`Commands/QuickSwitcher.swift`) for fast navigation to contacts, conversations, and posts. Not present on iOS.

### Backup UI

Richer than the iOS basic list: snapshot timeline, snapshot diff viewer, per-set conflict review, integrity check panel. Behavior owners: `docs/goal/ui/backups.md` + `docs/goal/behavior/backup-restore.md`.

### Admin Dashboard

Full administration UI for the nest **admin** — the shared FaunaKit admin shell (`AdminShellView` + `AdminPage`), identical page set on iOS (`behavior/admin.md` owns the shell + pages).

### Nest Provisioning Wizard

Full provisioning wizard from the WelcomeView "Create New Nest" button, with macOS-adapted layouts (a wider minimum window, and a monospaced DNS-instructions block with an `NSPasteboard` copy button in place of a mobile share sheet). Shares `OnboardingVM` — a thin Swift wrapper around the shared Rust `OnboardingMachine` (UniFFI) — with iOS via FaunaKit.

---

## P2P

macOS has no P2P page surface — the unsanctioned `PeerContactsView` / `WireGuardView` + both view-models (including the `SettingsPage.p2p` case that routed to them) were removed 2026-07-13 as unspecified in ui.yaml, and the WireGuard stack they fronted was deleted outright 2026-08-23. macOS is now one of five apps with no P2P surface (linux is the only one with a page). The whole P2P architecture, including per-app status, is owned by `docs/goal/behavior/p2p.md`; this doc does not restate it.

---

## Secret Storage

One shared FaunaKit `KeychainStore` serves both apple apps — the mechanics and the device-bound-default + opt-in-iCloud-Keychain-backup policy (landed 2026-07-19) live in **[`ios.md`](ios.md) § Credential Storage**; the cross-app contract lives in `common.md` § Credential storage. This doc adds nothing macOS-specific.

---

## Entitlements

| Entitlement | Purpose |
|-------------|---------|
| `com.apple.security.app-sandbox` | App Sandbox |
| `com.apple.security.network.client` | Outbound network connections |
| `com.apple.security.personal-information.photos-library` | PhotoKit access to the Photos library (used by `PhotoBackupEngine`), paired with `NSPhotoLibraryUsageDescription` for the TCC consent prompt. **Implementation status:** the entitlements file currently carries the broader `com.apple.security.assets.pictures.read-write` (the `~/Pictures` *folder* entitlement) and the app is not yet sandboxed (`app-sandbox` absent), so at dev runtime PhotoKit access is governed by TCC + the usage string alone; the sandboxed-release entitlement above is the target. |
| `com.apple.security.application-groups` (`7457N3M72H.group.social.fauna.shared`) | Shared app-group container between the app and the `Fauna-FileProvider` appex — houses the shared `FileProviderCredentialStore` (app-dead `BackupKey` + bearer, data-protection Keychain) that the app provisions and the sandboxed extension reads back, and (since 2026-08-25) the extension's state root, which the app only *stewards* (`domain-owners.json`, the pin replica, FP-bound `file_states` reads) — the app's own state lives in the user domain (`on-demand-files.md` § On-Demand Files → Apple File Provider binding, *state unification*). The sync agent names no group at all. The **id itself** — its spelling, its Rust/Swift owners, the Team-ID prefix — is owned by [`../installers/macos.md`](../installers/macos.md) § Identifier domain. |
| `com.apple.security.application-groups` (`7457N3M72H.group.social.fauna.account`) | The **app-only** keychain access group `KeychainStore`'s data-protection-plane rows live in (2026-08-25; [`ios.md`](ios.md) § Credential Storage, *keychain plane*). A second group rather than the shared one precisely so the sandboxed extension can never read the identity seed; keychain-only — no container directory is ever resolved for it; no appex or the agent may name it (`test_apple_identifier_pins.py`). |

---

## MLS Integration

Uses `FaunaFFI.xcframework` (UniFFI into the shared conversations session). SwiftData persists MLS state alongside app data; the protocol, key-package lifecycle, and platform-bindings table are owned by `docs/goal/behavior/direct-messages.md`. The macOS implementation is identical to iOS — only the persistent storage path differs. All DM crypto rides the conversations rail's own per-session `MlsEngine` (`conv-mls.db`); the separate process-global `MlsManager` singleton (a Swift wrapper over the standalone Rust engine, opened at every macOS launch but driving zero crypto — no caller, iOS never wired it) was **retired 2026-07-19** as dead code, and the now-caller-less Rust plane it wrapped was deleted outright 2026-07-22 (`libs/fauna-ffi/src/mls.rs` no longer exists).

---

## Key Differences from iOS

| Aspect | macOS | iOS |
|--------|-------|-----|
| Sync | Bound locations synced by the external per-user `fauna-sync-agent` (resident, watcher-driven, runs with the app closed since the A4 cutover 2026-07-19); the app's own in-process host is one-shot-only (photo ingress, restore, per-file reads) | Same one-shot engines, driven from `BackgroundScheduler` (no location bindings, no agent) |
| Navigation | Persistent sidebar + split views | Tab bar + More hub (`ui/README.md` § Navigation) |
| Windows | Multiple (main, settings, onboarding) | Single window |
| Menu bar | Yes (+ menu-command shortcuts) | N/A |
| Updates | notice-only check (you install the newer `.dmg`/`.pkg`) | App Store |
| Backup UI | Timeline, diffs, conflict review, integrity checks | Basic list |
| Secret storage | Shared `KeychainStore` — see [`ios.md`](ios.md) § Credential Storage | Same (one implementation) |
| P2P | No page surface (removed 2026-07-13 — `behavior/p2p.md`) | Same — no page surface |
