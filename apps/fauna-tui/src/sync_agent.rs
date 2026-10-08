//! fauna-tui's control surface for the external per-user **`fauna-sync-agent`**
//! (A6 — `sync-agent.md` § Control plane split + § Credential model; plan
//! D3/A6). The tui twin of `apps/fauna-linux/src/sync_agent.rs`: it links the
//! shared `fauna_client_sync::agent` provisioner directly (no FFI hop — tui and
//! the linux GTK client are the two direct consumers), mints + registers the
//! `RenewBearer` device grant at post-auth, keeps the agent provisioned over the
//! per-user agent endpoint, and re-pushes the per-set content-key blob so a **bound**
//! set seals under its M2 content key rather than the owner `BackupKey` (the
//! post-agent-cutover regression the desktop apps hit — KMH § M2). On a
//! headless SSH-only box tui is the surface that **mints** the capability, since
//! it holds identity via its sealed credential store (`sync-agent.md`
//! § Headless deployment).
//!
//! **Threading — the deliberate divergence from linux.** linux (GTK) holds the
//! surface in a `thread_local! AGENT` and re-drives reconcile via
//! `glib::idle_add_once`. tui is a single-threaded main loop + message passing,
//! so the surface lives on `App` (`SyncAgentState`, mirroring `media`). The
//! folder model sits behind an `Arc<Mutex<..>>` shared with the reconcile task —
//! the idiom every other tui manager uses (an `Arc` held internally + a
//! payload-free `DataMessage` tick), which also keeps the desktop-only
//! `LocationInfo` type out of the cross-platform `DataMessage` enum. The
//! agent-reachable edge posts [`DataMessage::SyncAgentReconcile`]; its handler
//! spawns the list→reconcile→push task, which posts
//! [`DataMessage::SyncAgentChanged`] when the rendered binding set actually
//! moved. This is the faithful translation of linux's `Rc<RefCell>` +
//! `idle_add_once` into tui's `Arc<Mutex>` + channel-tick idiom, NOT the one-shot
//! `LaunchRecoverBoxes` carry-the-rows shape (that is for a single late read; the
//! reconcile is a repeated manager operation).
//!
//! **`#[cfg(any(unix, windows))]`.** The surface runs on every OS fauna-tui
//! runs on (`sync-agent.md` § Consumers, ratified 2026-07-24), because the
//! shared `fauna_client_sync::agent` reaches the agent through one
//! `fauna_ipc::endpoint::AgentEndpoint` — the per-user unix socket on
//! linux/macOS, the per-SID named pipe on windows. The gate remains only to
//! keep wasm (no local agent at all) out. The single per-OS line inside is
//! [`native::platform_spawner`]; the `loginctl enable-linger` offer stays
//! unix-only, since windows has no lingering concept (a logon session always
//! exists for the Run key).
//!
//! **Teardown, not quit.** Sign-out / account-switch / factory-reset unprovision
//! the agent (delete its persisted capability + stop its engines) via
//! [`SyncAgentState::teardown`], wired into `session::sign_out` (the single
//! teardown door all three paths route through). A plain app quit deliberately
//! does NOT come here — always-running sync is the whole point of the agent.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use fauna_client::NestClient;

use crate::app::UiMessage;

/// The device label every tui-registered sync device shows in the nest's device
/// list (the analog of linux's `fauna-linux` / macOS's `fauna-macos`).
#[cfg(any(unix, windows))]
const DEVICE_LABEL: &str = "fauna-tui";

/// One rendered folder↔folder binding row, as the cross-platform Folders
/// page render reads it (the tui twin of linux's `LocationBinding`). A flat,
/// cfg-free projection of the desktop-only
/// [`fauna_client_sync::agent::BindingRow`], so `settings::folders` — which
/// compiles on every platform, wasm included — renders the `folder-location-*`
/// section without ever naming the gated agent types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLocationBinding {
    /// The device-local folder path bound to the set.
    pub path: String,
    /// The set this folder is bound to, keyed by the set **name** — the shared
    /// model's row key ([`fauna_client_sync::agent::LocationBindingsModel::add`] /
    /// `remove_by_set`), the same key linux's `location_binding.rs` uses. The
    /// render filters by it so each expanded `folder-row` shows only its own
    /// set's folders.
    pub folder: String,
    /// Mirrored from [`fauna_client_sync::agent::BindingRow::access_revoked`] on
    /// every reconcile — the D4 park surface (`ui/folders.md:152`,
    /// `file-sync.md` § Multi-writer shared sets): the owner withdrew this
    /// actor's `writer` grant, the nest refused the next mint/record, and the
    /// agent stopped syncing this folder without unbinding it. The Folders
    /// page reads it to decide whether a writer-member row's binding section
    /// shows `folder-access-revoked-warning`.
    pub access_revoked: bool,
    /// Mirrored from [`fauna_client_sync::agent::BindingRow::deletes_held`] on
    /// every status poll — the **mass-delete floor**'s hold for this row's set
    /// (`delete-propagation.md` § A wholesale-vanished folder is infrastructure
    /// failure). Non-zero means every tracked file vanished at once and nothing
    /// was recorded; the Folders page renders `folder-location-deletes-held` and
    /// offers `folder-location-apply-deletes-button`. `0` — the overwhelmingly
    /// common reading — renders neither.
    pub deletes_held: u64,
    /// Mirrored from [`fauna_client_sync::agent::BindingRow::deletes_skipped_unreadable`]
    /// by the same status-poll fold — part of this row's set could not be read,
    /// so the engine changed nothing and that subtree stopped syncing
    /// (`delete-propagation.md` § Unreadable is not absent). Non-zero renders
    /// `folder-location-unreadable`, a line with no action; `0` renders nothing.
    pub deletes_skipped_unreadable: u64,
    /// How this row's `folder-location-mode-toggle` renders, or `None` where no
    /// switch is rendered at all — the shared rule's answer
    /// ([`fauna_client_sync::agent::LocationBindingsModel::mode_toggle`]: agent
    /// truth for the mode, the agent's status reply for whether this host can
    /// serve an on-demand binding), projected flat. `Some` renders the switch
    /// on the row (`on-demand-files.md` § On-Demand Files → *The choice is the
    /// user's*); `None` — macOS, where on-demand is not the agent's — renders
    /// none, never an inert one.
    pub mode: Option<RenderedModeToggle>,
}

/// The flat, cfg-free projection of
/// [`fauna_client_sync::agent::ModeToggle`] — one bound row's
/// `folder-location-mode-toggle` as the Folders page paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderedModeToggle {
    /// The row's mode, agent truth. Checked = on-demand.
    pub on_demand: bool,
    /// Whether the switch takes a flip: `false` only where turning on-demand
    /// ON cannot be served on this host (a linux box without `fuse3`).
    pub enabled: bool,
    /// The line painted under the switch, as its key in the shared string
    /// table ([`fauna_client_sync::agent::OnDemandNotice::i18n_key`]) — why the
    /// switch is disabled, or why this row's on-demand root runs unmounted.
    pub notice: Option<&'static str>,
}

/// How often the local agent's `GetServiceStatus` is polled for the
/// `sync-agent-status` indicator. The agent has no health-push channel (unlike
/// per-file sync completion, which does), so polling is how the reading stays
/// current — the same 10 s cadence linux's `start_sync_agent_status_poll` uses.
#[cfg(any(unix, windows))]
const STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// The five readings ui.yaml pins for the global `sync-agent-status` element
/// (`sync-agent.md` § Local agent health). Derived client-side from a
/// `GetServiceStatus` attempt by the shared
/// [`fauna_client_sync::agent::agent_health_state`] — no wire field carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentHealth {
    /// The call succeeded and the agent's reported version matches this
    /// client's own build.
    Running,
    /// The call succeeded but the versions differ — an update replaced the
    /// on-disk binary and the running process hasn't picked it up.
    RestartPending,
    /// The call succeeded, but the agent reports a bound set behind its
    /// content-key floor or keyless (its writes held) — outranks
    /// `RestartPending`.
    KeysPending,
    /// The call succeeded, but the agent reports the nest holds no grant for
    /// this machine any more (nothing syncs once the app is closed) —
    /// outranks `KeysPending`.
    NotEnrolled,
    /// The IPC call itself failed (no socket / connect error). The starting
    /// reading, before the first poll lands.
    #[default]
    NotRunning,
}

