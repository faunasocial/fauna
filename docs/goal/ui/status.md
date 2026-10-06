# Status — target state

Owns: status-page
Status: partially-specified — the mounting model + per-app section inventory are ratified (2026-07-10 reconciliation). iOS and Android status surfaces have since landed (android 2026-04-16, iOS 2026-07-18), closing the mounting-parity gap this doc originally left open — see § Goal + § Done definition. The `status_snapshot()` shape is ratified for its node, sync, MLS and build legs (2026-09-27, § State & data shape — built in `libs/fauna-client-status`, tui rendering it first) and still open for the connection, service and P2P legs; per-app ID/section gaps (tui's copy-button IDs, android's quota mounting — the IDs exist on its Account screen, not its Status screen — and macOS's narrower section set) are tracked in § Layout & flow + § Done definition — resolved by whichever app's own build track next touches its status/settings surface. **NEEDS-RULING (found sweep):** § Layout & flow's "Connection status" row credits macOS's ✅² for a *persistent global chrome bar* (`ConnectionStatusBar`) rather than page-embedded content — but tui (`ui.rs::register_frame`, connection-status is shell-level, painted "on every screen including unauthenticated ones" per `ui-actual-tui.yaml`'s own global-elements notes), iOS (`MainTabView`'s `.safeAreaInset` wraps the whole `TabView`, so `ConnectionStatusBar` stays visible while a `NavigationLink` pushes `StatusDetailView`), and android (`FaunaNavHost.kt`'s `ConnectionStatusBar()` sits outside the `NavHost`, above every `composable` route including `settings/status`) all have the *same* persistent-chrome pattern today, yet are marked "—". Either extend the ✅ credit (with footnotes) to tui/ios/android for consistency, or state explicitly that the row means "page-embedded content only" and drop macOS's footnote-based exception — a genuine editorial-convention call this sweep did not make unilaterally.
Authority: ui.yaml (`status` page) owns element IDs + per-page element scope; this doc owns the status/diagnostics surface's behavior + section inventory and its per-app mounting; defers the settings-shell navigation model to [`settings.md`](settings.md) § Navigation model, the key-package lifecycle to [`../behavior/direct-messages.md`](../behavior/direct-messages.md) (registry `mls`), and admin entry gating to [`../behavior/admin.md`](../behavior/admin.md).

## Goal

The Status surface shows the local node's diagnostics: identity + copy
affordances, connection state, quota, node info, and (where built) service /
sync / P2P / MLS detail. **It is not a standalone page anywhere** — the fleet
converged on status-inside-Settings (2026-06-04 windows lead), and all seven
apps now mount it there:

| App | Mounting |
|---|---|
| windows | FIRST/default sub-page of the Settings sidebar-swap shell (`StatusPage`) |
| macos | Settings-shell sub-page (`MacStatusView` via `SettingsShellView`) |
| linux | `views/status.rs` doubles as the Settings landing view |
| web | inlined into `/settings` (no separate route) |
| tui | Settings rail Root sub-page (`settings/root.rs::root_elements`) doubles as the status landing — same "default sub-page" shape as linux/windows/macos |
| ios | Settings sub-page pushed by value (`StatusDetailView`, `NavigationLink(value: SettingsPage.status)`) — mobile idiomatic nav per [`settings.md`](settings.md); landed 2026-07-18 |
| android | Settings sub-page (`StatusScreen`, route `settings/status`) — mobile idiomatic nav; landed 2026-04-16 |

## Implementation status today

**The shared snapshot's four ratified legs are BUILT (2026-09-27) and tui renders them (§ State & data shape).** `libs/fauna-client-status` holds `StatusSnapshot`, `StatusClient::node()`, the `agent`-feature embedding of the sync projection, `BuildLeg` with the 12-hex abbreviation, and the `StatusText` projection; `libs/fauna-build-commit` is the one build-script derivation of `FAUNA_BUILD_COMMIT`, adopted by `apps/fauna-tui/build.rs` and `bins/fauna-nest/build.rs`; `fauna_conversations::snapshot::secure_channel_count` is the MLS channel count. tui's Settings root (`apps/fauna-tui/src/settings/root.rs`) paints Node, Sync, Encryption and Build from it under the seven canonical IDs, each section absent until its leg is loaded, and the five `test_settings.py` witnesses run on tui. **macOS and iOS lifted (2026-09-29):** `libs/fauna-ffi/src/status.rs` is the FFI face — `FfiStatusClient::node()` (via `FfiNestClient::status()`), `status_text` (`fauna_client_status::render` with the app's own catalog handed in as `FfiStatusLookup`) and `status_sync_leg` (the agent's `FfiAgentSyncStatus` → the sync leg); `libs/fauna-ffi/build.rs` stamps `FAUNA_BUILD_COMMIT` through `fauna-build-commit`. FaunaKit's `StatusVM` loads the node leg and the MLS leg (`getKeyPackageCount` + the conversations manager's `secureChannelCount()`) and `StatusLegSections` paints all four sections on `MacStatusView` and iOS `StatusDetailView`; macOS's sync leg rides `SyncAgentHealthModel.syncLeg`. **Not yet lifted — the other four apps still carry their per-app copies:** linux reads `fauna.nest.info` itself (`client.rs::fetch_nest_info`) and renders the nest-side `files_synced` summary rather than the local backlog; windows fills the sync rows from the FFI agent surface but its MLS rows (`KeyPackageCountText`/`DmChannelCountText` on `StatusPage.xaml`) are never populated — they paint the `--` placeholder — and it carries no node or build section; web reads `VITE_GIT_SHA` directly and paints a literal `dev` on an unstamped build where the snapshot's rule is *absent*; android renders none of the four sections. The wasm face of `fauna-client-status` lands with the first web lift, not ahead of it; the FFI face is landed (Apple), and windows and android adopt it.

## Layout & flow

Section inventory per app today (the spec target is the union; per-app
gaps are listed, not implied uniform):

| Section | windows | macos | linux | web | tui | ios | android |
|---|---|---|---|---|---|---|---|
| Identity + copy affordances | ✅ | ✅ | ✅ | ✅ | ✅¹ | ✅ | ✅ |
| Connection status | ✅ | ✅² | ✅ | —⁷ | — | — | — |
| Quota (`quota-inbox`/`quota-storage`/`quota-devices`) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅³ |
| Node info (domain, version) | — | ✅¹⁰ | ✅ | — | ✅⁸ | ✅¹⁰ | — |
| Service info | ✅ | — | — | — | — | — | — |
| Sync status | ✅⁶ | ✅¹⁰ | ✅⁴ | — | ✅⁸ | — | — |
| P2P tunnel/peer summary | — | — | ✅⁵ | — | — | — | — |
| MLS details (read-only count/state display) | ✅⁹ | ✅¹⁰ | — | — | ✅⁸ | ✅¹⁰ | — |
| Build (commit SHA) | — | ✅¹⁰ | — | ✅ (`VITE_GIT_SHA` in settings) | ✅⁸ | ✅¹⁰ | — |

¹ tui's actor-id copy button uses `account-actor-id-copy-btn`, not the
canonical `status-actor-id-copy-btn` (it reuses the Account sub-page's
element), and tui has no node-url display or copy affordance at all
(`apps/fauna-tui/src/settings/root.rs`) — a real per-app ID/coverage gap,
tracked in § Done definition, not a doc-only inconsistency.

² macOS shows connection status via the shared FaunaKit `ConnectionStatusBar`
— global chrome pinned above the sidebar/detail split on every page
(`Fauna-macOS/Views/MainWindow/ContentView.swift`), not a section embedded in
`MacStatusView` the way windows/linux embed theirs. Owner:
`../architecture/transport.md` § Connection-status indicator.

³ android's Status screen (`ui/screen/status/StatusScreen.kt`) shows a plain storage-usage card (progress bar + used/max text) with
no `quota-section`/`quota-inbox`/`quota-storage`/`quota-devices` test IDs and
no inbox/device rows. The full canonical quota family (all four IDs, with inbox and device rows) is built on android's Account
settings screen instead (`AccountSettingsScreen.kt`, beside `feature-limits-section`) — the gap is where it is mounted, not whether it exists.

⁴ linux's Sync section (`Files Synced` / `Last Sync` rows) — **FIXED
2026-07-19**: was built but never wired to live data after initial paint (a
static "0"/"Never" placeholder, not a functioning display). `FaunaClient::
fetch_sync_status_summary` now aggregates `fauna.sync.files` across every
device-sync location binding (`sync_agent::current_locations()`) at the universal
post-auth hook, best-effort/log-only per set (`apps/fauna-linux/src/client.rs`,
`views/status.rs::update_sync_status`).

⁵ linux's P2P group polls `settings::p2p_tab::get_tunnel_status_summary()` and
the peer list every 3s — a read-only mirror of the Settings → P2P tab
summary; owner: `../behavior/p2p.md`.

⁶ **Fixed in two passes (narrow 2026-08-02 am, projection 2026-08-02 pm);
one windows-side remainder.** `StatusPage.xaml`/`StatusPage.xaml.cs`
render a wired Sync group (`SyncStatusText`, `FilesPendingText` ←
`StatusViewModel.FilesPending`, `BytesPendingText` ← `.BytesPending`). The
original claim was that `DirectNestClient.GetServiceStatusAsync`
(`FaunaApp.Core/Services/DirectNestClient.cs:403`) merely needed to "source
real sync data" the way linux's ⁴ fix did — false: `DirectNestClient` is an
HTTP client to the remote nest and has no way to see this device's local
sync-agent state at all, so the narrow fix moved the Sync* fields to the
LOCAL sync agent, mirroring `MainViewModel.PollSyncAgentStatusAsync`'s
established pattern — wired in `StatusViewModel.LoadAsync`. (That read went
over the C# named-pipe client at the time; since the 2026-08-05 pipe
retirement it is `SyncStatusAsync()` on the shared FFI agent surface. The
placement conclusion — local agent, not `DirectNestClient` — is unchanged.) The per-engine backlog projection the
narrow fix exposed as missing **landed the same day in shared Rust**
(priority #2 — `fauna_sync_engine::db::SyncDb::transfer_backlog` behind one
shared handler helper in `bins/fauna-sync-agent`; semantics owned by
`../architecture/apps/sync-agent.md` § Local agent health → *the
sync-status projection*), so `FilesPending`/`BytesPending` now render REAL
numbers on windows with zero C# changes, and every pipe consumer receives
the same data. `LastSync` now reads into `StatusViewModel.LastSync` (wired
2026-08-10 — a new `status/sync/last_sync` row on `StatusPage.xaml`, relative-time
formatted, "Never" baseline when the agent has not finished a pass yet); the
`en-US` string `photo_backup/last_sync` stays unused by this page (a different
concept — the photo-backup feature's own last-upload stamp) and
`status/sync/files_synced` stays unused (no consumer asks this page for a raw
synced-file count). **`EngineInfo.last_sync` remains unbuilt and is NOT windows-owed
work** (corrects the prior note here, which queued it as windows' slice alongside
its C# codec regen): the 2026-08-05 codec retirement deleted the C# pipe codec
rather than regenerating it, and windows dropped `ListEngines`/`EngineInfo`
consumption entirely with it — the wire field's only other reader, fauna-tui, uses
`EngineInfo` for `corpus_reseal`/`reseal_drain`, never a timestamp. Nobody consumes
per-engine freshness today; the field stays a cheap additive addition
(`../architecture/apps/sync-agent.md` § Local agent health) for whenever a real
consumer appears, not queued work in any NEXT file.

⁷ **web shows no connection status at all on this page today** — corrects an
overstated ✅ here (found sweep). Web's global `connection-status`
indicator lives in the root shell sidebar (`routes/+layout.svelte`), but that
whole sidebar — connection-status included — is bypassed while
`$page.url.pathname.startsWith('/app/settings')` (the same bare-canvas
sidebar-swap the Settings shell uses for onboarding/admin), and
`routes/settings/+layout.svelte`'s own rail renders no connection indicator
of its own. `ui-actual-web.yaml`'s `settings` page-element list and its
`global.elements` list both independently omit `connection-status`,
corroborating the code read. Unlike macOS (footnote ²), web has neither an
embedded section nor persistent chrome showing connection state while
viewing Status — a real per-app gap, not just a doc-only inconsistency.

⁸ tui renders all four sections from the shared snapshot (2026-09-27, § State & data shape, § Implementation status today): Node (`status-node-domain`/`-version`), Sync as the LOCAL agent backlog like windows — `status-sync-pending` + `status-sync-last` — not linux's nest-side files-synced summary, Encryption (`status-mls-key-packages`/`-channels`) and Build (`status-build-sha`). Each section is absent until its leg is loaded (the node and MLS reads resolve on the landing's one awaited op, the sync leg once the agent first answers), and the Build section is absent on an unstamped build.

⁹ windows' MLS group exists on `StatusPage.xaml` (`MlsInitText`, `KeyPackageCountText`, `DmChannelCountText`) but nothing in `StatusPage.xaml.cs` ever fills the three — they paint the `--` placeholder for the life of the page (found 2026-09-27 while ratifying the snapshot; the key-package count windows does read, `SettingsViewModel.KeyPackageCount`, feeds the Encryption sub-page, not this one). The ✅ credits the section's presence; its counts arrive with the windows leg of the snapshot lift.

¹⁰ macOS (`MacStatusView`) and iOS (`StatusDetailView`) render Node, Encryption and Build from the shared snapshot (2026-09-29, § Implementation status today) through one shared FaunaKit view (`StatusLegSections`, over the FFI `statusText` projection); macOS additionally renders Sync as the LOCAL agent backlog off `SyncAgentHealthModel`'s 10 s `GetServiceStatus` tick, like windows and tui. iOS has no local agent, so its Sync leg is `nil` and the section is a declared absence (§ State & data shape, the sync leg). Each section is absent until its leg loads; Build is absent on an unstamped build.

Top-level affordances: copy actor ID (`status-actor-id-copy-btn`), copy node
URL (`status-node-url-copy-btn`) — canonical on windows/macos/linux/web/ios/android;
tui is the one outlier (see note ¹ above).

The "reproducible builds note" is unbuilt everywhere (target-state only).

Cross-reference (admin): when the user is a nest admin (gated by the
`fauna.account.am_i_admin` check), the surface offers a link to the admin
shell. The link itself belongs to the admin shell — see
[`../behavior/admin.md`](../behavior/admin.md) for gating + entry-point
contract — and is listed here only as a cross-reference.

### Encryption / MLS key packages

Where an app shows MLS detail here (windows and tui today — the MLS row of
§ Layout & flow's table; macOS's `MacStatusView` carries no MLS/
Encryption section, confirmed by reading the current view: Identity, Quota,
Actions only), it is a **read-only display** — published key-package count and
secure-channel count, the snapshot's MLS leg (§ State & data shape; windows'
extra engine-state row is its own unfilled placeholder, footnote ⁹). This page owns no key-package mutation:

- The **replenish mechanism** is the shared `ConversationsSession` top-up to
  `KEYPACKAGE_TARGET = 20` — owner:
  [`../behavior/direct-messages.md`](../behavior/direct-messages.md) § Key
  Package Management (which also records the 2026-07-10 unification ruling:
  the legacy per-app 5→10 page-load floors are declared drift to delete).
- The **Refresh Keys** button and the Encryption settings sub-page it lives on
  belong to [`settings.md`](settings.md) (the Settings shell's sub-page
  inventory), not this surface.

### P2P tunnel/peer summary

linux-only today: a read-only mirror of the tunnel-active flag and peer count
(`views/status.rs`'s P2P group, live-polled every 3s), duplicating what the
Settings → P2P tab already shows. This page owns no P2P configuration — the
toggle and full peer/contact list live on the P2P sub-page, owner:
[`../behavior/p2p.md`](../behavior/p2p.md).

### Build

The git commit the running build was made from, abbreviated to the snapshot's `BUILD_SHA_ABBREV` = 12 hex characters (§ State & data shape, the build leg) — web (`VITE_GIT_SHA`, the same derivation in `apps/fauna-web/build-id.js`) and tui today. Plain text, never a link: builds are stamped with a development-repository commit that the public mirror's history does not carry. **An unstamped build (no `FAUNA_BUILD_COMMIT`, no git checkout — a source tarball) renders NO build row**, the same absent-until-known rule as the node and MLS sections; web's literal `dev` in that case is drift the web lift removes. Target adds a short reproducible-builds note (unbuilt; exists nowhere yet).

## Element IDs

Page elements (from ui.yaml): `page-heading`, `status-actor-id-copy-btn`,
`status-node-url-copy-btn`, `settings-view` (root landmark for this page's
content — web wraps its whole settings route in it; linux/macos scope it to
Status specifically), `settings-nest-url-field` (read-only nest URL display,
web), `error-message`. The quota family
(`quota-section`/`quota-inbox`/`quota-storage`/`quota-devices`) and
`account-actor-id` are canonical, e2e-read IDs (see ui.yaml's windows note).
The node/sync/MLS/build detail IDs are canonical too (user-approved 2026-09-27): `status-node-domain` + `status-node-version` (node info), `status-sync-pending` (this device's local sync-agent backlog, files/bytes) + `status-sync-last` (when the last pass finished), `status-mls-key-packages` + `status-mls-channels` (the read-only MLS counts), `status-build-sha` (the abbreviated commit of the running build) — one ID per section fact, on every app that renders the section (the § Layout & flow matrix says which do today).
Remaining service/connection/P2P detail IDs are per-app implementation detail —
lift to canonical ui.yaml IDs (user approval) before adding behavior to them.

## State & data shape

**Ratified 2026-09-27 for the node, sync, MLS and build legs — the shared snapshot is `fauna_client_status::StatusSnapshot` (`libs/fauna-client-status`), an AGGREGATE of four sources that already exist in shared Rust, never a second definition of any of them.** The crate owns the shape, the one nest read the shape needs, the build stamp's semantics and the text projection every app paints; the four sources keep their owners. Anything not listed below (connection state, the service registry, P2P, quota) stays TBD here or is owned elsewhere (quota: [`settings.md`](settings.md) § Quota; the connection indicator: `../architecture/transport.md` § Connection-status indicator; P2P: `../behavior/p2p.md`).

- `StatusSnapshot { node: Option<NodeLeg>, sync: Option<SyncLeg>, mls: Option<MlsLeg>, build: BuildLeg }` — every `Option` means *not loaded, or not applicable on this app*, and a section whose leg is `None` is **not rendered** (no placeholder, no zero): the un-hydrated-paint rule the quota and feature-limits sections already follow.
- **Node leg** — `NodeLeg { domain, version }`, the nest's own `fauna.nest.info` reply (`domain` + `version`), read by `StatusClient::node()` — the crate's only RPC (the `FeaturesClient::node_capabilities` shape). One read per visit to the surface.
- **Sync leg (resolved 2026-08-02, embedded 2026-09-27)** — `SyncLeg { files_pending, bytes_pending, last_sync }` are the sync-agent pipe's `SyncStatusInfo` fields verbatim, a real shared-Rust projection whose field semantics [`../architecture/apps/sync-agent.md`](../architecture/apps/sync-agent.md) § Local agent health → *the sync-status projection* owns; the crate's `agent` feature carries the `From<&SyncStatusInfo>` embedding and nothing else. `None` on an app with no local agent (web, iOS, android) and, on a desktop, until the agent first answers `GetServiceStatus` — an agent that is down is reported by `sync-agent-status`, not by a zero backlog here. (linux's footnote-⁴ `files_synced`/`last_sync_at` summary is the *nest-side* view — `fauna.sync.files` across bindings — a complementary fact, not a competing definition of the same field; the canonical `status-sync-*` IDs carry the local leg.)
- **MLS leg** — `MlsLeg { key_packages, channels }`: `key_packages` is the bearer's own `fauna.conversations.keypackage.count` (the existing `ConversationsClient::keypackage_count` / FFI `KeypackageCount` / wasm `keypackage_count` faces — the crate adds no second wrapper for a kind every face already carries), `channels` is `fauna_conversations::snapshot::secure_channel_count` over the conversations snapshot: the threads on the `FaunaMls` rail, each of which is one MLS group, so bridged (SMTP/Bluesky/Nostr/Mastodon) threads never count. `None` until the key-package read resolves.
- **Build leg** — `BuildLeg { commit: Option<String> }`, the commit the running artifact was compiled from, stamped at compile time as `FAUNA_BUILD_COMMIT` by the ONE build-script derivation `libs/fauna-build-commit` (the environment when the build passes it — the nest image's build argument — else the checkout's `HEAD`, else nothing; `dev` and empty read as nothing). Native binaries call it from their `build.rs` (`fauna-tui`, `fauna-nest`, `fauna-ffi` — the last since 2026-09-29; the linux app adopts it with its lift); web's `VITE_GIT_SHA` is the same derivation in `apps/fauna-web/build-id.js`. The projection abbreviates to `BUILD_SHA_ABBREV` = 12 hex characters; an unknown commit renders no row (§ Build).
- **The text projection** — `StatusText` (`fauna_client_status::render`) is the seven `status-*` element texts, each `Option<String>` (`None` = do not render): the domain and version verbatim; `status-sync-pending` = the shared `status.sync.pending_summary` string over the file count and `fauna_core::format::byte_size`; `status-sync-last` = the shared `common.never` while `last_sync` is `None`, else `fauna_core::format::relative_time_text` (the `local-clock` feature); the two MLS counts as bare digits; the abbreviated commit. Labels beside the values are the shared `status.*`/`common.*` strings and never part of the element text, so a witness reads the bare value on every app.

## Where logic lives

- **Status aggregation** — shared Rust. The node, sync, MLS and build legs are `libs/fauna-client-status` (2026-09-27, § State & data shape): the snapshot shape, `StatusClient::node()`, the sync embedding, the build stamp's semantics and the `StatusText` projection, over sources that keep their owners (the sync projection: `../architecture/apps/sync-agent.md` § Local agent health; the key-package count: the conversations client; the channel count: `fauna-conversations`; the stamp: `libs/fauna-build-commit`). An app fetches the legs on its own cadence (tui: the landing's one awaited op + the agent poll) and paints `render`'s texts; it derives none of them. The service-registry and connection legs stay TBD — shared Rust when they come.
- **Connection state derivation.** TBD — shared Rust.
- **Copy buttons.** App glue.

## User actions

| Element | Action | Where it runs |
|---|---|---|
| `status-actor-id-copy-btn` | Copy actor ID. | App glue. |
| `status-node-url-copy-btn` | Copy node URL. | App glue. |

## Persistence

None — status is read-only state derived from running services.

## Errors & edge cases

- `error-message` page-level.
- Disconnected, partial-service, sync-degraded — TBD; snapshot variants.

## Architectural rules

1. **The mounting model is status-inside-Settings** (§ Goal) — a
   Settings-mounted status surface with the same core IDs on every app,
   NOT a standalone page (the fleet converged away from standalone
   mountings; adding one would be a divergent shape). iOS and Android closed
   the mounting-parity gap (android 2026-04-16, iOS 2026-07-18); the open
   work is now ID/section-level, not mounting-level (§ Layout & flow, §
   Done definition).
2. Observer-driven rendering once shared snapshot exists.
3. Same `status-*` IDs on every app regardless of mounting.

## Don't do these

- Don't render Status data per-app. Shared Rust supplies it.
- Don't introduce `web-status-*` IDs to distinguish web's inlined status — same IDs everywhere.
- Don't put key-package mutation on this surface (§ Encryption / MLS key packages — display only).

## Done definition

- [ ] `status-actor-id-copy-btn`/`status-node-url-copy-btn` render with canonical IDs on every app (Settings-mounted per § Goal) — closed on windows/macos/linux/web/ios/android; tui is the sole remaining gap (its Root sub-page reuses `account-actor-id-copy-btn` and has no node-url copy affordance).
- [x] iOS and Android gained the Settings-mounted status surface with the same core IDs (parity gap closed: android 2026-04-16, iOS 2026-07-18).
- [x] Node/sync/MLS/build detail IDs canonicalized in ui.yaml (user-approved 2026-09-27, § Element IDs) and tagged on the apps that render each section today.
- [ ] Every app renders the node/sync/MLS/build sections under those IDs from the shared snapshot — **tui done 2026-09-27** (§ Implementation status today); **macOS and iOS done 2026-09-29**; linux, web, windows and android owe the lift, replacing their per-app reads where they have them.
- [ ] Quota/service/P2P detail IDs canonicalized in ui.yaml and rendered from the shared snapshot (user approval for ui.yaml additions) — android's quota family is mounted on its Account screen rather than its Status screen, and macOS's section set is narrower than the canonical union today (§ Layout & flow).
- [ ] `tests/e2e-unified/ui-actual-<app>.yaml`'s `status` block refreshed; `ui-actual-lint` introduces no new errors.

## Reading list

1. `principles.md` — product invariants + engineering principles.
2. `tests/e2e-unified/ui.yaml` — `status:` page block (+ the per-app mounting notes).
3. [`settings.md`](settings.md) — the Settings shell this surface mounts in.
4. [`../behavior/direct-messages.md`](../behavior/direct-messages.md) — key-package lifecycle owner.
5. `tests/e2e-unified/ui-actual-<app>.yaml`.
