# Linux App — target state

Owns: linux
Status: ratified — as-built architecture; reference app for handle-first onboarding (full `LaunchMachine` + `AwaitingManualDns` slot adopted)
Authority: linux-app architecture — the `fauna-desktop` monolith (GTK4↔tokio `UiMessage` bridge, in-process crate linking, sqlite cache shape, the app's control surface over the external per-user sync+backup agent, the in-process e2e automation agent), linux credential storage mechanics (libsecret + the declared test-build-only file fallback), and tray/notification/desktop integration; cross-app behavior → [`common.md`](common.md) (incl. the credential-storage contract); the P2P layer → [`../../behavior/p2p.md`](../../behavior/p2p.md); onboarding flow → [`../../behavior/onboarding.md`](../../behavior/onboarding.md); the per-user sync+backup agent → [`sync-agent.md`](sync-agent.md); multi-account instance scoping, the launch-collision chooser and the per-account raise channel → [`account-scoping.md`](account-scoping.md).

Last verified: 2026-08-19 (docs-consistency sweep — re-checked against current code over the 79-commit span touching `apps/fauna-linux/` since the prior sweep. Specifically resolved: today's i18n consolidation deleted `apps/fauna-linux/src/i18n/strings.rs` and turned `src/i18n/mod.rs` into a `pub use fauna_i18n::strings;` re-export, and lifted the month/weekday name lookup to `fauna_i18n::time` — this doc never described linux's own i18n string generation/emission mechanism or cited the deleted file anywhere, so no claim here went stale (the corresponding fix landed in `docs/goal/ui/events.md` § Where logic lives, in the same commit, which owns that claim). Also re-checked: `UiMessage`'s variant set (`app.rs`) unchanged; the credential-storage test-build-only gate (`client.rs`) unchanged; the tray host-detection mechanics (`tray.rs` — `TRAY_HOST_AVAILABLE`, `watcher_online`/`watcher_offine`, `ProvideXdgActivationToken`) unchanged; Multi-Account Instance Handling's launch-collision mechanics (`main.rs` — `NON_UNIQUE`, `FAUNA_BOUND_ACCOUNT`, `show_launch_instance_chooser`) and `account_scope::become_session_instance`/`resolve_and_adopt` (`account_scope.rs`) unchanged; the `Cargo.toml` dependency versions in § Key Dependencies unchanged. Noted but out of scope for this doc: the new `libs/fauna-client-account-runtime` crate and `AccountRuntimeHost` lifecycle (new `apps/fauna-linux/src/account_runtime.rs`) — that architecture is owned by `docs/goal/architecture/account-data-plane.md`, which both commits updated directly; linux.md makes no claim about it and needs none.) | Source: `apps/fauna-linux/`

## Goal

- **Declared absences (owner [`../../behavior/family-safety.md`](../../behavior/family-safety.md) § The account age band):** no store age signal and therefore no `invite-request-age-notice` (D3 — at most the two store-distributed mobile apps ever receive one; the id is `platform_elements: {android, ios}` in ui.yaml), and no kids flavor (the kids-app bullet: the kids door is a store construct, android and ios only). The e2e gate for both is `declared_absence`, never `skip_unbuilt`.

A single monolithic Rust binary `fauna-desktop` — UI, async backend, MLS, and P2P all in one process — with no FFI and no UniFFI boundary anywhere in it. Internal crates (`fauna-core`, `fauna-client-core`, `fauna-mls`, `fauna-peer`, `fauna-wireguard`) link directly as Cargo dependencies; the GTK4 main thread and the tokio runtime communicate over an mpsc channel of `UiMessage` values. System integration (credentials, tray, notifications) goes through standard freedesktop interfaces. **One declared exception:** the always-resident file-sync and backup engines run in a companion per-user sidecar process, `fauna-sync-agent`, which the app provisions at post-auth and talks to over a unix socket — so sync and backup keep running while the app is closed (ratified 2026-07-18, cut over 2026-07-19 — milestone A3; § File Sync; owner: [`sync-agent.md`](sync-agent.md)).

---

## Architecture: Monolithic Rust Binary

The Linux app is built around a single binary `fauna-desktop` — UI, MLS, and P2P all run in one process, with no separate services and no IPC for those subsystems. File sync is the one declared exception: its always-resident engines run in the companion `fauna-sync-agent` sidecar the app provisions at post-auth (§ File Sync), so the app itself hosts only one-shot engine uses — the in-app segment-backup *upload* coordinator that briefly ran alongside them is now **deleted** (the 2026-07-29 slice-5 flip); that work moved nest-side (§ File Sync).

- **UI:** GTK4 with libadwaita for GNOME-style UI
- **Async backend:** Tokio multi-threaded runtime for the in-process work (UI-driving nest calls, MLS operations, P2P)
- **In-process everything except sync's resident engines:** every other crate links directly — no FFI bridges, no socket IPC

---

## Key Dependencies

```toml
gtk4 = { version = "0.9", features = ["v4_12"] }
libadwaita = { version = "0.7", features = ["v1_5"] }
tokio = { features = ["full"] }
notify = "8"            # inotify-based file watching
secret-service = ...    # freedesktop credential storage (v4, rt-tokio-crypto-rust)
ksni = ...              # D-Bus StatusNotifierItem (system tray); vendored+patched fork at libs/ksni (see § System Tray)
notify-rust = ...       # freedesktop desktop notifications
c2pa = ...              # C2PA manifest reading (unconditional dependency, not a feature)
```

Internal crates linked as Cargo dependencies (no FFI):

- `fauna-core` — actor identity, keypair, token signing
- `fauna-client-core` — shared client logic (feeds, inbox, contacts, groups, bridges)
- `fauna-mls` (native feature) — MLS engine with SQLite persistence
- `fauna-peer` / `fauna-transport` / `fauna-iroh` — P2P seam + node hosting

Cargo features: `default = ["store-safe", "payments", "zaps", "p2p-share"]`
(flavor split adopted 2026-08-14; `zaps` and `p2p-share` joined the registry as
their own charter members afterward — `p2p-share` 2026-08-20, `zaps`
2026-08-28). `store-safe` is the complement of every gated-feature-registry
member — every surface a store-safe build still ships. Until 2026-10-02 that
included `onnx` (the neural spam classifier: model auto-downloaded on login via
`FaunaClient::load_onnx_model()`; a Settings → Privacy "Refresh Model" button),
which made linux's complement non-empty, unlike tui's, because `onnx` was this
crate's whole `default` before the split; the scorer is retired
(`content-scoring.md` § The placement matrix → *Deployment-wide content models
at the client position*), the feature, the loader and the Privacy group are
removed, and the complement is now empty (`store-safe = []`) like tui's. `payments` is the money-plane
surface (profile Tiers §§4-5 provider/claim UI, Settings → Subscriptions claim
redemption, the feed tip surface); `zaps` is a subset of `payments` (the
NIP-57 Lightning zap-signer trust root on the Nostr page); `p2p-share` is the
co-present offline share-initiation ceremony (`crate::offline_share` + the
Folders page's `offline-{share,receive}-*` elements). `--no-default-features
--features store-safe` drops all three (`just linux-store-safe` /
`just linux-store-safe-check`). The flavor-split mechanism, the full registry,
and each member's rationale are owned by
[`../dynamic-features.md`](../dynamic-features.md) § Platform-family surface
excision / § Charter members, not restated here. Two opt-in test-only features
round out the set: `live-secret-service` (runs credential tests against a real
Secret Service) and `e2e-agent` (§ E2E automation, § Credential Storage).

---

## Message Flow: GTK Main Thread ↔ Tokio

GTK requires all widget mutations on its main thread. Tokio tasks run on a separate thread pool. The bridge between them is an mpsc channel carrying `UiMessage` values.

```
Tokio task
  └─ send UiMessage → mpsc::Sender

GTK main thread
  └─ glib::timeout_add_local (50ms poll)
       └─ recv UiMessage → update widgets
```

`UiMessage` variants:

| Variant | When used |
|---------|-----------|
| `Data(DataMessage)` | Feed posts, inbox messages, contact lists, etc. |
| `Action(ActionResult)` | Response to a user-initiated action (send, upload, etc.) |
| `Realtime(WsEvent)` | Typed push event (new message, knock, etc.) |
| `Noop` | Heartbeat / keep-alive (no widget update needed) |

Views pattern-match on the message type and update their widgets accordingly.

**Snapshot observers — the push direction.** A shared-Rust manager or machine (`FeedManager`, `ConversationsManager`, the search manager, the devices/backups/media/onboarding machines, the critical-alert registry, …) announces a change through its observer trait on whichever thread mutated it; the page repaints from a fresh snapshot on the GTK main thread. Every such page consumes its observer through one shape, `async_helper::snapshot_wake_channel` + `async_helper::spawn_wake_loop`, and that shape carries two rules. **Wakes coalesce:** the channel holds one pending wake, because a repaint re-reads the whole snapshot, so any number of notifications before it runs owe one repaint. **Every repaint yields:** after each repaint the loop hands the thread back to the main loop before taking the next wake, so input, the frame clock and the e2e agent's op drain and heartbeat all run between two repaints. Without coalescing and the yield, a feed page whose cards each resolve an embed was rebuilt once per notification inside a single main-loop dispatch — the UI thread stayed unresponsive for 25 s and more in the 2026-09-15b whole-suite sweep (e2e-conventions.md convention 11). Pinned by `async_helper::wake_loop_tests`. **A repaint rebuilds widgets, not reads:** coalescing bounds how often a page repaints, not what one repaint costs, so a card must not re-fetch on every build what it already painted — every image a rebuilt card paints from a fetch — the feed's post images and `c2pa-badge` verdicts, and on the feed and conversations pages alike the link-preview og:image and the revealed remote image — comes from a per-key cache the card consults (`media_loads`; owned by `../../ui/media.md` § Persistence and its § Encryption at rest linux bullet), and a repaint of an already-painted card issues no blob read, contacts no image host and decodes nothing.

**The repaint loop stays at DEFAULT priority, and idle priority is refuted by measurement.** Yielding did not end the starvation one level down: the first sweep with it (2026-09-19) still lost every idle callback — the e2e agent acks `barrier` from one — and every paint of the frame clock, which is what maps a freshly presented dialog. Moving the loop to `DEFAULT_IDLE`, so it could never outrank them, made the same sweep worse (81 failures against 48) and produced repaints that never happened at all. Both directions failing located the fault elsewhere: something held DEFAULT continuously. Measured on 2026-09-21 by the main-loop meter (below), it was not this section's UI pump, the suspect of the day, but the e2e agent's own 50 ms state publish: the shared conversations serializer deep-cloned every message of every thread on each tick, so from the first ~3 MiB inbound mail onward every tick cost ~230 ms and was always due again — the above-redraw probe starved for up to 84 s while the heartbeat kept beating. The serializer now reads threads without cloning a message (the cost rule is `e2e-conventions.md` convention 11's second corollary). A second, shorter holder measured in the same sweep — connection-state messages re-deciding the offline-gate declarations of every window the process had built, 2 s each, pump ticks of up to 13 s — ended when an actor change began retiring the outgoing window's declarations ([`account-offline-mutation.md`](../account-offline-mutation.md) § Implementation status today → *Built — the linux leg*). A repaint belongs level with the rest of DEFAULT work, never beneath it.

**The main-loop meter is how a starvation names its cause** (`apps/fauna-linux/src/main_loop_meter.rs`, started beside the e2e heartbeat, e2e launches only). Every main-loop dispatch site that matters books its exclusive time under its own source — the UI pump with a per-message-kind tally, the agent's command drain, state publish, barrier ack and element-op drain, each snapshot wake loop and render by call site, the frame clock's paint cycles; a wrapped poll function yields the thread's busy time, so what no meter covers shows up as the unmetered remainder; and two probes, one at `DEFAULT_IDLE` (the barrier ack's priority) and one just above the frame clock, say which band is starved. A 5 s window that was mostly busy, or whose idle probe waited, logs one `[main-loop]` line to `app.err`, sources ranked by busy time. The heartbeat ([`e2e-conventions.md`](../e2e-conventions.md) convention 11) says the thread is running; the meter says what it is running.

---

## MLS Integration

MLS runs directly in-process — no WASM, no UniFFI, no socket. Linux is the one
app that links `fauna-mls` as a plain Cargo dependency (the others cross an
FFI/WASM boundary — platform table: [`common.md`](common.md) § MLS).

- `MlsManager` wraps `MlsEngine` with a `Mutex` for safe concurrent access
- State persisted in SQLite via the `native` feature of `fauna-mls`
- Key operations: `create_dm_channel()`, `encrypt_text()`, `decrypt_message()`, key package generation

---

## P2P

Linux hosts an in-process **iroh `PeerNode`** over the substrate-agnostic
transport seam (`apps/fauna-linux/src/p2p.rs` — `IrohTransport` + `PeerNode`;
there is no pairing UI, per p2p.md § No pairing step, ever). The P2P layer's
architecture, status, and the dormant-data-plane reality are owned by
[`../../behavior/p2p.md`](../../behavior/p2p.md) — do not re-derive them here.

The linux surface is the Settings → P2P tab (`src/settings/p2p_tab.rs`), and
since 2026-08-23 it is the **only P2P page on any app**: windows' and tui's were
WireGuard registration forms and went with the stack. linux's page never was —
it drives a local node — so it survived the deletion with two renamed ids
(`p2p-tunnel-toggle`, `p2p-node-id-copy-btn`) and three rows removed (the WG
key, the STUN endpoint, the listen port).

---

## File Sync

**The always-resident engines run in the per-user sync+backup agent** (`bins/fauna-sync-agent` — a
systemd *user* unit the app enables at post-auth), so sync and backup run with the app closed; the
app provisions the agent (shared convergence loop over the `fauna-ipc` unix socket) and keeps only
one-shot engine uses in-process. **Ratified 2026-07-18, cut over 2026-07-19 (milestone A3).** Owner:
[`sync-agent.md`](sync-agent.md) § Implementation status today. The on-demand surface — a FUSE root
the same agent mounts over a bound location, with the windows-shaped mode toggle on the Folders page —
is ratified 2026-09-26 and built, the agent's root and the linux app's switch both (the owner carries the status); owner [`../../behavior/on-demand-files.md`](../../behavior/on-demand-files.md) § Linux FUSE binding.

The engine those in-process one-shots drive is the **shared `fauna-sync-engine` crate**
(`libs/fauna-sync-engine`), linked as an ordinary Cargo dependency and driven from the tokio
runtime — consistent with the monolith (no sidecar, no IPC). It is the *same* conflict-aware engine the per-user
sync agent runs (watcher via inotify/`notify` v8 → chunk pipeline → 3-way merge → conflict
detection that reports to the nest via `fauna.sync.conflicts.report` → SQLite state DB), so Linux gets
conflict-aware sync rather than a private reimplementation. See `docs/goal/behavior/sync-engine-deployments.md`
§ Control Plane Principle for the engine-vs-deployment model and
`docs/goal/behavior/file-sync.md` § Conflicts for the conflict flow.

**Control plane.** Folder / device / status / conflict management routes through the nest (rule 9),
via the shared folder creation wizard (`fauna-folders-machine`) and the
`fauna-client-folders` / `fauna-client-sync` / `fauna-client-snapshots` adapters — not through any
app↔daemon side channel. The local folder ↔ folder mapping is **device-local** config (the
per-device equivalent of the daemon's `watch_dir`), which is not a control-plane concern.

### Implementation status today

**Superseded by the A3 cutover (ratified 2026-07-18, cut over 2026-07-19) — the in-process multiplexer
described in earlier revisions of this section is retired.** `crate::sync::SyncDriver` and the
`fauna_sync_engine::engine_host::EngineHost` multiplexer it drove no longer run inside `fauna-desktop`;
`apps/fauna-linux/src/sync.rs` today holds only device-local state-dir layout (`sync_state_dir`,
`backup_state_dir`, the stable `device_id()` read); its migration-seed read of the legacy location map
(`location-map.json`) was retired 2026-09-24 with the name-keyed binding — the agent's own config is the one record of this device's bindings. The always-resident watcher
engines instead run inside the external per-user `fauna-sync-agent` process; owner:
[`sync-agent.md`](sync-agent.md) § Implementation status today.

`apps/fauna-linux/src/sync_agent.rs` is the app's control surface for that agent, installed at
post-auth (`sync_agent::install`): it (1) ensures the agent runs — production via a self-installed
systemd *user* unit (`SystemdAgentSpawner`), e2e via a direct-spawned child inheriting the launch's
isolated XDG world; (2) runs the shared provisioning convergence loop
(`fauna_client_sync::agent::SyncAgentProvisioner`, linked directly — no FFI hop) which mints and renews
the agent's `RenewBearer` device grant; (3) reroutes the location-binding UI
(`bind_folder`/`unbind_folder`/`list_folders`) over the agent's unix socket through the shared
`LocationBindingsModel` — optimistic rows, reconciled against agent truth on every reachable edge, not
just once at attach; and (4) subscribes to the agent's pushed events
(`fauna_ipc::events::spawn_event_listener`) to re-surface per-file completed syncs as a desktop
notification — the in-process consumer this replaced. `sync_agent::teardown` runs on sign-out /
account-switch / factory-reset / e2e-reset (unprovisions + stops the agent's engines for this account;
`account_runtime::teardown` awaits the reply on its spawned stop, never on the GTK thread — the erase
waits per [`account-scoping.md`](account-scoping.md) § Erasure follows scope);
a plain app quit does not — always-running sync while the app is closed is the point of the agent.

Device-local config keyed on this device: the per-account sync-state dir
`~/.config/fauna/sync/<actor-id-hex>/` ([`account-scoping-dispositions.md`](account-scoping-dispositions.md) § Serialized switching) holding
per-folder state DBs and the shared `device.db`. The Folders settings page shows the agent's
reported status + this device's location bindings; the nest-authoritative folder list / backup status
live on their owning pages. **The in-app segment-backup *upload* coordinator (`crate::backup`) is
deleted** — `apps/fauna-linux/src/backup.rs` and all its wiring (the `mod` decl, the `backup::shutdown()`
calls, the login-time `start_from_config`, the post-destination-mutation `rebuild`) came out at the
2026-07-29 slice-5 flip. Linux no longer schedules or performs a segment-backup upload at all, and its
Backups page renders per-destination status purely from the nest projection; the source nest's own
`NestBackupWorker` does the upload instead, unconditionally, with every app and agent asleep. Linux was
the first app to drop its in-app driver this way (2026-07-29); the flip's cross-app status (now complete
on all seven) is owned by [`../../behavior/backup-restore.md`](../../behavior/backup-restore.md)
§ Background Tasks — not restated here.

---

## System Integration

### System Tray

D-Bus `StatusNotifierItem` via the `ksni` crate. Supports close-to-tray: closing the window hides it rather than quitting. The tray icon provides a menu to show/hide the window and quit. (The cross-app promise this mechanism serves — closing the window is never a silent stop — is owned by [`common.md`](common.md) § Desktop Residency since 2026-08-30; this section keeps linux's mechanism.)

**Close-to-tray defaults ON, and is a persisted user choice (shape A, adopted 2026-07-16).** At adoption, quitting the app (rather than hiding it) would have silently stopped the always-on backup *upload* coordinator (`crate::backup`) — and, before the A3 sync-agent cutover, file sync too — exactly the tension [`windows.md`](windows.md) § App Lifecycle → *Window close* resolved by making the app always-present; Linux adopted that resolution rather than diverging. **Neither in-app engine remains today**: file sync moved to the `fauna-sync-agent` sidecar at A3 (2026-07-19), and the segment-backup upload coordinator was deleted outright at the 2026-07-29 slice-5 flip (§ File Sync) — its work moved nest-side. The default stays ON regardless, for presence/UX parity with windows (the same narrowed rationale as autostart, below) rather than because quitting still stops a live in-app engine; a deliberate stop is the tray's **Quit**, and an explicit opt-out is never overridden. The choice lives in a device-local JSON store at `$XDG_CONFIG_HOME/fauna/app-settings.json` (the linux twin of windows' `AppSettingsStore`), read at startup into `tray::CLOSE_TO_TRAY` and written only by a genuine user toggle — a key *absent* from the file takes the ON default while a key present as `false` wins over it, which is what lets the default be ON without clobbering a user who already turned it off. It is deliberately **not** nest state: it is a per-device choice that means nothing on another machine, so it never round-trips through the account's synced account-state plane. (It is still a app-UI choice, not a hand-edited knob — [`principles.md`](../../principles.md).)

**The tray-host gate bounds what the default can deliver on Linux.** Because hiding stays gated on a live tray host (next paragraph), close-to-tray default-ON is a *no-op on a stock GNOME with no tray host* — there, `X` quits regardless, and residency across a window close is simply not available. This is a real platform limit, not a policy divergence: the setting's default matches windows, but the guarantee it buys is conditional on a tray host existing. Delivering presence on a stock GNOME desktop is therefore auto-start's job, not close-to-tray's.

**Close-to-tray is gated on an actual tray host (out-of-the-box safety).** Hiding the window is only honoured when a `org.kde.StatusNotifierWatcher` is present to restore it from — otherwise hiding would strand the window with no affordance to bring it back (stock GNOME ships no tray host). Host availability is tracked dynamically via the vendored `ksni` `watcher_online` / `watcher_offine` hooks into `tray::TRAY_HOST_AVAILABLE` (a host can appear/disappear at runtime, e.g. enabling/disabling the appindicator extension), defaulting to *unavailable* so a window is never hidden before a host is confirmed. Every hide path — the `X` button (`connect_close_request`), Ctrl+W, Escape — routes through `tray::should_hide_to_tray()` (`CLOSE_TO_TRAY && TRAY_HOST_AVAILABLE`); when it returns false, `X` quits the app instead (matching close-to-tray-off and Ctrl+Q). The General preferences row is rendered insensitive with an explanatory subtitle ("No system tray detected — closing the window will quit Fauna") when no host is present; because the settings stack builds each page once, `build_general_page` returns a refresh closure the shell re-runs when the General page becomes visible (`connect_visible_child_name_notify`), so the row greys/un-greys to match a host that appeared or disappeared since the page was built. This is a Linux-specific guard: macOS (`NSStatusBar`) and Windows always have a tray host, so the trap cannot arise there and there is no cross-app guard to mirror.

The guard is covered end-to-end by `tests/e2e-unified/tests/test_tray_close_to_tray.py` (tier_3, linux): a private D-Bus session bus (`helpers/tray_bus.py`, run with **no service activation** so the app's startup libsecret/a11y/portal lookups fast-fail with `ServiceUnknown` instead of hanging on an auto-activated locked keyring) makes the host-present/absent condition deterministic regardless of the box's real GNOME state. No watcher → the toggle is greyed and `X` quits; a `jeepney`-based fake `StatusNotifierWatcher` (`helpers/fake_status_notifier_watcher.py`) → the toggle enables and `X` hides; stopping the fake watcher mid-test drives the runtime `watcher_offine` re-greying + quit-on-close. The titlebar close is driven by the `/window/close` automation verb, which runs the real `connect_close_request` handler. This replaced a manual hand-check (disabling the GNOME appindicator extension is session-bus-wide and would disturb sibling app).

**Restoring from the tray on Wayland needs an activation token.** mutter (GNOME Shell) refuses to raise a window unless the activation request carries an xdg-activation token minted from a real input event — and a tray click lands on the shell's surface, not ours, so only the tray host can mint one. The host hands it to us via the freedesktop SNI method `org.kde.StatusNotifierItem.ProvideXdgActivationToken(token)`, called just before the "Open Fauna" activation. Upstream `ksni` (incl. 0.3.x) does not implement that method, so we use a **vendored, locally-patched fork at `libs/ksni`** that does (see `libs/ksni/PATCH.md`); the app stashes the token (`tray.rs`) and feeds it to `gtk::Window::set_startup_id` before `present()` (`show_window` in `main.rs`). Without it the raise is denied and the user gets only a passive "Fauna is ready" notification. Requires a tray host that implements `ProvideXdgActivationToken` (GNOME's `ubuntu-appindicators` / `appindicatorsupport` extension does); on a stock GNOME with no tray host the tray icon is absent — and close-to-tray is then disabled and falls back to quit-on-close (see the close-to-tray gate above) so the window is never stranded.

### Home-screen widget

**The surface (design pass 2026-09-26): the launcher badge.** Linux has no one widget host — GNOME Shell offers third-party apps none without a separately installed extension, KDE's plasmoids are a per-desktop package no GTK app ships out of the box — so the glanceable unread count goes where the most stock desktops already look for one: the **`com.canonical.Unity.LauncherEntry` count badge on the launcher icon**, a session-bus broadcast (`Update(s app_uri, a{sv} props)` with `count`/`count-visible`) that Ubuntu's dock (the stock Ubuntu GNOME session), KDE Plasma's task manager, elementary's dock and Dash-to-Dock paint as a number on the app's icon, with no host to discover: a desktop with nothing listening shows nothing, and nothing is stranded. `apps/fauna-linux/src/launcher_badge.rs` emits it; the `app_uri` is `application://<desktop id>` where the desktop id is the installed entry's basename — `social.fauna.fauna.desktop` on every native channel and under Flatpak (`FLATPAK_ID`, read rather than assumed), `<instance>_fauna.desktop` under Snap (snapd rewrites every snap's entries) — and the object path is libunity's `/com/canonical/unity/launcherentry/<digits>`, the shape snapd's `unity7` interface admits. The sandbox grants (`--talk-name=com.canonical.Unity` in the Flatpak manifest, the `unity7` plug in the snap) are owned by [`../installers/linux-desktop.md`](../installers/linux-desktop.md) § Flatpak / § Snap.

**The number is the tray's number, and the tray corroborates it.** The badge and the tray tooltip are fed by one computation — `conversations::state::sum_unread` over the shared `fauna_conversations` snapshot, the per-thread `unread_count` the conversations list renders, summed, once per snapshot tick in `main.rs`'s tray-toast loop — so the widget can never show a count the app would not; what "unread" means is owned by [`../../ui/conversations.md`](../../ui/conversations.md) § State & data shape → *When a thread is read*, and the cross-app promise by [`common.md`](common.md) § Home-screen widget. Where a tray host exists (§ System Tray) the tray shows the same count in its tooltip and switches to the attention icon, but the tray alone does not meet the promise: the SNI protocol has no count field, so the number is only on hover — it corroborates the badge, it is not the widget.

**The per-desktop limit, stated (the tray-host gate's shape, not a per-app absence).** An upstream stock GNOME session — no dock extension, no tray host; the Flatpak runtime's nominal desktop — paints neither surface, so there the count is not glanceable at all and only the desktop notifications (§ Desktop Notifications) remain. This is a real platform limit of that one desktop, exactly as close-to-tray is a no-op there (§ System Tray → *The tray-host gate*), and it is recorded here rather than as a catalog absence: the linux app has the behaviour, and the [`home-screen-widget`](../../../features/home-screen-widget.md) column measures the app, not each desktop it may land on ([`../feature-catalog.md`](../feature-catalog.md) § Closing a gap — an `absences` entry is a per-app deviation). Ubuntu's stock session, the snap's home and the commonest Linux desktop, paints both.

**Background currency — how "without the app being opened" holds on linux.** The badge lives exactly as long as the process: a dock clears it when the app exits, and no linux desktop runs app code on the app's behalf. So the promise's second half rests on residency, which linux already owes for presence: auto-start registers at the first successful sign-in and defaults ON (§ Auto-start at sign-in), so Fauna is resident from every later desktop sign-in with its WS-RPC subscription live, and the snapshot tick that feeds the badge runs whether the window is mapped or hidden; where a tray host exists, close-to-tray keeps the process (and the badge) across a window close. Where none exists, `X` quits and the badge goes with it until the next sign-in or launch — the same conditional the tray-host gate already states. The witness drives this leg with the fake watcher present: close hides, a planted message moves the badge while nothing is on screen.

**Witness.** `tests/e2e-unified/tests/test_home_screen_widget_launcher_badge.py::test_launcher_badge_shows_the_unread_count_and_moves_while_hidden` (tier_3, marked `linux`): a subscriber on the app's private session bus (`helpers/launcher_entry.py` — on a bus whose policy allows eavesdropping, the listener *is* the dock) reads the `app_uri` and `count` a dock would paint, checks the count against the app's own thread list, then closes the window (hidden, tray host present) and reads the badge move on a second planted message. Cited on the catalog page's outcomes 1 and 2 for the linux column. What no test reads is the last inch — whether a real dock paints it — which is a human's glance at a badge on an Ubuntu dock, not a mechanism.

**Implementation status today (2026-09-26): BUILT** — `launcher_badge.rs` + the `main.rs` feed, both sandbox grants, the witness above. Unverified by construction on this box: a real dock's paint (no dock reads the harness's private bus), and the snap channel's `unity7` leg (snapcraft is not on any dev machine — § Snap's standing stance).

### Desktop Notifications

Freedesktop notifications via `notify-rust`. Notifications are suppressed when the main window has focus to avoid double-alerting the user. With the app closed, the per-user sync agent posts the push banner instead — the `ws-device` transport ([`common.md`](common.md) § Push Notifications → *Transports*, ruled 2026-09-26, built 2026-10-08): the agent posts only while no app is attached over the IPC seam, so this focus rule and the agent's arm never both fire — the app holds its attachment lease from the post-auth `sync_agent::install`, once per process. Per-file completed-sync notifications are pushed from the per-user sync agent's event socket, not generated in-process — the agent and its IPC event contract are owned by [`sync-agent.md`](sync-agent.md) § Implementation status (A3 remainder).

### Update check

A notify-only check: `apps/fauna-linux/src/updater.rs` reads the shared `fauna_core::version::latest_release_api_url` (the GitHub Releases API for `RELEASE_REPO`) and folds the answer through the shared `newer_release_from_latest_json` (the `is_newer` semver rule), so every checker in the fleet agrees on what counts as newer and where to look — the linux module is the HTTP round trip and the desktop notification, nothing more. It follows the one rule every desktop follows (amended 2026-10-03): on demand from Settings → General (`settings/general.rs`, through the shared `fauna_client::update_look::check_for_newer_release_over_http`, whose three answers the button's label carries — the newer release, "Up to date", or "Check failed" when the feed could not be read, never "Up to date" for a failure), plus ONE unasked look per sign-in (`app.rs` → `client.rs::check_for_updates`, through the shared `fauna_client::update_look::look_at_sign_in_over_http` — the entry point tui calls too, so no app keeps its own), which raises the same notice when a newer release is out and stays silent otherwise, a failed look included; no timer, no download, no self-update, no toggle (that loop is `fauna-update`'s, for the nest and the sync binaries). Both paint one notice on Settings → General's About group, `update-available-notice` (the version and the release page, the shared `update_available_notice` words; `settings/general.rs::show_update_notice`), beside `settings-app-version` and `settings-check-updates-button`; the sign-in look also raises a toast. Both reach the feed through `updater.rs::feed_origin`, which is `GITHUB_API_ORIGIN` save under e2e automation in a test-capable build, where `FAUNA_E2E_RELEASE_FEED_URL` names the harness's stub (tui's seam). The cross-platform promise this serves, and the desktop-only scope that makes it owed here and absent on web and the phones, is owned by [`../installers/README.md`](../installers/README.md) § Knowing a newer version is out.

### Credential Storage

Freedesktop Secret Service via the `secret-service` crate (v4) — integrates
with GNOME Keyring and KWallet. The Ed25519 secret key lives there in
production; the cross-app contract is owned by [`common.md`](common.md)
§ Credential storage. One **declared, test-build-only exception**:
`FAUNA_E2E_CREDENTIAL_DIR` routes the identity trio to a 0600 JSON file — but
only inside a test-capable build (`cfg(any(test, debug_assertions, feature =
"e2e-agent"))`, the shared `fauna-credential-store` crate's gate;
[`e2e-automation-surface-gating.md`](../e2e-automation-surface-gating.md) § point 15). A genuine
`--release` build with no `e2e-agent` feature ignores the env var entirely and
always resolves the keyring — **linux has no headless arm** (unlike tui's
passphrase-sealed store), so a keyring failure in a shipped build is a loud
error, not a silent fallback to a file store. (The gate on that env var was
added 2026-08-11; it has held since.) `FAUNA_KEYRING_APP` namespaces keyring
entries per instance (`apps/fauna-linux/src/client.rs`), through the same shared-crate
gate (dispositioned 2026-08-15 — it was the family's last ungated read;
`e2e-automation-surface-gating.md` § Implementation status today).
The keyring is a file under the home directory, so the person's own backup
of it carries the secret, sealed as their keyring is; that is one case of
the cross-app rule in `common.md` § Credential storage → *What a device's
own backup carries*, which owns the statement.

### Desktop Entry

- `packaging/social.fauna.fauna.desktop` — the one XDG desktop entry every channel installs, named after `APP_ID` (`packaging_identity_test.rs` pins the spelling; `../installers/linux-desktop.md` § Flatpak owns the grants derived from it). The launcher badge keys on this basename (§ Home-screen widget).
- `packaging/social.fauna.fauna.metainfo.xml` — the one AppStream metainfo for GNOME Software and other app stores.

### Auto-start at sign-in

**Target state (shape A, adopted 2026-07-16 alongside the close-to-tray default).** Fauna registers itself to start at desktop sign-in **by default** — writing `~/.config/autostart/fauna.desktop` at the universal post-auth hook (`launch_authenticated`), so after the *first* successful login the app is present at every subsequent sign-in with no manual step. That is the only mechanism that delivers presence on a stock GNOME desktop, where the tray-host gate makes close-to-tray a no-op (§ System Tray). *(Since the per-user sync agent's A3 cutover — [`sync-agent.md`](sync-agent.md), 2026-07-19 — file sync no longer depends on app presence; since the 2026-07-29 slice-5 flip deleted the in-app segment-backup upload coordinator outright (§ File Sync), backup no longer does either — autostart's rationale is now fully narrowed to notifications, provisioning immediacy, and UX presence, with no in-app engine left whose liveness depends on the app ever having been opened.)* Rationale and the cross-app intent: [`windows.md`](windows.md) § App Lifecycle → *Auto-start at sign-in*.

Three properties are load-bearing, mirroring the windows leg:

- **The choice is tri-state, not a file-existence check.** `autostart_choice` in the same `app-settings.json` store: unset ⇒ register by default; explicitly `false` ⇒ never re-register; explicitly `true` ⇒ register. "The `.desktop` file is absent" must **not** be read as the choice, because it cannot distinguish *never chosen* from *deliberately turned off* — and re-registering over a deliberate opt-out at the next login would violate the app-is-the-only-config-surface contract ([`principles.md`](../../principles.md)). Re-writing the entry on each login self-heals a moved/upgraded install.
- **Registration is gated off under e2e** (`crate::e2e_mode_enabled()`), so harness runs never write the real user's autostart directory.
- **The autostart launch is tray-resident (hidden)** — `Exec=fauna --autostart` — but *conditionally*: with no tray host there is nothing to restore a hidden window from, so a hidden start would be unreachable. Linux therefore starts **visible** unless a tray host is confirmed. ksni answers this at tray startup (it calls `watcher_online` on a successful `RegisterStatusNotifierItem` and `watcher_offine` on `ServiceUnknown`), so the answer arrives asynchronously and the presentation decision must wait for it rather than read the default-`false` `TRAY_HOST_AVAILABLE`. As on windows, an `--autostart` launch that routes to *onboarding* shows the window regardless — a signed-out auto-start must be loud, not a silently dead agent.

**Implementation status today (2026-07-16): IMPLEMENTED — all three properties above ship.** `src/autostart.rs` holds the two decisions as pure functions (`should_register`, mirroring windows' `AutoStartGate` truth table exactly; `should_start_hidden`) plus the `.desktop` mechanics; `autostart_choice: Option<bool>` lives in the `app-settings.json` store beside `close_to_tray`; `register_at_post_auth()` runs inside `launch_authenticated`, which every authenticated launch funnels through (first login, returning-user boot, account switch). `settings-autostart-toggle` now renders the *choice* rather than the `.desktop` file's existence, signal-blocked around its programmatic update so a page build cannot record a fake explicit choice. ui.yaml scopes the toggle via `settings.platform_elements` (linux+windows+macos).

The hidden-launch decision waits for ksni's answer on a bounded glib poll (`show_main_window_unless_autostart_hidden`, 5s) rather than reading the default-`false` `TRAY_HOST_AVAILABLE`, and **fails safe to visible in both directions**: answered-no shows immediately, and an answer that never arrives shows at the timeout. That last case is real, not defensive padding — ksni calls the hooks only on `Ok` and on `ServiceUnknown`, so any *other* D-Bus error answers neither way; `tray::TRAY_HOST_ANSWERED` (set by both hooks) is what separates "no host" from "not heard yet". Only the `LaunchPhase::Online` boot site consults it: the onboarding routes call `show_window` unconditionally, so a signed-out auto-start is loud, and the account-switch/relaunch sites always present because the user is already driving that session.

⚠ **`--autostart` must never reach GTK.** `GApplication` parses argv itself (the app registers no main options and does not set `HANDLES_COMMAND_LINE`), so an unrecognised option makes it print `Unknown option --autostart` and refuse to start — measured against the real binary, 2026-07-16. Because the registered entry execs exactly that, an unfiltered argv breaks **every** sign-in, which is why `main` hands GTK `autostart::strip_autostart_flag(env::args())` rather than calling `run()`. `is_autostart_launch()` reads the real process argv, so the flag still does its job. A future `main.rs` rework that restores a bare `application.run()` silently re-breaks the feature; `the_autostart_flag_never_reaches_gtk` pins the filter.

**Not covered by an e2e test, by construction — but the mechanism is pinned anyway.** Registration is gated off under `e2e_mode_enabled()`, so no harness run can observe the hook writing an entry. The unit tests in `autostart.rs` + `app_settings.rs` therefore cover not just the pure decisions but the *composition*: `apply_registration_at` takes the path, the e2e flag, the choice and the exe path as arguments, so "decision → entry on disk" is asserted without a desktop session — including the load-bearing property that **an explicit opt-out is never re-registered**, the stale-`Exec` self-heal, e2e writing nothing, and the hook's idempotence across the many logins that run it. What genuinely remains for a live pass is only the last inch: on a real desktop session, sign out → sign in → the app is present and resident (§ Auto-start at sign-in's success condition). That is an appearance/integration check, not an untested mechanism.

### Multi-Account Instance Handling

The multi-account capability stages (serialized switching → concurrent instances → concurrent
identities), the shared `AccountInstanceLock` mechanism, and the per-app implementation status are
owned by [`account-scoping.md`](account-scoping.md) — not restated here. What follows is only *where*
those mechanics attach to the Linux launch path, since linux is a reference leg for this work.

Linux re-keys its existing single-instance affordance — GApplication's D-Bus bus-name uniqueness —
from "per OS login" to "per (OS login, account)" rather than replacing it: an ordinary unbound launch
still gets GNOME's usual raise-the-existing-window behavior, while a launch that carries a
`FAUNA_BOUND_ACCOUNT` binding, or that detects it is colliding with an already-served account, opts
out of that uniqueness (`gio::ApplicationFlags::NON_UNIQUE`, `main.rs`) so it can run as its own
process alongside the primary. The collision *check* (`main.rs::launch_collision_detected`) runs
**before** `adw::Application` is built — the one moment it can, since GApplication's uniqueness would
otherwise silently redirect a colliding plain launch into the running instance first and exit it before
any of this process's code runs; a positive result is what decides the `NON_UNIQUE` opt-out above. The
chooser itself (`main.rs::show_launch_instance_chooser` → `views::launch_instance_chooser`) renders
later, from `build_ui` on the `activate` signal: a colliding process renders the
`launch_instance_chooser` page there rather than the ordinary window. The per-(OS login, account) raise
channel — restoring a window when the collision target is itself a bound (non-unique) sibling — is a
D-Bus well-known name `social.fauna.fauna.a<token>` claimed alongside the instance-lock acquire
(`account_scope::become_session_instance`). Account-scoped app state (MLS and the two P2P DBs; the parked WireGuard key
went 2026-08-23 with the stack) resolves through `account_scope::resolve_and_adopt` (`apps/fauna-linux/src/account_scope.rs`),
which also holds the cross-process account-registry mutation lock's install-scoped base.

---

## Local Database

None of its own. The app keeps no local content database: per-account state
lives in the shared stores (the MLS engine's SQLite via `fauna-mls`, the sync
engine's state DBs, the account registry), and the views' row models
(`src/rows.rs`) are in-memory shapes the client layer maps nest replies onto.
A desktop SQLite cache (`db.rs` + a `schema_v1`–`v4` `user_version` chain) once
sat here with no production caller; it was removed 2026-09-27 in the
compat-remnant sweep ([`../version-compatibility.md`](../version-compatibility.md)
§ Dimension 2).

---

## View Architecture

No formal MVVM pattern. State lives either in GTK widget properties or in
`Rc<RefCell<T>>` held by the view struct. Views are organized **one module per
feature under `src/views/`** (conversations, contacts, feed, events, media,
backups, bridges, devices_folders, profile, personalization, moderation,
search, status, notifications, onboarding, launch, admin, settings_shell +
`src/settings/` sub-modules, …) — see the directory; an enumerated tree here
re-rots on every feature landing. CSS customization is applied at startup from
`style.css`, layered on top of the libadwaita base theme.

---

## E2E automation (in-process agent)

Widgets that need to be found by E2E tests use `crate::testid::set_test_id(&widget, "id")`, which sets the GTK widget name (`set_widget_name`, always, ungated) — the carrier the agent finds by — and, only in a test-capable build (`cfg(any(debug_assertions, feature = "e2e-agent"))`, mirroring `mod test_agent`'s gate), also stamps an accessible description/tooltip as a discovery fallback. A **release** build sets neither: a kebab-case internal ID is not real screen-reader a11y (invariant since 2026-08-05, when the accessible-description/tooltip stamp was confined to test-capable builds); a human label per widget would be separate work. The unified `LinuxBridgeDriver` speaks the same `HttpBridgeDriver` `/element/*` contract as every other app; only Linux's *backend* behind that contract differs. Since 2026-07-10 the agent's HTTP front-end (server, parsing, routes, op types) is the shared `libs/fauna-e2e-agent` crate, hosted identically by the tui app (`apps/tui.md` § E2E automation); `apps/fauna-linux/src/automation/server.rs` is thin wiring over it and the GTK widget find/actuate halves stay linux-local.

**A widget's `get_text` answer is inferred from its kind — and a row whose title is its identity must override that inference** with `crate::testid::set_test_text(&widget, text)`, the linux peer of apple's `.automationValue(id, text:)` and of the text tui carries on its `Element`. `automation::find::text_of` checks the declaration first, then falls back to per-kind rules; the one that cannot be inferred is `adw::ActionRow`, which serves two opposite shapes — a **caption/value** row whose value is the *subtitle* (the identity actor-id row, which the subtitle rule exists for), and a **content** row whose identity is the *title*. The second read back as its secondary detail and silently lost its title: a read-only member `folder-row` is `.title(name).subtitle(mode)`, so every such row answered `get_text` with the mode string `"sync"`. It surfaced cross-nest, where it was indistinguishable from the nest having delivered an empty set name and cost a session's hunt through the server-side resolution chain before the read itself was suspected (2026-08-11; the sibling owner/writer rows are `adw::ExpanderRow`s, which kept their titles through the descendant-label join purely by accident of widget kind, which is why no earlier test caught it). Declare the text on any row whose title carries meaning; the tier_1 `a_content_row_reads_its_declared_text_not_its_subtitle` pins both halves. **Swept 2026-08-11:** every `adw::ActionRow` construction site carrying both a `.subtitle()` and its own direct test id was enumerated (`grep`, cross-checked against a full scan for `.subtitle(` + `set_test_id(&<binding>,` on the same row binding) and its e2e consumers checked. Two more content rows needed the declaration — `atproto-app-credential-item` (`settings/atproto.rs`) and `atproto-connected-app-item` (same file) — now fixed the same way as `folder-row`. Every other dynamic-title candidate the sweep found (`logs_view.rs`'s `log-entry`, `mail_aliases.rs`/`mail.rs`/`mail_spam.rs`/`mail_lists.rs`/`mail_list_members.rs`/`mail_export.rs`'s list rows, `account.rs`'s account-switcher row, `admin.rs`'s dashboard user row) turned out **not** exposed: each either carries no direct test id on the row itself (the readable text lives on a dedicated 1px marker child instead, the `value_marker`/`marker` idiom) or isn't an `adw::ActionRow` at all. No known remaining exposure.

The same module's `run_on_gtk_thread` owns the crate's **tier_1** widget tests, which run against a display of their own rather than the developer's — a test that opens a window is a launch, so convention 10's isolation binds it; mechanism and rationale live in [`e2e-launch-isolation.md`](../e2e-launch-isolation.md) § point 10.

**The Linux app drives itself in-process.** When launched with a `FAUNA_E2E_AGENT_PORT`, `fauna-desktop` runs an automation HTTP server (`apps/fauna-linux/src/automation/`) on that per-instance port and serves the whole contract — `/element/{click,type,clear,select,text,visible,count,enabled,attr}` plus the `/app/{commands,state}` state protocol — by **direct GTK widget access** on the main loop: DFS the showing widget tree matching `widget_name()` (pruning filtered `gtk::ListBox` rows and any other non-`is_child_visible` subtree, **and every `gtk::Stack` page the stack is not currently on** — the latter tested against the stack's own `visible_child`, not against child-visibility, because GTK keeps the *outgoing* page child-visible and mapped for the full duration of a transition and both navigation stacks in the window crossfade; so the surface reports the logical current page the instant a navigation is issued, mirroring the old AT-SPI showing-only tree and the visible-view-only surface the other six apps expose), read via downcast, actuate via `set_active`/`activate`/`Editable`/`Range`/`DropDown::set_selected`. There is **no external AT-SPI bridge** (removed); the driver owns the app process and points at its port. Because actuation is `set_active`/`activate` on the real widget, **native `adw::SwitchRow`/`gtk::Switch`/`CheckButton`/`DropDown` controls are actuable** — so on/off and pick-one controls are their proper native widgets, not `gtk::ToggleButton` work-arounds. Per-instance ports give true per-session isolation (no shared a11y bus → no cross-session contention). (Design ratified 2026-05-30; tracked internally.)

**The five actuation routes consult the widget's live sensitivity before driving it** — the rule, its staged-rollout method and the reason a sweep must be permissive are owned by [`e2e-conventions.md`](../e2e-conventions.md) § convention 11 (*an illegal command is the same failure one layer down*), and the decision itself is the shared `fauna_e2e_agent::gate_actuation`, so linux and tui refuse in the same shape. Linux-local: the predicate is plain `find::is_enabled` (GTK's `is_sensitive()`, already ancestor-inclusive, so there is no registration surface and no `folding` analogue to maintain), and `LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT` in `automation/agent.rs` is the staging switch — see that constant's doc comment for what flipping it requires.

Sidebar rows use tab IDs per ui.yaml's `navigation.tabs`. Dynamic widget names (row IDs set to group/conversation/post IDs) still use plain `set_widget_name()`; a scope test-id that needs to coexist with a real id rides `widget_name` while the real id is recovered another way (e.g. `post-card` carries the test-id on `widget_name` and resolves the post by row index).

### Implementation status today

The in-process agent is **built and is the default** (the external AT-SPI bridge and its launch plumbing were removed — tracked internally, P6). `crate::e2e_mode_enabled()` (either `FAUNA_E2E_AGENT_PORT` or the legacy `FAUNA_E2E_BRIDGE`) gates the in-app server and all e2e-mode behavior. The custom slider (`widgets/toggle.rs` + the `.fauna-switch` CSS) is **gone**: every boolean on/off is a native `adw::SwitchRow`/`gtk::Switch`, every checkbox a `gtk::CheckButton`, and small pick-one groups native `gtk::CheckButton` radios. Labelled binary `gtk::ToggleButton`s that match web's `<button>` and feed a `get_text` contract (`admin-dns-domain-mode`, `admin-dns-manage-all-toggle`) and the indexed `gtk::Button` provider rows (`vps/dns-provider-row`, matching web's `<button class:selected>`) are kept as-is. The `gtk::DropDown` conversions are done (the `vps-location-picker` and the three tier pickers are native `gtk::DropDown`s, actuated by the agent's `DropDown::set_selected`); the shared e2e `driver.select` gained an `index=` param, and `actions/admin.py` drives the tier pickers select-with-cycle-fallback so the native apps still on the legacy cycle-button stay green; ui.yaml types those pickers `select`. (Tracked internally — see § P5 execution log.)

Cross-app capability comparisons live in [`common.md`](common.md)'s platform
tables — this doc deliberately carries no per-platform difference matrix.