/// A flat, cfg-free projection of the local agent's health, as the shell paint
/// reads it — the `sync-agent-status` twin of [`RenderedLocationBinding`], and for
/// the same reason: `ui.rs` compiles on every platform and must never name the
/// gated `fauna_ipc` types.
///
/// `version`/`uptime` are empty unless the agent answered, which is exactly the
/// ui.yaml contract for the optional `sync-agent-status-version` /
/// `sync-agent-status-uptime` children ("empty/absent while sync-agent-status
/// reads 'Not running'") — and the registry's empty-does-not-register rule then
/// makes that literal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedAgentStatus {
    pub health: AgentHealth,
    /// The running agent process's own version, verbatim.
    pub version: String,
    /// Its uptime, humanized through the shared `fauna_core::format::duration_secs`
    /// formatter (resolved here, so the paint stays dumb).
    pub uptime: String,
    /// The Status page's sync leg (`ui/status.md` § State & data shape) — the
    /// agent's `SyncStatusInfo` projection embedded verbatim, `Some` only when
    /// the agent answered. The Settings landing's Sync section reads it off
    /// this same cell, so the shell indicator and the Status page can never
    /// describe two different agents.
    pub sync: Option<fauna_client_status::SyncLeg>,
    /// The agent's `ServiceStatusInfo::notification_sink` — whether it can post
    /// the `ws-device` push arm's banner here (`None`: not answered, or the
    /// agent's sink probe has not finished). Read by Settings → Push notifications'
    /// inline line (`crate::push::standing_failure`).
    pub notification_sink: Option<bool>,
}

/// This device's sealed custodian store, in the platform-independent shape the
/// Backups page renders from — the [`RenderedAgentStatus`] pattern, and for the
/// same reason: `backups.rs` compiles on every platform and must never name the
/// gated `fauna_ipc` types.
///
/// `bytes` is disk truth and `generations` is index truth; they can legitimately
/// disagree (an interrupted store write leaves blobs no index row names), which
/// is why [`Self::holds_bytes`] — not a generation count — is what "this device
/// holds a sealed store" means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderedCustodianStore {
    pub generations: u64,
    pub files: u64,
    pub bytes: u64,
}

impl RenderedCustodianStore {
    /// The `store_holds_bytes` input to
    /// `fauna_core::data::custodian_store_is_orphaned`.
    pub fn holds_bytes(&self) -> bool {
        self.bytes > 0
    }
}

/// What a confirmed reclaim did — the platform-independent twin of
/// `fauna_ipc::sync::CustodianReclaimOutcome`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderedReclaim {
    /// The agent's own replica had not finished stopping, so **nothing was
    /// deleted**. A reported outcome, not an error: the page says so and the
    /// store stays exactly as it was.
    pub still_hosting: bool,
    pub freed_bytes: u64,
}

/// A carryable handle on this device's custodian store (see
/// [`SyncAgentState::custodian_store`]).
///
/// `Default` is the inert handle — off-desktop, or before the post-auth
/// install. Its reads answer `None`, which is **not** an empty store: a device
/// with no agent hosts no custodian and can measure nothing, while a zero-byte
/// reply from a real agent is the ordinary never-enrolled case. Both paint
/// nothing, and collapsing them would let a later caller read "cannot know" as
/// "measured zero".
#[derive(Clone, Default)]
pub struct CustodianStoreHandle {
    #[cfg(any(unix, windows))]
    provisioner: Option<Arc<native::TuiAgentProvisioner>>,
    /// The sync device id that agent was provisioned under — this account's
    /// (`crate::media::device_id_hex`), and so the id its custody rows name.
    #[cfg(any(unix, windows))]
    device_id: Option<String>,
}

impl CustodianStoreHandle {
    /// The sync device id this store's agent registers under — what the
    /// orphaned-store verdict compares the destination rows against. Carried
    /// from the agent's own init rather than re-read, so the verdict judges
    /// the custody this agent actually runs. `None` on the inert handle.
    pub fn device_id(&self) -> Option<&str> {
        #[cfg(any(unix, windows))]
        {
            self.device_id.as_deref()
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    /// Measure the store. `None` where tui drives no agent.
    pub async fn read(&self) -> Result<Option<RenderedCustodianStore>, String> {
        #[cfg(any(unix, windows))]
        if let Some(provisioner) = self.provisioner.as_ref() {
            let info = provisioner
                .custodian_store()
                .await
                .map_err(|e| e.to_string())?;
            return Ok(Some(RenderedCustodianStore {
                generations: info.generations,
                files: info.files,
                bytes: info.bytes,
            }));
        }
        Ok(None)
    }

    /// This device's own custodian store as the audit pass folds it
    /// (`fauna_client_backup::audit::run_audit_pass`'s `own_custodian`): the
    /// agent's standing source regressions under the device id this agent
    /// registers by. `None` where tui drives no agent, or the agent did not
    /// answer — "no store was read", which the pass never takes for a clean
    /// store.
    pub async fn own_custodian(&self) -> Option<fauna_client_backup::audit::OwnCustodianStore> {
        #[cfg(any(unix, windows))]
        if let (Some(provisioner), Some(device_id)) =
            (self.provisioner.as_ref(), self.device_id.as_deref())
        {
            return provisioner.own_custodian_store(device_id).await;
        }
        None
    }

    /// Free the whole store — the confirmed `backup-destination-reclaim-button`
    /// action. **Awaited**, unlike this seam's fire-and-forget binding
    /// mutations: it is destructive and behind a confirm, so the page repaints
    /// on what actually happened (`still_hosting` included) rather than on the
    /// keystroke.
    pub async fn reclaim(&self) -> Result<Option<RenderedReclaim>, String> {
        #[cfg(any(unix, windows))]
        if let Some(provisioner) = self.provisioner.as_ref() {
            let outcome = provisioner
                .reclaim_custodian_store()
                .await
                .map_err(|e| e.to_string())?;
            return Ok(Some(RenderedReclaim {
                still_hosting: outcome.still_hosting,
                freed_bytes: outcome.freed_bytes,
            }));
        }
        Ok(None)
    }

    /// Run the re-seed ceremony over the store this agent owns and wait for it
    /// to end — the confirmed `backup-destination-reseed-button` action,
    /// through the shared `fauna_client_sync::reseed_wire::await_agent_reseed`.
    /// `None` where tui drives no agent.
    ///
    /// `nest` and `folder_keys` are the owner's connection and custody: the
    /// target pre-create (`writer-signed-change-records.md` ruling (7)(a)(i))
    /// creates each restored folder's set through them before the job starts.
    #[cfg_attr(not(any(unix, windows)), allow(unused_variables))]
    pub async fn reseed(
        &self,
        nest: Arc<fauna_client::NestClient>,
        folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
        nest_backup_key: [u8; 32],
    ) -> Option<Result<fauna_client_backup::reseed::ReseedOutcome, String>> {
        #[cfg(any(unix, windows))]
        if let Some(provisioner) = self.provisioner.as_ref() {
            use fauna_client_sync::reseed_wire::{AGENT_RESEED_POLL, await_agent_reseed};
            let files = fauna_client_folders::FoldersClient::new(nest);
            let prepare = |names: Vec<String>| async move {
                fauna_client_folders::prepare_reseed_targets_logged(&files, &*folder_keys, &names)
                    .await;
            };
            return Some(
                await_agent_reseed(
                    provisioner.as_ref(),
                    prepare,
                    nest_backup_key,
                    AGENT_RESEED_POLL,
                )
                .await
                .map_err(|e| e.to_string()),
            );
        }
        None
    }
}

/// The session-scoped sync-agent surface held on `App` (mirrors `media`).
/// Default = signed-out empty; the authenticated surface is installed at
/// post-auth ([`init`]) and torn down on sign-out ([`SyncAgentState::teardown`]).
/// The `inner` surface is gated to the platforms with a local agent — on wasm
/// the whole thing is a zero-field no-op.
#[derive(Default)]
pub struct SyncAgentState {
    #[cfg(any(unix, windows))]
    inner: Option<native::AgentSurface>,
}

impl SyncAgentState {
    /// Whether the authenticated agent surface is installed — i.e. whether the
    /// mutating methods below actually reach the agent, or silently no-op.
    ///
    /// Exists so a **test-agent command can fail loudly instead of no-opping**:
    /// every mutator here is documented "a no-op before install / where tui
    /// drives no agent", which is the right shape for a UI gesture arriving
    /// pre-auth but is precisely what `testing.md` § conventions point 11
    /// forbids a driver-visible command from doing — an unreachable surface that
    /// acks clean reads downstream as a product bug in whatever the test asserted
    /// next. `false` off-desktop and before post-auth [`init`].
    pub fn is_active(&self) -> bool {
        #[cfg(any(unix, windows))]
        {
            self.inner.is_some()
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    /// Teardown for sign-out / account-switch / factory-reset: stop the
    /// convergence loop and push `UnprovisionCapability` (the agent deletes its
    /// persisted capability and stops engines). Idempotent — a no-op before
    /// install / where tui drives no agent. A plain app quit deliberately does NOT come here.
    ///
    /// The surface is dropped now; the un-provision round trip is handed back
    /// (`None` when nothing was installed) for `session::sign_out` to run on
    /// its one spawned stop, ahead of the erase that waits for it.
    pub fn teardown(
        &mut self,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.take() {
            return Some(Box::pin(surface.teardown()));
        }
        None
    }

    /// Drive one reconcile against the agent's `ListLocations` truth — the
    /// handler for [`crate::app::DataMessage::SyncAgentReconcile`] (the
    /// agent-reachable edge). Spawns the list→reconcile→push task; a no-op before
    /// install / where tui drives no agent.
    pub fn drive_reconcile(&self, tx: &UnboundedSender<UiMessage>) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.spawn_reconcile(tx.clone());
        }
        #[cfg(not(any(unix, windows)))]
        let _ = tx;
    }

    /// The device-local folder bindings the Folders page renders (the
    /// `folder-location-*` section, Slice 3), read off the shared
    /// [`fauna_client_sync::agent::LocationBindingsModel`]'s optimistic-UI rows —
    /// including a still-`PendingBind` add made while the agent is down, which
    /// must stay visible. Empty before install / where tui drives no agent.
    pub fn rendered_locations(&self) -> Vec<RenderedLocationBinding> {
        #[cfg(any(unix, windows))]
        {
            self.inner
                .as_ref()
                .map(|surface| surface.rendered_locations())
                .unwrap_or_default()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Vec::new()
        }
    }

    /// Optimistic add of a device-local folder bound to the set `folder_id`
    /// (its `FolderRef` wire form; `folder` is the label):
    /// the row renders immediately (the next frame reads [`Self::rendered_locations`]),
    /// the bind is pushed to the agent and re-pushed on every reconcile until it
    /// confirms (union semantics), and the per-set content-key blob is re-pushed
    /// so the newly-bound set's engine seals under its M2 content key rather than
    /// the owner `BackupKey` (the post-cutover regression). A no-op before install
    /// / where tui drives no agent. The tui twin of linux `sync_agent::add_binding`.
    pub fn add_binding(&self, path: String, folder: String, folder_id: String) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.add_binding(path, folder, folder_id);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = (path, folder, folder_id);
    }

    /// The local agent's health, as the sidebar's `sync-agent-status` line reads
    /// it (`sync-agent.md` § Local agent health). `None` means **this build never
    /// renders the element** — the ui.yaml conditional-presence rule ("only
    /// clients with a local fauna-sync-agent process render it") expressed in the
    /// type, so `ui.rs` neither paints nor registers it where tui drives no
    /// agent at all. On every desktop it is always `Some`, defaulting to
    /// [`AgentHealth::NotRunning`] before the surface is installed and before the
    /// first poll lands — the same starting reading linux shows.
    pub fn rendered_status(&self) -> Option<RenderedAgentStatus> {
        #[cfg(any(unix, windows))]
        {
            Some(
                self.inner
                    .as_ref()
                    .map(|surface| surface.rendered_status())
                    .unwrap_or_default(),
            )
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    /// Optimistic remove of every folder bound to `folder` (by set name — the
    /// shared model exposes only `remove_by_set`, and linux's per-row remove keys
    /// the same way, so removing any row of a set drops the whole set's bindings;
    /// on a device with one folder per set — the common shape — this is exactly
    /// per-row remove). The rows leave the rendered list at once and
    /// `RemoveLocation` re-pushes until the agent drops them. A no-op before
    /// install / where tui drives no agent. The tui twin of linux `sync_agent::remove_binding`.
    pub fn remove_binding(&self, folder: &str) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.remove_binding(folder);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = folder;
    }

    /// Nudge the agent's resident engine for `folder` to pull remote changes
    /// now, off its rescan cadence — the handler for an incoming
    /// `PushEvent::SyncChanged` (`file-sync.md` § Remote-change nudge). A no-op
    /// before install / where tui drives no agent. The tui twin of linux
    /// `sync_agent::pull_set_now`.
    pub fn pull_set_now(&self, folder: String, folder_hash: Option<Vec<u8>>) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.pull_set_now(folder, folder_hash);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = (folder, folder_hash);
    }

    /// A cloneable handle to this device's sealed custodian store on the agent,
    /// for the Backups page's `backup-orphaned-store-row`.
    ///
    /// The page's ops are self-contained futures (`backups::Op::run(self)`), so
    /// what they need has to be *carried*, not reached for — and this is the
    /// smallest thing that carries. Inert by `Default`, which is what an op
    /// built pre-auth or off-desktop gets.
    pub fn custodian_store(&self) -> CustodianStoreHandle {
        CustodianStoreHandle {
            #[cfg(any(unix, windows))]
            provisioner: self.inner.as_ref().map(|s| s.custodian_provisioner()),
            #[cfg(any(unix, windows))]
            device_id: self.inner.as_ref().map(|s| s.device_id().to_string()),
        }
    }

    /// **Test-only.** Run one custodian pull pass on the agent's hosted replica.
    ///
    /// `Ok(None)` means tui drives no agent on this platform at all — distinct
    /// from a report whose `hosting` is false, which means the agent is there
    /// and simply not hosting a replica yet. Collapsing the two would let the
    /// tier_3 proof's poll loop spin forever on a platform that can never host.
    ///
    /// `now_offset_secs` shifts this one pass's clock (`0` = the real one) —
    /// convention 14's fake clock for the self-audit's 24-hour debounce.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub async fn custodian_run_pass_now(
        &self,
        now_offset_secs: i64,
    ) -> Result<Option<fauna_ipc::sync::CustodianPassReport>, String> {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            return surface
                .custodian_run_pass_now(now_offset_secs)
                .await
                .map(Some);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = now_offset_secs;
        Ok(None)
    }

    /// Apply the mass-delete floor's held deletes for `folder` — the
    /// `folder-location-apply-deletes-button` gesture (`delete-propagation.md`
    /// § A wholesale-vanished folder is infrastructure failure: propagation of a
    /// held set is an **explicit user action**). Fire-and-forget on the agent
    /// seam like every other binding mutation; the reply repaints the row's hold
    /// and posts a tick. A no-op before install / where tui drives no agent.
    pub fn apply_held_deletes(
        &self,
        folder: String,
        tx: tokio::sync::mpsc::UnboundedSender<crate::app::UiMessage>,
    ) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.apply_held_deletes(folder, tx);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (folder, tx);
        }
    }

    /// Set the bound folder at `path` to `mode` (`"always"` | `"on-demand"`) —
    /// the `folder-location-mode-toggle` gesture. Fire-and-forget on the agent
    /// seam like every other binding mutation; the reconcile after the push
    /// repaints the row with the agent's mode and posts a tick. A no-op before
    /// install / where tui drives no agent.
    pub fn set_location_mode(
        &self,
        path: String,
        mode: String,
        tx: tokio::sync::mpsc::UnboundedSender<crate::app::UiMessage>,
    ) {
        #[cfg(any(unix, windows))]
        if let Some(surface) = self.inner.as_ref() {
            surface.set_location_mode(path, mode, tx);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (path, mode, tx);
        }
    }

    /// The share pump's two agent seams (`p2p.md` § Cross-user shared-set
    /// transfer, the out-of-process refinement): where each bound folder's
    /// state DB and tree live (`GetShareServeInfo`), and the engine's
    /// provisional-ingest door (`ShareIngest`). `None` before install / where
    /// tui drives no agent — the share glue simply has no sets to serve then.
    ///
    /// Both adapters are the SHARED ones
    /// (`fauna_sync_engine::share_glue::agent_share_access`): every app that
    /// drives the external agent needs the identical pair, so it is written
    /// once rather than per leg.
    #[cfg(feature = "p2p-share")]
    pub fn share_access(&self) -> Option<crate::share_glue::ReplicaAccess> {
        #[cfg(any(unix, windows))]
        {
            Some(fauna_sync_engine::share_glue::agent_share_access(
                self.inner.as_ref()?.share_provisioner(),
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }
}

/// Whether this machine's user manager keeps the agent alive after logout —
/// the `sync-agent-linger-toggle` reading (`sync-agent.md` § Headless
/// deployment). `None` means **do not render the offer at all**: no `loginctl`
/// to ask, or a platform with no such concept. `Some(bool)` is a definite
/// answer we may paint.
///
/// The `Option` is the same cross-platform-façade shape [`RenderedAgentStatus`]
/// uses, collapsing the unix-only [`fauna_client_sync::agent_spawner::LingerState`]
/// so the Folders page — which compiles everywhere — never names it.
pub fn linger_enabled() -> Option<bool> {
    #[cfg(unix)]
    {
        use fauna_client_sync::agent_spawner::{LingerControl, LingerState, LoginctlUser};
        match LoginctlUser.state() {
            LingerState::Enabled => Some(true),
            LingerState::Disabled => Some(false),
            LingerState::Unavailable => None,
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Flip lingering for the calling user, returning the state to paint afterwards
/// — re-read from `loginctl` rather than assumed, so a refusal (a locked-down
/// polkit) leaves the toggle showing the truth instead of an optimistic lie.
/// This is the one place tui runs a privileged-ish machine mutation, and it is
/// only ever the user's own explicit gesture.
pub fn set_linger(enabled: bool) -> Option<bool> {
    #[cfg(unix)]
    {
        use fauna_client_sync::agent_spawner::{LingerControl, LoginctlUser};
        if !LoginctlUser.set(enabled) {
            tracing::warn!("loginctl could not set linger={enabled}");
        }
        linger_enabled()
    }
    #[cfg(not(unix))]
    {
        let _ = enabled;
        None
    }
}

/// Build + start the sync-agent surface at the post-auth hook (the tui twin of
/// linux's `sync_agent::install`). Registers the `RenewBearer` grant, starts the
/// convergence loop, and pushes the initial content-key blob. Returns a
/// signed-out-empty state when the agent surface is unavailable: a platform with
/// no local agent, or a missing device id. (Under e2e it still provisions — the spawner just
/// direct-child-spawns the agent, see [`crate::e2e_mode_enabled`].)
/// `predecessors` are the account's retired owner `BackupKey`s, resolved once by
/// the caller off the shared `AccountRegistry` walk and shared with the `__mls`
/// barrier — see `session.rs`'s hook. Empty for every identity that never
/// succeeded. `attested_predecessors` are that same walk's actor ids — the
/// agent's fleet-view `prior` (`account-data-taxonomy.md` § The generation machinery
/// → *The source of `prior`*), which the seedless agent cannot attest itself.
/// `predecessor_chain` pairs those keys with their identities (ruling (8)(c)'s
/// per-signer bound).
#[allow(clippy::too_many_arguments)]
pub fn init(
    client: Arc<NestClient>,
    secret: [u8; 32],
    nest_url: &str,
    tx: &UnboundedSender<UiMessage>,
    predecessors: &[fauna_core::crypto::BackupKey],
    attested_predecessors: &[fauna_core::identity::ActorId],
    predecessor_chain: &[(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)],
    corpus: fauna_client_sync::agent::SuccessionCorpusContext,
) -> SyncAgentState {
    #[cfg(any(unix, windows))]
    {
        native::init(
            client,
            secret,
            nest_url,
            tx,
            predecessors,
            attested_predecessors,
            predecessor_chain,
            corpus,
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            client,
            secret,
            nest_url,
            tx,
            predecessors,
            attested_predecessors,
            predecessor_chain,
            corpus,
        );
        SyncAgentState::default()
    }
}

/// The `data.sync` agent-state object the e2e state report carries: the external
/// agent's live `running` flag and the binding model's rendered folder map.
///
/// **The tui twin of linux's `main.rs::sync_state_json`, deliberately using its
/// exact two derivations** (`sync-agent.md` § Implementation status today — the
/// windows A5 follow-on paragraph, which closed this same observability gap for
/// the other out-of-process-agent client): `running` = *any* hosted engine
/// reports `serving`, read over the per-user endpoint by the shared
/// [`fauna_client_sync::agent::any_engine_serving`]; each folder rendered as
/// `{path, folder}`. The shared consumer `conftest.py::_wait_for_engine` polls
/// exactly this pair, so any drift here silently changes what every
/// `_wait_for_engine`-backed e2e means.
///
/// **The block is emitted unconditionally**, even before the agent surface is
/// installed — an *absent* block reads to a polling test exactly like a present
/// one saying "not running", which is the ambiguity the windows leg called out.
///
/// **No `files` key.** linux carries a third `files` half (its in-process
/// engine's recent-sync-files list) which tui structurally has no source for —
/// its sync story is the out-of-process agent. Verified unconsumed before
/// omitting: no test under `tests/e2e-unified/` reads `data.sync.files`, so
/// emitting a fabricated `null` would invent a contract nothing asks for.
/// `running`/`locations` are the pair with real consumers.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn state_json(state: &SyncAgentState) -> serde_json::Value {
    let locations: Vec<serde_json::Value> = state
        .rendered_locations()
        .iter()
        .map(|b| {
            serde_json::json!({
                "path": b.path,
                "folder": b.folder,
            })
        })
        .collect();
    serde_json::json!({
        "running": running(),
        "locations": locations,
    })
}

/// Whether the local agent is serving ≥1 sync engine — the `data.sync.running`
/// read, over the shared derivation. `false` off-desktop (no agent exists).
///
/// ⚠ **`_cached`, because [`state_json`] is the test agent's state provider —
/// the ack path.** The blocking spelling costs a full request timeout against an
/// agent that connects but does not answer, which is longer than any command's
/// ack budget and presents as an unrelated command being dropped rather than as
/// a slow state push (`e2e-conventions.md` § convention 14 build-out → the
/// windows leg, where it cost three sessions).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn running() -> bool {
    #[cfg(any(unix, windows))]
    {
        fauna_client_sync::agent::any_engine_serving_cached()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(any(unix, windows))]
mod native {
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc::UnboundedSender;

    use fauna_client::NestClient;
    use fauna_client_account_runtime::p2p_participation::{
        EngineHolderNudge, EngineHolderNudgeSlot,
    };
    use fauna_client_sync::agent::PendingBind;
    use fauna_client_sync::agent::{
        AgentCapabilityInputs, AgentControlError, AgentHealthState, LocationBindingsModel,
        ReachabilityObserver, SuccessionCorpusContext, SyncAgentProvisioner, agent_health_state,
    };
    use fauna_sync_engine::share_glue::AgentBearerSource;

    use super::{DEVICE_LABEL, SyncAgentState};
    use crate::app::{DataMessage, UiMessage};

    /// The concrete provisioner instantiation tui drives (the twin of linux's
    /// `LinuxAgentProvisioner`).
    pub(super) type TuiAgentProvisioner = SyncAgentProvisioner<Arc<NestClient>, AgentBearerSource>;

    /// The live authenticated surface: the shared provisioner + the folder model
    /// (shared with the reconcile task) + the identity inputs the content-key
    /// refresh re-reads.
    pub(super) struct AgentSurface {
        provisioner: Arc<TuiAgentProvisioner>,
        /// The sync device id the provisioner registers under, kept for the
        /// Backups page's `CustodianStoreHandle`.
        device_id: String,
        /// Shared with the reconcile task. Locked only for the synchronous fold
        /// steps (never across an await), and by the Folders page's render
        /// (Slice 3).
        model: Arc<Mutex<LocationBindingsModel>>,
        /// The `sync-agent-status` reading, shared with the poll task. Same
        /// `Arc<Mutex<..>>`-plus-payload-free-tick idiom as `model` above: the
        /// task writes, the next frame's paint reads.
        status: Arc<Mutex<super::RenderedAgentStatus>>,
        /// Stops the poll task at teardown — it holds an `Arc` of the
        /// provisioner, so without this it would outlive the session and keep
        /// polling an agent we just unprovisioned.
        status_poll: tokio::task::AbortHandle,
        /// The UI loop, for the bind-confirmed tick [`spawn_bind`](Self::spawn_bind)
        /// posts so the expanded row re-reads its roster.
        tx: UnboundedSender<UiMessage>,
    }

    impl AgentSurface {
        /// Stop the loops now, and hand back the un-provision of the agent's
        /// persisted capability for the caller to drive (a dead agent is
        /// success, as before).
        ///
        /// Returned, not awaited here and not spawned: the reply is the agent's
        /// receipt that its own mount of the account store is down
        /// (`sync-agent.md` § Control plane split), and `sign_out`'s erase
        /// unlinks that store next — so the un-provision rides `sign_out`'s one
        /// spawned stop, which the erase waits for. Bounded by the pipe
        /// client's request ceiling; an unreachable agent degrades open, as it
        /// always did.
        pub(super) fn teardown(self) -> impl std::future::Future<Output = ()> + Send + 'static {
            self.status_poll.abort();
            let provisioner = self.provisioner;
            async move {
                let _ = provisioner.unprovision().await;
            }
        }

        /// The health reading the shell paints, cloned off the poll task's cell.
        pub(super) fn rendered_status(&self) -> super::RenderedAgentStatus {
            self.status.lock().unwrap().clone()
        }

        /// Spawn one reconcile pass against the agent's `ListLocations` truth.
        pub(super) fn spawn_reconcile(&self, tx: UnboundedSender<UiMessage>) {
            let provisioner = Arc::clone(&self.provisioner);
            let model = Arc::clone(&self.model);
            tokio::spawn(reconcile_once(provisioner, model, tx));
        }

        /// Project the model's rendered rows onto the cross-platform
        /// [`super::RenderedLocationBinding`] the Folders page reads. The lock is
        /// held only for the synchronous clone, never across an await.
        pub(super) fn rendered_locations(&self) -> Vec<super::RenderedLocationBinding> {
            let model = self.model.lock().unwrap();
            model
                .rendered()
                .into_iter()
                .map(|row| super::RenderedLocationBinding {
                    mode: model
                        .mode_toggle(&row)
                        .map(|toggle| super::RenderedModeToggle {
                            on_demand: toggle.on_demand,
                            enabled: toggle.enabled,
                            notice: toggle.notice.map(|notice| notice.i18n_key()),
                        }),
                    path: row.path,
                    folder: row.folder,
                    access_revoked: row.access_revoked,
                    deletes_held: row.deletes_held,
                    deletes_skipped_unreadable: row.deletes_skipped_unreadable,
                })
                .collect()
        }

        /// `folder-location-mode-toggle` — set the bound folder at `path` to
        /// `mode` over the agent's `SetLocationSyncMode` (the verb the windows
        /// app's `LocationBindingsController.SetModeAsync` sends; the agent
        /// persists `LocationConfig.mode` and re-drives its engines, so the
        /// placeholder root actually comes down or up). Not optimistic: the
        /// row repaints from the reconcile that follows, which re-reads the
        /// agent's `ListLocations` — so a refused flip leaves the switch
        /// showing the truth.
        ///
        /// A reconcile runs FIRST as well: a switch clicked on a row whose bind
        /// push has not landed yet would address a path the agent does not know,
        /// and that pass pushes (and confirms) the bind before the flip.
        pub(super) fn set_location_mode(
            &self,
            path: String,
            mode: String,
            tx: UnboundedSender<UiMessage>,
        ) {
            let provisioner = Arc::clone(&self.provisioner);
            let model = Arc::clone(&self.model);
            tokio::spawn(async move {
                reconcile_once(Arc::clone(&provisioner), Arc::clone(&model), tx.clone()).await;
                if let Err(e) = provisioner.set_location_sync_mode(path, mode).await {
                    tracing::warn!("set_location_sync_mode failed (the mode stands): {e}");
                }
                reconcile_once(provisioner, model, tx).await;
            });
        }

        /// Optimistic add (the twin of linux `sync_agent::add_binding`): record the
        /// row in the model — it renders on the next frame — then push the bind.
        /// The set's content keys are the agent's to resolve: the bind's reconcile
        /// re-reads its holder's custody (`on-demand-files.md` § Shared sets on a
        /// capability host → *One mechanism*).
        pub(super) fn add_binding(&self, path: String, folder: String, folder_id: String) {
            let pending = self.model.lock().unwrap().add(path, folder, folder_id);
            self.spawn_bind(pending);
        }

        /// Optimistic remove by set (the twin of linux `sync_agent::remove_binding`):
        /// flip every row for the set to pending-unbind (they leave the render) and
        /// push the unbinds.
        pub(super) fn remove_binding(&self, folder: &str) {
            let paths = self.model.lock().unwrap().remove_by_set(folder);
            for path in paths {
                self.spawn_unbind(path);
            }
        }

        /// Nudge the agent's resident engine for `folder` to pull remote changes
        /// now, off its rescan cadence — the tui twin of linux `sync_agent::
        /// pull_set_now`. Fire-and-forget: an absent engine or an already-pending
        /// pull is a no-op agent-side, and the rescan tick stays the backstop
        /// (`file-sync.md` § Remote-change nudge).
        pub(super) fn pull_set_now(&self, folder: String, folder_hash: Option<Vec<u8>>) {
            let provisioner = Arc::clone(&self.provisioner);
            tokio::spawn(async move {
                if let Err(e) = provisioner.pull_folder_now(folder, folder_hash).await {
                    tracing::debug!(
                        "remote-change pull-now nudge failed (tick will catch up): {e}"
                    );
                }
            });
        }

        /// The provisioner clone the Backups page's `CustodianStoreHandle`
        /// carries into its ops — the same shape [`Self::share_provisioner`]
        /// takes, and for the same reason: an op is a self-contained future, so
        /// what it needs must be carried into it.
        pub(super) fn custodian_provisioner(&self) -> Arc<TuiAgentProvisioner> {
            Arc::clone(&self.provisioner)
        }

        /// The sync device id this agent was provisioned under.
        pub(super) fn device_id(&self) -> &str {
            &self.device_id
        }

        /// **Test-only.** Run one custodian pull pass on the agent's hosted
        /// replica and return what it did.
        ///
        /// Unlike [`Self::pull_set_now`] this is **awaited**, not
        /// fire-and-forget: the whole point is that the agent replies only once
        /// the pass has finished, which is what lets a test assert state instead
        /// of timing (`testing.md` convention 14).
        #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
        pub(super) async fn custodian_run_pass_now(
            &self,
            now_offset_secs: i64,
        ) -> Result<fauna_ipc::sync::CustodianPassReport, String> {
            self.provisioner
                .custodian_run_pass_now(now_offset_secs)
                .await
                .map_err(|e| e.to_string())
        }

        /// The provisioner clone the share plane's two SHARED agent adapters
        /// ride (`fauna_sync_engine::share_glue::agent_share_access`). The
        /// glue task holds it past teardown harmlessly: `unprovision` makes
        /// every later request fail, and the glue loop exits on the account
        /// runtime's own death.
        #[cfg(feature = "p2p-share")]
        pub(super) fn share_provisioner(&self) -> Arc<TuiAgentProvisioner> {
            Arc::clone(&self.provisioner)
        }

        /// `folder-location-apply-deletes-button` — the user confirmed the
        /// mass-delete floor's hold on `folder`, so propagate it. Repaints from
        /// the reply's `remaining_held` (the post-apply truth) rather than from
        /// the count the button rendered, and posts a tick so the row's
        /// `folder-location-deletes-held` line clears without waiting for the
        /// 10 s poll.
        ///
        /// A failure leaves the hold exactly as it was — nothing was recorded,
        /// the surface keeps offering the verb, and the next poll re-derives.
        /// That is the whole point of the agent re-deriving at click time: there
        /// is no client-side state to unwind.
        pub(super) fn apply_held_deletes(&self, folder: String, tx: UnboundedSender<UiMessage>) {
            let provisioner = Arc::clone(&self.provisioner);
            let model = Arc::clone(&self.model);
            tokio::spawn(async move {
                match provisioner.apply_held_deletes(folder.clone()).await {
                    Ok(info) => {
                        model
                            .lock()
                            .unwrap()
                            .set_engine_hold(&folder, info.remaining_held);
                        let _ = tx.send(UiMessage::Data(DataMessage::SyncAgentChanged));
                    }
                    Err(e) => {
                        tracing::warn!("apply_held_deletes failed (the hold stands): {e}");
                    }
                }
            });
        }

        /// Push one bind (`add_binding`); confirm into the model on success, the
        /// tui twin of linux's `push_bind`. `reconcile_once`'s own `to_bind` loop
        /// awaits `bind_location` inline instead of spawning per-row, so this is
        /// the only spawned bind.
        ///
        /// A confirmed bind posts [`DataMessage::FolderBindConfirmed`]: the
        /// agent's bind enrolled this device's place if it held none
        /// (`file-sync.md` § 4, *A local presence writes the place it needs*),
        /// so an expanded row for the set re-reads its roster and the new seat
        /// paints.
        fn spawn_bind(&self, pending: PendingBind) {
            let provisioner = Arc::clone(&self.provisioner);
            let model = Arc::clone(&self.model);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let path = pending.path.clone();
                let folder = pending.folder.clone();
                match provisioner
                    .bind_location(pending.path, pending.folder, pending.folder_id)
                    .await
                {
                    Ok(()) => {
                        model.lock().unwrap().confirm_bind(&path);
                        let _ = tx.send(UiMessage::Data(DataMessage::FolderBindConfirmed(folder)));
                    }
                    Err(e) => tracing::warn!("bind_location failed (reconcile will retry): {e}"),
                }
            });
        }

        /// Push one unbind; drop the pending row on success. The tui twin of
        /// linux's `push_unbind`.
        fn spawn_unbind(&self, path: String) {
            let provisioner = Arc::clone(&self.provisioner);
            let model = Arc::clone(&self.model);
            tokio::spawn(async move {
                match provisioner.unbind_location(path.clone()).await {
                    Ok(()) => model.lock().unwrap().confirm_unbind(&path),
                    Err(e) => tracing::warn!("unbind_location failed (reconcile will retry): {e}"),
                }
            });
        }
    }

    /// Forwards the convergence loop's agent-reachable edge (a tokio task) onto
    /// the main loop as a [`DataMessage::SyncAgentReconcile`] tick — the re-fire
    /// that closes the attach-time race (the 2026-07-19 A4 review finding; tui
    /// inherits it identically, over the message channel where linux uses
    /// `glib::idle_add_once`).
    struct TuiReachabilityObserver {
        tx: UnboundedSender<UiMessage>,
    }

    impl ReachabilityObserver for TuiReachabilityObserver {
        fn on_agent_reachable(&self) {
            let _ = self
                .tx
                .send(UiMessage::Data(DataMessage::SyncAgentReconcile));
        }
    }

    /// Assemble the capability inputs the identity-holding client supplies once at
    /// construction (`sync-agent.md` § Credential model). Pulled out as a pure
    /// function so the crypto-critical assembly — the `BackupKey` derivation the
    /// agent seals sync segments under — is unit-pinned without a live nest.
    fn build_capability_inputs(
        secret: [u8; 32],
        device_id: String,
        nest_url: &str,
        predecessors: &[fauna_core::crypto::BackupKey],
        attested_predecessors: &[fauna_core::identity::ActorId],
    ) -> AgentCapabilityInputs {
        AgentCapabilityInputs {
            identity_secret: secret.to_vec(),
            backup_key: fauna_core::crypto::BackupKey::derive(&secret)
                .to_bytes()
                .to_vec(),
            // The retired owner keys of the identities this account succeeded
            // from — read candidates for the corpus a succession re-pointed but
            // did not re-seal (`sync-agent.md` § Credential model → *Retired
            // owner keys after an identity succession*). Resolved by the caller
            // from the shared `AccountRegistry` walk, never re-derived here:
            // the agent is the process that opens this account's bytes, and on
            // a fresh successor device every chunk is predecessor-sealed.
            predecessor_backup_keys: predecessors.iter().map(|k| k.to_bytes().to_vec()).collect(),
            // The same walk's ATTESTED actor ids — the agent's fleet-view `prior`
            // for the account runtime it hosts app-dead. The agent holds no
            // registry and must not read a writer-asserted list for this
            // (`account-data-taxonomy.md` § The generation machinery → *The
            // source of `prior`*); public material, outside bound (3).
            predecessor_actor_ids: attested_predecessors
                .iter()
                .map(|id| id.0.to_vec())
                .collect(),
            // Set by `init` from the registry's paired walk
            // (`with_predecessor_chain`).
            predecessor_keys_by_actor: Vec::new(),
            device_id,
            device_label: DEVICE_LABEL.to_string(),
            nest_url: nest_url.to_string(),
        }
    }

    /// The retired keys paired with their identities, nearest hop first
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`) — the per-signer
    /// bound's input on the agent (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(c)).
    fn with_predecessor_chain(
        mut inputs: AgentCapabilityInputs,
        chain: &[(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)],
    ) -> AgentCapabilityInputs {
        inputs.predecessor_keys_by_actor = chain
            .iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .collect();
        inputs
    }

    /// How this OS starts the agent when the endpoint probe finds it absent
    /// (`sync-agent.md` § Packaging + lifecycle) — the one per-OS line in an
    /// otherwise OS-neutral surface.
    ///
    /// * **unix** — production writes+ensures the systemd user unit; under e2e
    ///   the same shared spawner direct-child-spawns the agent into this
    ///   launch's isolated XDG world, so no machine-global systemd surface is
    ///   touched while the real agent binary still runs.
    /// * **windows** — a console-less detached child (the shape the C# app
    ///   used). No e2e arm is needed: there is no per-user unit to avoid
    ///   writing, and the isolation an isolated XDG world gives unix for free
    ///   arrives here as the harness's forwarded `--pipe-name`/`--data-dir`,
    ///   which the shared spawner reads from the environment.
    fn platform_spawner() -> Arc<dyn fauna_client_sync::agent::AgentSpawner> {
        #[cfg(unix)]
        {
            // Selects the spawner MODE, not whether we provision: under e2e the
            // `SystemdUserUnitSpawner` direct-child-spawns the agent into the
            // launch's isolated `XDG_RUNTIME_DIR` (per-launch socket, so no
            // cross-test leak) instead of writing a machine-global systemd user
            // unit — the linux shape. Production is unaffected, and
            // `e2e_mode_enabled`'s production twin is what keeps a release build
            // from even naming `FAUNA_E2E_AGENT_PORT`: this call site is plumbing
            // every flavor compiles, so it cannot carry a `#[cfg]` of its own.
            Arc::new(
                fauna_client_sync::agent_spawner::SystemdUserUnitSpawner::new(
                    crate::e2e_mode_enabled(),
                ),
            )
        }
        #[cfg(windows)]
        {
            Arc::new(fauna_client_sync::agent_spawner::WindowsDetachedSpawner::new())
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn init(
        client: Arc<NestClient>,
        secret: [u8; 32],
        nest_url: &str,
        tx: &UnboundedSender<UiMessage>,
        predecessors: &[fauna_core::crypto::BackupKey],
        attested_predecessors: &[fauna_core::identity::ActorId],
        predecessor_chain: &[(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)],
        corpus: SuccessionCorpusContext,
    ) -> SyncAgentState {
        // This account's id (`sync-agent-credentials.md` § Credential model):
        // the agent registers under it, and the custodian handle carries it.
        let device_id = match crate::media::device_id_hex_for_secret(secret) {
            Some(id) => id,
            None => {
                tracing::error!(
                    "sync agent: could not establish device id — surface not installed"
                );
                return SyncAgentState::default();
            }
        };
        let registered_device_id = device_id.clone();
        let bearer_source = Arc::new(AgentBearerSource(client.auth().bearer()));
        let provisioner = match SyncAgentProvisioner::new(
            Arc::clone(&client),
            with_predecessor_chain(
                build_capability_inputs(
                    secret,
                    device_id,
                    nest_url,
                    predecessors,
                    attested_predecessors,
                ),
                predecessor_chain,
            ),
            platform_spawner(),
            bearer_source,
            Some(Arc::new(TuiReachabilityObserver { tx: tx.clone() })),
        ) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                tracing::error!("sync agent provisioner build failed: {e}");
                return SyncAgentState::default();
            }
        };
        // The devices page's participation switch asks the agent — the usual
        // engine holder — for the pass that reads the row (`p2p.md`
        // § Per-device participation, (c)); weak, so it lapses with this session.
        let holder_nudge: Arc<dyn EngineHolderNudge> = provisioner.clone();
        EngineHolderNudgeSlot::seat().publish(&holder_nudge);

        {
            let provisioner = Arc::clone(&provisioner);
            tokio::spawn(async move {
                if let Err(e) = provisioner.start().await {
                    tracing::error!("sync agent provisioner start failed: {e}");
                }
            });
        }

        // This process is an open app: hold the agent's attachment lease for
        // the process's life, so the agent's `ws-device` arm leaves banners to
        // tui while it runs (`fauna_client_sync::attachment`). Once per process
        // — an account switch re-inits this surface, the app stays open — and
        // the same endpoint the provisioner resolves.
        {
            static ATTACHMENT: std::sync::OnceLock<
                Option<fauna_client_sync::attachment::AppAttachment>,
            > = std::sync::OnceLock::new();
            ATTACHMENT.get_or_init(|| {
                fauna_ipc::endpoint::AgentEndpoint::default_for_user()
                    .inspect_err(|e| tracing::warn!("agent attachment: no endpoint: {e}"))
                    .ok()
                    .map(|endpoint| {
                        fauna_client_sync::attachment::attach_app(
                            endpoint,
                            fauna_client_sync::attachment::AttachingApp {
                                app: "tui".into(),
                                notification_identity: None,
                            },
                        )
                    })
            });
        }

        // The `sync-agent-status` poll (`sync-agent.md` § Local agent health).
        // Starts at the post-auth hook alongside the convergence loop, so the
        // indicator reflects the agent this session just provisioned — including
        // the spawn window, which it renders honestly as "Not running".
        let status = Arc::new(Mutex::new(super::RenderedAgentStatus::default()));
        // tui has no `location-map.json` migration source (it never hosted in-app
        // engines), so the model starts empty and adopts agent-side rows on the
        // first reconcile. Built before the poll spawns because the poll folds the
        // mass-delete floor's holds onto it (`ListEngines` → `fold_engine_holds`).
        let model = Arc::new(Mutex::new(LocationBindingsModel::default()));
        let status_poll = tokio::spawn(status_poll_loop(
            Arc::clone(&provisioner),
            Arc::clone(&status),
            Arc::clone(&model),
            corpus,
            tx.clone(),
        ))
        .abort_handle();

        let surface = AgentSurface {
            provisioner,
            device_id: registered_device_id,
            model,
            status,
            status_poll,
            tx: tx.clone(),
        };

        SyncAgentState {
            inner: Some(surface),
        }
    }

    /// Project one `GetServiceStatus` attempt onto the flat reading the shell
    /// paints (`sync-agent.md` § Local agent health). Pure — the four-state comes
    /// from the shared [`agent_health_state`] (*Keys pending* included, which the
    /// agent reports on the same reply), and `version`/`uptime` are filled
    /// only when the agent actually answered, so a failed call renders the
    /// "Not running" line with both children empty (the ui.yaml contract).
    fn project_status(
        result: &Result<fauna_ipc::sync::ServiceStatusInfo, AgentControlError>,
        local_build_version: &str,
    ) -> super::RenderedAgentStatus {
        let status = result.as_ref().ok();
        let health = match agent_health_state(result, local_build_version) {
            AgentHealthState::Running => super::AgentHealth::Running,
            AgentHealthState::RestartPending => super::AgentHealth::RestartPending,
            AgentHealthState::KeysPending => super::AgentHealth::KeysPending,
            AgentHealthState::NotEnrolled => super::AgentHealth::NotEnrolled,
            AgentHealthState::NotRunning => super::AgentHealth::NotRunning,
        };
        match status {
            Some(s) => super::RenderedAgentStatus {
                health,
                version: s.version.clone(),
                uptime: crate::wizard::localized(&fauna_core::format::duration_secs(s.uptime_secs)),
                sync: Some(fauna_client_status::SyncLeg::from(&s.sync)),
                notification_sink: s.notification_sink,
            },
            None => super::RenderedAgentStatus {
                health,
                version: String::new(),
                uptime: String::new(),
                sync: None,
                notification_sink: None,
            },
        }
    }

    /// Poll the local agent's `GetServiceStatus` forever, publishing each new
    /// reading to the shared cell and posting a payload-free
    /// [`DataMessage::SyncAgentStatusChanged`] tick **only when it actually
    /// moved** — the observer idiom every tui manager uses, which also keeps a
    /// steady-state agent from waking the main loop every 10 s.
    ///
    /// The first tick fires immediately (tokio's `interval` contract), so the
    /// indicator leaves its starting "Not running" as soon as the agent answers,
    /// exactly like linux's first `poll()` call.
    /// It also carries the **post-succession corpus re-seal**'s progress
    /// (`succession-aftermath.md` § Re-key scope, leg 5 of the aftermath), on the
    /// same tick and for a reason that is not convenience: the pass runs inside
    /// the agent, so this poll is the *only* channel by which the app can learn
    /// how far it has got, and a Settings hook that re-ran the pass to render its
    /// own line would race the very work it reports on (the finding, one leg
    /// on). The reading is pushed as a payload-carrying
    /// [`DataMessage::CorpusResealProgress`] rather than through the shared cell,
    /// so it lands on `App::aftermath` beside the four legs that *do* run
    /// in-process and the Settings render stays uniform across all five.
    /// It also folds the **mass-delete floor**'s holds onto the binding model
    /// (`ListEngines` → `LocationBindingsModel::fold_engine_holds`), for the same
    /// reason the corpus line rides here: the hold is derived inside the agent on
    /// its own reconcile cadence, so a level the app must *watch* — no user
    /// gesture produces it, and the binding reconcile that would otherwise be the
    /// natural home only runs on mutations and reachability edges. Folding it
    /// into the cached model (never reading the socket at paint time) is also
    /// what keeps the e2e state provider free of blocking I/O.
    async fn status_poll_loop(
        provisioner: Arc<TuiAgentProvisioner>,
        status: Arc<Mutex<super::RenderedAgentStatus>>,
        model: Arc<Mutex<LocationBindingsModel>>,
        corpus: SuccessionCorpusContext,
        tx: UnboundedSender<UiMessage>,
    ) {
        let mut ticker = tokio::time::interval(super::STATUS_POLL_INTERVAL);
        // A slow poll must not make the next tick fire instantly (and then burst
        // to catch up) — health is a level, not an event stream.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_reseal = None;
        loop {
            ticker.tick().await;
            let result = provisioner.get_service_status().await;
            let next = project_status(&result, env!("CARGO_PKG_VERSION"));
            let changed = {
                let mut cell = status.lock().unwrap();
                if *cell == next {
                    false
                } else {
                    *cell = next;
                    true
                }
            };
            if changed {
                let _ = tx.send(UiMessage::Data(DataMessage::SyncAgentStatusChanged));
            }
            // The host's on-demand surface rides the same reply, and every bound
            // row's `folder-location-mode-toggle` reads it
            // (`LocationBindingsModel::mode_toggle`) — a linux host without
            // `fuse3` paints the switch disabled with its reason. An unreachable
            // agent folds nothing: the rows keep the last-known surface.
            if let Ok(reply) = &result
                && model.lock().unwrap().fold_service_status(reply)
            {
                let _ = tx.send(UiMessage::Data(DataMessage::SyncAgentChanged));
            }

            // An unreachable agent yields no rows, which folds to the same
            // reading as an agent that has recorded no pass: silence. That is
            // honest — the app genuinely does not know — and the health
            // indicator above is already saying the agent is down. For the hold
            // fold the same emptiness reads as "no live engine stands behind a
            // count", which is exactly the derived-never-stored contract.
            let engines = provisioner.list_engines().await.unwrap_or_default();
            // The binding PARK (`access_revoked`) is the same kind of level: the
            // agent derives it when the owning nest refuses a write, and the
            // binding reconcile — which also mirrors it — runs only on mutations
            // and reachability edges, so without this watch a demoted writer's
            // row never learned it had stopped syncing. An unreachable agent
            // folds nothing (the rows keep their last-known park).
            let locations = provisioner.list_locations().await.ok();

            // The mass-delete floor's per-set hold and the park, onto the rows
            // the Folders page renders. A tick is posted only when the rendered
            // rows moved, so a steady-state agent costs nothing but the socket
            // round trips.
            {
                let before = model.lock().unwrap().rendered();
                model.lock().unwrap().fold_engine_holds(&engines);
                if let Some(locations) = &locations {
                    model.lock().unwrap().fold_parks(locations);
                }
                let after = model.lock().unwrap().rendered();
                if before != after {
                    let _ = tx.send(UiMessage::Data(DataMessage::SyncAgentChanged));
                }
            }

            // The aftermath's corpus line. Skipped entirely for every identity
            // that never succeeded — the overwhelmingly common fleet.
            if !corpus.succeeded {
                continue;
            }
            let reading = fauna_client_sync::agent::corpus_reseal_progress(&engines, corpus);
            if reading != last_reseal {
                last_reseal = reading.clone();
                if let Some(progress) = reading {
                    let _ = tx.send(UiMessage::Data(DataMessage::CorpusResealProgress(progress)));
                }
            }
        }
    }

    /// One reconcile pass: list the agent's folders, fold them into the model,
    /// push the outstanding binds/unbinds, and post a [`DataMessage::SyncAgentChanged`]
    /// tick iff the rendered set changed. Runs on a tokio task; the model lock is
    /// held only for the synchronous fold steps, never across an await.
    async fn reconcile_once(
        provisioner: Arc<TuiAgentProvisioner>,
        model: Arc<Mutex<LocationBindingsModel>>,
        tx: UnboundedSender<UiMessage>,
    ) {
        let agent_rows = match provisioner.list_locations().await {
            Ok(rows) => rows,
            // Unreachable — the next agent-reachable edge re-drives us.
            Err(_) => return,
        };
        let before = model.lock().unwrap().rendered();
        let actions = model.lock().unwrap().reconcile(&agent_rows);
        for pending in actions.to_bind {
            // A failed push leaves the row pending; the next reconcile re-pushes
            // (union semantics — the model owns the retry, not this call site).
            let path = pending.path.clone();
            if provisioner
                .bind_location(pending.path, pending.folder, pending.folder_id)
                .await
                .is_ok()
            {
                model.lock().unwrap().confirm_bind(&path);
            }
        }
        for path in actions.to_unbind {
            if provisioner.unbind_location(path.clone()).await.is_ok() {
                model.lock().unwrap().confirm_unbind(&path);
            }
        }
        let after = model.lock().unwrap().rendered();
        if before != after {
            let _ = tx.send(UiMessage::Data(DataMessage::SyncAgentChanged));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The crypto-critical assembly: the agent's capability must seal under the
        /// `BackupKey` derived from THIS identity's seed (not some other key), the
        /// content-key blob must start empty (the real per-set blob is pushed
        /// separately, else a bound set runs unbound), and the device label must
        /// be `fauna-tui`.
        #[test]
        fn capability_inputs_carry_the_derived_backup_key_and_tui_label() {
            let secret = [7u8; 32];
            let inputs = build_capability_inputs(
                secret,
                "dev-abc".to_string(),
                "https://nest.example",
                &[],
                &[],
            );
            assert_eq!(inputs.identity_secret, secret.to_vec());
            assert_eq!(
                inputs.backup_key,
                fauna_core::crypto::BackupKey::derive(&secret)
                    .to_bytes()
                    .to_vec(),
                "the capability must seal under the BackupKey derived from this seed"
            );
            assert_eq!(inputs.device_id, "dev-abc");
            assert_eq!(
                inputs.device_label, "fauna-tui",
                "the nest device list must show this device as fauna-tui"
            );
            assert_eq!(inputs.nest_url, "https://nest.example");
        }

        /// **The call-site pin for the identity-succession read fallback**
        /// (`sync-agent.md` § Credential model → *Retired owner keys after an
        /// identity succession*).
        ///
        /// ⚠ A pin inside `fauna-client-sync` or `fauna-core` structurally
        /// cannot observe *this app* reverting to `Vec::new()` — the
        /// lesson, and the exact shape that let the read half ship with zero
        /// production writers. The regression pin has to
        /// live where the caller does.
        ///
        /// Mutation: replace the `predecessor_backup_keys` line in
        /// `build_capability_inputs` with `Vec::new()` → this reds.
        #[test]
        fn capability_inputs_carry_the_accounts_retired_owner_keys() {
            let retired = [
                fauna_core::crypto::BackupKey::from_bytes([0x11u8; 32]),
                fauna_core::crypto::BackupKey::from_bytes([0x22u8; 32]),
            ];
            let inputs = build_capability_inputs(
                [7u8; 32],
                "dev-abc".to_string(),
                "https://nest.example",
                &retired,
                &[],
            );
            assert_eq!(
                inputs.predecessor_backup_keys,
                retired
                    .iter()
                    .map(|k| k.to_bytes().to_vec())
                    .collect::<Vec<_>>(),
                "the agent is the process that opens this account's bytes on desktop; \
                 without the retired keys a successor's corpus stays dark, and on a \
                 fresh device that is EVERY file"
            );
            assert_eq!(
                inputs.predecessor_backup_keys.len(),
                2,
                "every ancestor, not just the nearest — a corpus can still rest under \
                 a grandpredecessor when an intermediate re-seal never finished"
            );
        }

        /// The negative control: an identity that never succeeded pays nothing.
        /// Without this the pin above would pass on an assembly that always
        /// pushes something.
        #[test]
        fn capability_inputs_carry_no_retired_keys_for_a_never_succeeded_identity() {
            let inputs = build_capability_inputs([7u8; 32], "d".to_string(), "u", &[], &[]);
            assert!(inputs.predecessor_backup_keys.is_empty());
            assert!(inputs.predecessor_actor_ids.is_empty());
        }

        /// **The call-site pin for the agent's ATTESTED `prior`**
        /// (`account-data-taxonomy.md` § The generation machinery → *The
        /// source of `prior`*): the registry's attested predecessor ids reach
        /// the capability inputs verbatim and in order. Same reason the
        /// retired-keys pin above lives here — a shared-crate pin cannot see
        /// this app reverting to `Vec::new()`, and the failure is silent: the
        /// agent's fleet view would drop every predecessor-signed enrollment
        /// while tui's own view kept them, two views of one account disagreeing
        /// with nothing on screen.
        ///
        /// Mutation: replace the `predecessor_actor_ids` line in
        /// `build_capability_inputs` with `Vec::new()` → this reds.
        #[test]
        fn capability_inputs_carry_the_accounts_attested_predecessor_ids() {
            let attested = [
                fauna_core::identity::ActorId([0x77u8; 32]),
                fauna_core::identity::ActorId([0x78u8; 32]),
            ];
            let inputs = build_capability_inputs(
                [7u8; 32],
                "dev-abc".to_string(),
                "https://nest.example",
                &[],
                &attested,
            );
            assert_eq!(
                inputs.predecessor_actor_ids,
                vec![vec![0x77u8; 32], vec![0x78u8; 32]],
                "the agent hosts this account's runtime seedless and holds no registry: \
                 these ids ARE its succession-crossing signer list"
            );
        }

        /// A different seed derives a different `BackupKey` — the derivation is
        /// keyed on the identity, not a constant.
        #[test]
        fn a_different_seed_derives_a_different_backup_key() {
            let a = build_capability_inputs([1u8; 32], "d".to_string(), "u", &[], &[]);
            let b = build_capability_inputs([2u8; 32], "d".to_string(), "u", &[], &[]);
            assert_ne!(a.backup_key, b.backup_key);
        }

        fn status_info(version: &str, uptime_secs: u64) -> fauna_ipc::sync::ServiceStatusInfo {
            fauna_ipc::sync::ServiceStatusInfo {
                version: version.to_string(),
                uptime_secs,
                ..Default::default()
            }
        }

        /// The agent answered with our own build version: "Running", and both
        /// optional children carry real text.
        #[test]
        fn a_matching_version_projects_running_with_version_and_uptime() {
            let status = project_status(&Ok(status_info("1.4.2", 3_600)), "1.4.2");
            assert_eq!(status.health, super::super::AgentHealth::Running);
            assert_eq!(status.version, "1.4.2");
            assert!(
                !status.uptime.is_empty(),
                "uptime must humanize through the shared duration_secs formatter"
            );
        }

        /// The agent answered, but from a binary an update already replaced —
        /// "Restart pending", and the children still show the RUNNING process's
        /// values (that mismatch is the whole point of the reading).
        #[test]
        fn a_differing_version_projects_restart_pending() {
            let status = project_status(&Ok(status_info("1.4.1", 60)), "1.4.2");
            assert_eq!(status.health, super::super::AgentHealth::RestartPending);
            assert_eq!(status.version, "1.4.1");
        }

        /// The agent's own *Keys pending* reads "Keys pending", over a version
        /// mismatch too (Restart pending is the weeks-long normal on an
        /// un-restarted agent and must not hide it), and the children still
        /// describe the process that answered.
        #[test]
        fn an_agent_reporting_keys_pending_projects_keys_pending() {
            let info = fauna_ipc::sync::ServiceStatusInfo {
                keys_pending: true,
                ..status_info("1.4.1", 60)
            };
            let status = project_status(&Ok(info.clone()), "1.4.2");
            assert_eq!(status.health, super::super::AgentHealth::KeysPending);
            assert_eq!(status.version, "1.4.1");
            assert!(!status.uptime.is_empty());
        }

        /// The agent's own `needs_reenrollment` reads "Not enrolled", over
        /// *Keys pending* and a version mismatch, and the children still
        /// describe the process that answered.
        #[test]
        fn an_agent_reporting_a_refused_renewal_projects_not_enrolled() {
            let info = fauna_ipc::sync::ServiceStatusInfo {
                needs_reenrollment: true,
                keys_pending: true,
                ..status_info("1.4.1", 60)
            };
            let status = project_status(&Ok(info), "1.4.2");
            assert_eq!(status.health, super::super::AgentHealth::NotEnrolled);
            assert_eq!(status.version, "1.4.1");
            assert!(!status.uptime.is_empty());
        }

        /// The IPC call itself failed: "Not running", and BOTH children are empty
        /// — ui.yaml pins that ("empty/absent while sync-agent-status reads 'Not
        /// running'"), and the registry's empty-does-not-register rule turns it
        /// into real absence.
        #[test]
        fn a_failed_call_projects_not_running_with_empty_children() {
            let status = project_status(&Err(AgentControlError::new("unreachable")), "1.4.2");
            assert_eq!(status.health, super::super::AgentHealth::NotRunning);
            assert!(status.version.is_empty());
            assert!(status.uptime.is_empty());
            assert_eq!(
                status.sync, None,
                "an agent that did not answer has no backlog to report"
            );
        }

        /// The Status page's sync leg is the agent's projection verbatim
        /// (`ui/status.md` § State & data shape) — present exactly when the
        /// agent answered.
        #[test]
        fn an_answer_carries_the_sync_projection_verbatim() {
            let mut info = status_info("1.4.2", 60);
            info.sync.files_pending = 3;
            info.sync.bytes_pending = 4_096;
            info.sync.last_sync = Some(1_700_000_000);
            let status = project_status(&Ok(info.clone()), "1.4.2");
            assert_eq!(
                status.sync,
                Some(fauna_client_status::SyncLeg {
                    files_pending: 3,
                    bytes_pending: 4_096,
                    last_sync: Some(1_700_000_000),
                })
            );
        }

        /// The default (pre-install, pre-first-poll) reading is the same one a
        /// failed call produces — so the indicator never claims health it has not
        /// observed.
        #[test]
        fn the_default_reading_matches_a_failed_call() {
            assert_eq!(
                super::super::RenderedAgentStatus::default(),
                project_status(&Err(AgentControlError::new("unreachable")), "1.4.2")
            );
        }
    }
}
