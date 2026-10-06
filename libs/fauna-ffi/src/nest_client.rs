//! UniFFI handle for the per-actor WS-RPC connection (`fauna_client::NestClient`).
//!
//! This is the shared entry point every non-Rust UniFFI app (Apple,
//! Windows, Android) uses to reach the authenticated WS-RPC surface — the
//! analogue of the `Arc<NestClient>` the Rust-native Linux app builds in
//! `apps/fauna-linux/src/client.rs`. `NestClient` is `Send + Sync`, so this
//! wraps `Arc<NestClient>` directly and exports its async methods through
//! UniFFI's tokio runtime — no dedicated-worker-thread dance like
//! `mail_backup.rs` (whose coordinator is `!Sync`).
//!
//! ## Auth model
//!
//! `FfiNestClient::new` builds the client from the actor's ed25519 secret;
//! the WS connection then authenticates itself (silent challenge → bearer)
//! via the embedded `AuthClient`, independent of any HTTP bearer loop the
//! client already runs. That's the simplest correct shape at the FFI
//! boundary. Sharing a single bearer source between the HTTP and WS paths
//! (as Linux does via `LaunchMachineBearer`) is a possible future
//! optimization — it needs a UniFFI callback interface and isn't required
//! for correctness, since each WS handshake authenticates on its own.
//!
//! ## Lifecycle
//!
//! `new` → `connect` (authenticate + start the reconnect supervisor; blocks
//! until the WS reaches `Connected` or the bounded timeout fires) →
//! `bridges()` / `email()` typed-call clients → `disconnect`. The supervisor
//! handles token refresh and reconnection internally; `bridges()`/`email()`
//! are cheap to call repeatedly (each builds a thin wrapper over the shared
//! `Arc<NestClient>`).

#[cfg(feature = "conversations-session")]
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

#[cfg(feature = "conversations-session")]
use fauna_client::push::KindSubscriber;
use fauna_client::{ConnectionState, NestClient};
#[cfg(feature = "conversations-session")]
use fauna_client_conversations::{
    MailKeyCache, NestBridgedGlue, NestConversationsRpc, NestFolderGate, NestInboxDrainSource,
    NestMailInboundSource, NestOutboundMailSink, NestSchedulingSink, conv_push_source,
};
// Only `connection_state_label` below uses this, and that export is
// `value-format`-gated (see its doc comment) — so the import must be too, or the
// Go `--no-default-features` build warns unused and fails under `-D warnings`.
#[cfg(feature = "value-format")]
use fauna_core::localized::LocalizedText;
// The member content-key custody-ingest seam lives in `fauna-client-folders` (the
// read twin of its owner-side `FoldersAuthor`); `fauna-client-folders` depends on
// `fauna-client-conversations`, so the seam cannot live there without a cycle.
// `folders-author` is what pulls `fauna-client-folders/mls` (a default feature)
// and implies `conversations-session`, so the registration below is gated on it.
#[cfg(feature = "folders-author")]
use fauna_client_folders::NestFolderCustodySink;
#[cfg(feature = "conversations-session")]
use fauna_conversations::ConversationsSession;
#[cfg(feature = "conversations-session")]
use fauna_mls::engine::MlsEngine;
#[cfg(feature = "conversations-session")]
use fauna_protocol::PushEvent;
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};

use crate::FfiError;
use crate::account::FfiAccountClient;
use crate::admin::FfiAdminClient;
use crate::bluesky_client::FfiBlueskyClient;
use crate::bridges::FfiBridgesClient;
use crate::caldav_client::{FfiCaldavClient, SchedulingSessionHolder};
use crate::contacts_client::FfiContactsClient;
use crate::conversations_client::FfiConversationsClient;
use crate::email_client::FfiEmailClient;
use crate::family::FfiFamilyClient;
#[cfg(feature = "value-format")]
use crate::features::FfiFeaturesClient;
use crate::feed_client::FfiFeedClient;
use crate::folders_client::FfiFoldersClient;
use crate::inbox_client::FfiInboxClient;
use crate::moderation_client::FfiModerationClient;
use crate::nostr_client::FfiNostrBunkerClient;
#[cfg(feature = "zaps")]
use crate::nostr_client::FfiNostrZapSignerClient;
use crate::notifications_client::FfiNotificationsClient;
#[cfg(feature = "payments")]
use crate::payments_client::FfiPaymentsClient;
use crate::posts_client::FfiPostsClient;
use crate::push_client::{FfiPushClient, FfiPushRegistration};
use crate::search_client::FfiSearchClient;
use crate::snapshots_client::FfiSnapshotsClient;
use crate::spam::FfiSpamClient;
use crate::stats_client::FfiStatsClient;
use crate::subscriptions_client::FfiSubscriptionsClient;
use crate::sync_client::FfiSyncClient;

/// Bounded wait for the WS handshake inside [`FfiNestClient::connect`].
/// The reconnect supervisor keeps retrying past this; the timeout only
/// bounds the *first* connect so a bad URL surfaces as an error instead of
/// hanging the caller.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// FFI mirror of [`fauna_protocol::discovery::SetupStatusReply`] — the
/// setup-wizard progress the authed [`FfiNestClient::setup_status`] read
/// returns; the fields mirror the wire so every native app (macOS / iOS /
/// Android / Windows) can reuse the same read for its own status surfaces.
/// The wire's `extra` forward-compat
/// catch-all is dropped — it is a decode escape hatch, never a rendered value
/// (same discipline as the `admin` mirrors).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSetupStatus {
    pub domain: String,
    pub dns_configured: bool,
    pub tls_active: bool,
    pub email_enabled: bool,
    pub admin_exists: bool,
    pub claimed: bool,
    pub version: String,
    /// Whether the mail subsystem's nest-side config query is healthy — `false`
    /// surfaces the "bridge bricked while health says ok" outage
    /// class to clients (see [`SetupStatusReply::mail_subsystem_ok`]).
    pub mail_subsystem_ok: bool,
    /// Deployment policy: auto-enable mail for new users (default-on). The
    /// first-setup auto-mint glue gates on this **and** `email_enabled` (see
    /// [`SetupStatusReply::auto_enable_mail_for_new_users`]).
    pub auto_enable_mail_for_new_users: bool,
    /// Deployment policy: the nest's registration posture, as the
    /// [`fauna_protocol::node_policy::RegistrationMode`] wire string (`"open"` /
    /// `"invite_required"` / `"closed"`). The admin reads this to render
    /// `admin-users-registration-mode-select` and sets it via
    /// [`crate::admin::FfiAdminClient::set_registration_mode`] (see
    /// [`SetupStatusReply::registration_mode`]).
    ///
    /// Deliberately the **raw string**, not the enum: `None` (the field absent from the reply) and an unrecognized value (a *newer* nest's posture) are
    /// distinct states a client must not collapse into a guess, because saving
    /// that guess would overwrite the nest's real posture. Parse with
    /// [`crate::admin::registration_mode_from_wire`], which documents the
    /// contract.
    pub registration_mode: Option<String>,
    /// Deployment policy: the free-tier ceiling — `Some(v)` caps free-tier
    /// accounts at `v`, `None` is no cap. **Orthogonal to
    /// [`Self::registration_mode`]** (it applies regardless of posture), and the
    /// count includes the admin's own free-tier account. The admin reads this to
    /// render `admin-users-max-free-users-input` (blank = no cap) and sets it via
    /// [`crate::admin::FfiAdminClient::set_registration_mode`], which carries it
    /// alongside the mode (see [`SetupStatusReply::max_free_users`]).
    pub max_free_users: Option<u64>,
    /// Deployment policy: the client-set `subhandles` gate. The admin reads this
    /// to render the toggle and flips it via `fauna.admin.set_subhandles` (see
    /// [`SetupStatusReply::subhandles`]).
    pub subhandles: bool,
    /// Deployment policy: the "accept only signups carrying app age
    /// verification" gate (default-off; see
    /// [`SetupStatusReply::age_verification_required`]). The admin reads this
    /// to render the toggle and flips it via
    /// `fauna.admin.set_age_verification_required`.
    pub age_verification_required: bool,
    /// Deployment policy: the client-set node-wide storage cap, in bytes
    /// (`Some(v)` = cap, `None` = no limit). The admin reads this to render the
    /// field and sets/clears it via `fauna.admin.set_max_storage_bytes` (see
    /// [`SetupStatusReply::max_storage_bytes`]).
    pub max_storage_bytes: Option<u64>,
    /// Deployment policy: the client-set list of trusted browser origins for the
    /// nest's own HTTP API (empty = the built-in default origin only). The admin
    /// reads this to render the list and sets/clears it via
    /// `fauna.admin.set_cors_origins` (see [`SetupStatusReply::cors_origins`]).
    pub cors_origins: Vec<String>,
    /// Deployment policy: the admin's chosen client-facing API serving port
    /// (default 443). The admin reads this to render the field and sets it via
    /// `fauna.admin.set_serving_port` (see [`SetupStatusReply::serving_port`]).
    pub serving_port: u16,
    /// Deployment wiring: whether the nest is fronted by the SNI router
    /// (Docker/cloud). When `true` the admin client renders the serving-port field
    /// read-only — the port is the router's fixed 443 and a save is rejected (see
    /// [`SetupStatusReply::fronted_by_router`]).
    pub fronted_by_router: bool,
    /// Host-OS maintenance: pending security updates on the host Ubuntu box. The
    /// admin client renders "N security updates pending" when `> 0`. `0` on a nest
    /// without the maintenance channel (dev / desktop nests). See
    /// [`SetupStatusReply::os_security_updates_pending`] + `installers/vps.md`
    /// § Host OS Maintenance.
    pub os_security_updates_pending: u32,
    /// Host-OS maintenance: whether the host has a pending reboot. The admin
    /// client renders "Restart pending — will restart automatically when idle"
    /// when `true` (see [`SetupStatusReply::os_reboot_pending`]).
    pub os_reboot_pending: bool,
    /// Host-OS maintenance: unix seconds the pending reboot was first observed
    /// (`None` when none). See [`SetupStatusReply::os_reboot_deferred_since`].
    pub os_reboot_deferred_since: Option<i64>,
    /// Host-OS maintenance: unix seconds the host last applied patches (`None`
    /// when never). See [`SetupStatusReply::os_last_patched_at`].
    pub os_last_patched_at: Option<i64>,
    /// The web-app origin choice's wire spelling (`bundled` / `central`, or a
    /// newer nest's mode) — see [`SetupStatusReply::web_app_origin`]. The
    /// `admin-nest-web-app-origin-*` section renders through
    /// [`crate::admin::admin_web_app_origin_status`], never from these three.
    pub web_app_origin: String,
    /// The exact address this nest's `/app/` redirects to; `None` while it
    /// serves the bundled app. See [`SetupStatusReply::web_app_origin_target`].
    pub web_app_origin_target: Option<String>,
    /// Central is chosen but the nest has no handle domain, so it serves
    /// bundled regardless. See [`SetupStatusReply::web_app_origin_domainless`].
    pub web_app_origin_domainless: bool,
}

impl From<SetupStatusReply> for FfiSetupStatus {
    fn from(r: SetupStatusReply) -> Self {
        FfiSetupStatus {
            domain: r.domain,
            dns_configured: r.dns_configured,
            tls_active: r.tls_active,
            email_enabled: r.email_enabled,
            admin_exists: r.admin_exists,
            claimed: r.claimed,
            version: r.version,
            mail_subsystem_ok: r.mail_subsystem_ok,
            auto_enable_mail_for_new_users: r.auto_enable_mail_for_new_users,
            registration_mode: r.registration_mode,
            max_free_users: r.max_free_users,
            subhandles: r.subhandles,
            age_verification_required: r.age_verification_required,
            max_storage_bytes: r.max_storage_bytes,
            cors_origins: r.cors_origins,
            serving_port: r.serving_port,
            fronted_by_router: r.fronted_by_router,
            os_security_updates_pending: r.os_security_updates_pending,
            os_reboot_pending: r.os_reboot_pending,
            os_reboot_deferred_since: r.os_reboot_deferred_since,
            os_last_patched_at: r.os_last_patched_at,
            web_app_origin: r.web_app_origin,
            web_app_origin_target: r.web_app_origin_target,
            web_app_origin_domainless: r.web_app_origin_domainless,
        }
    }
}

/// UniFFI handle wrapping a [`NestClient`] (one per actor session).
#[derive(uniffi::Object)]
pub struct FfiNestClient {
    nest: Arc<NestClient>,
    /// The active [`ConversationsSession`], stashed by [`Self::conversations_session`]
    /// at login and handed to every [`Self::caldav`] handle so the organizer
    /// scheduling dispatch (`FfiCaldavClient::invite_attendee`) can reach the
    /// mailbox-less WS-RPC iMIP rail (`ConversationsSession::deliver_scheduling_imip`)
    /// with **no per-app glue** — the send twin of the receive-side
    /// `NestSchedulingSink` the same factory wires. Shared `Arc<Mutex<…>>` so a
    /// `caldav()` taken before login still sees the session once it lands.
    scheduling_session: SchedulingSessionHolder,
    /// This login's content-index arm, stashed by [`Self::conversations_session`]
    /// so [`Self::attach_local_search_index`] can register the Search page's
    /// local arm afterwards — the same late-population contract as
    /// `scheduling_session` above, and for the same reason: `search_manager()`
    /// may be taken before login. `None` until a conversations session is built
    /// (there is no mail rail before that), which the attach reports honestly as
    /// *no local arm* rather than an error.
    #[cfg(feature = "conversations-session")]
    index_arm: crate::index_launch::IndexArmHolder,
    /// This login's member-side succession witness + its harvest log — the two
    /// halves [`Self::succession_witness_state_json`] renders
    /// (`succession-aftermath.md` § Propagation → *MLS groups*).
    ///
    /// A **second** handle to objects the session already holds: the session
    /// keeps the witness behind a `dyn SuccessionWitness`, which cannot answer
    /// `observation()`, and it never sees the harvest log at all. Stashed by
    /// [`Self::conversations_session`] like `scheduling_session` above, and
    /// `None` until then — which the state provider reports as `null` rather
    /// than as an empty report, because "no session yet" and "a session whose
    /// witness has seen nothing" are different readings.
    #[cfg(feature = "conversations-session")]
    succession_report: crate::succession_witness::SuccessionReportHolder,
    /// The post-succession aftermath sink most recently registered by
    /// [`crate::succession_aftermath::run_succession_aftermath`], reused as
    /// leg 3's (the `__mls` re-seal) progress target — see
    /// `crate::mls_sync_launch::mls_sync_launcher`. Late-populated like
    /// `scheduling_session`: the reseal reads this at the moment
    /// its own pass reports, which may run before, during or after the
    /// aftermath call that last set it, in either build order. Gated on
    /// `recovery-aftermath` — the feature that gates
    /// [`crate::succession_aftermath`] itself, which owns `FfiAftermathSink`.
    #[cfg(feature = "recovery-aftermath")]
    aftermath_sink: Arc<Mutex<Option<Arc<dyn crate::succession_aftermath::FfiAftermathSink>>>>,
    /// The account registry most recently handed to
    /// [`crate::succession_aftermath::run_succession_aftermath`] — the
    /// post-store-ready pass's registry half (the parked ceremony it drains),
    /// late-populated like `aftermath_sink` for the same either-order reason
    /// (`crate::succession_aftermath::LedgerPassSeams`).
    #[cfg(feature = "recovery-aftermath")]
    ledger_registry: Arc<Mutex<Option<Arc<crate::accounts_registry::FfiAccountRegistry>>>>,
    /// This connection's author-pump once-per-connect latch. The pump tick
    /// (`subscriptions_reconcile_once`) rebuilds its author per call, so the
    /// latch lives here, with the connection, or the once-per-connect pass
    /// would run on every 30 s tick.
    #[cfg(feature = "subscriptions-author")]
    subscriptions_connect_pass: fauna_client_subscriptions::ConnectPassLatch,
}

impl FfiNestClient {
    /// Clone of the inner `Arc<NestClient>` for sibling FFI modules that build
    /// shared machines over the same connection (e.g. `mail_admin`'s
    /// `build_*_machine` constructors). Plain (non-exported) — the `Arc` itself
    /// isn't an FFI type; callers stay inside the crate. Mirrors how `bridges()`
    /// clones `self.nest`. Gated to the `mail-admin` / `folders` /
    /// `backup-destinations` / `subscriptions-author` / `nostr-npub-confirm` /
    /// `spam-threshold-override` features (plus the others named on the `cfg`
    /// below) — its only callers
    /// are those config-owning-orchestration modules, so a `--no-default-features`
    /// (Go-bridge) build would otherwise flag it dead.
    #[cfg(any(
        feature = "mail-admin",
        feature = "folders",
        feature = "backup-destinations",
        feature = "subscriptions-author",
        feature = "conversations-session",
        feature = "host-address",
        feature = "sync-engine-host",
        feature = "nostr-npub-confirm",
        feature = "spam-threshold-override",
        feature = "recovery-ceremony",
        feature = "pairing",
        all(unix, feature = "sync-agent-provisioning")
    ))]
    pub(crate) fn nest_arc(&self) -> Arc<NestClient> {
        Arc::clone(&self.nest)
    }

    /// This connection's author-pump latch (see the field's doc).
    #[cfg(feature = "subscriptions-author")]
    pub(crate) fn subscriptions_connect_pass(
        &self,
    ) -> fauna_client_subscriptions::ConnectPassLatch {
        self.subscriptions_connect_pass.clone()
    }

    /// The conversations session [`Self::conversations_session`] stashed, if
    /// it has run — the share plane's advertisement rail and roster.
    #[cfg(feature = "p2p-share")]
    pub(crate) fn conversations_session_handle(
        &self,
    ) -> Option<Arc<fauna_conversations::ConversationsSession>> {
        self.scheduling_session.lock().ok()?.clone()
    }

    /// This login's live conversations `MlsEngine`, or `None` when the
    /// conversations rail was never brought up.
    ///
    /// **Read here rather than passed in, deliberately.** The succession sweep
    /// takes the *old* identity's engine, and MLS holds one engine per
    /// `mls_state.db` (`folders_author.rs` § "ONE MlsEngine per mls_state.db"):
    /// an app that handed one in could hand in a second engine over the same
    /// store, which is unrepresentable if the only route is the session this
    /// client already stashed. `None` is an ordinary answer — the sweep reports
    /// `SweepStatus::NoEngine`, never a failure — and it is the same read
    /// [`Self::sync_engine_host`] does for the bound-set seam.
    ///
    /// Spelled with the full `fauna_mls::engine::MlsEngine` path on purpose:
    /// the local `MlsEngine` import is gated on `conversations-session`, while
    /// the holder this reads through is not.
    #[cfg(feature = "recovery-ceremony")]
    pub(crate) fn conversations_engine(&self) -> Option<Arc<fauna_mls::engine::MlsEngine>> {
        self.scheduling_session
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.engine())
    }

    /// Register the post-succession aftermath sink — called by
    /// [`crate::succession_aftermath::run_succession_aftermath`], never by an
    /// app directly. Replaces whatever was registered before, which is what a
    /// subsequent sign-in's call wants. Plain (non-exported): the app supplies
    /// the sink through `run_succession_aftermath`'s own parameter, exactly as
    /// before this existed; this is the internal wiring that also makes leg 3
    /// (the `__mls` re-seal, built separately by
    /// `crate::mls_sync_launch::mls_sync_launcher`) report through it.
    #[cfg(feature = "recovery-aftermath")]
    pub(crate) fn set_aftermath_sink(
        &self,
        sink: Arc<dyn crate::succession_aftermath::FfiAftermathSink>,
    ) {
        *self.aftermath_sink.lock().unwrap() = Some(sink);
    }

    /// The post-store-ready pass's late-populated halves on this connection
    /// (`crate::succession_aftermath::LedgerPassSeams`).
    #[cfg(feature = "recovery-aftermath")]
    pub(crate) fn ledger_pass_seams(&self) -> crate::succession_aftermath::LedgerPassSeams {
        crate::succession_aftermath::LedgerPassSeams {
            sink: Arc::clone(&self.aftermath_sink),
            registry: Arc::clone(&self.ledger_registry),
        }
    }
}

// The desktop sync-agent provisioner factory (src/sync_agent_provisioning.rs).
// Own gated impl block so it stays out of every non-desktop / non-feature build.
// Gated `any(unix, windows)` to match the module it constructs: exporting
// `FfiSyncAgentProvisioner` without this factory would give C# a type it cannot
// construct — the dead-code warnings on `build`/the two adapters are exactly what
// that mistake looks like from the compiler's side.
#[cfg(all(any(unix, windows), feature = "sync-agent-provisioning"))]
#[uniffi::export]
impl FfiNestClient {
    /// Build the desktop sync-agent provisioner over this actor's authenticated
    /// connection (`sync-agent.md` § Control plane split + § Credential model,
    /// milestone A4). The returned object registers a `RenewBearer` device grant on
    /// the nest and runs the shared convergence loop over this user's agent
    /// endpoint (unix socket on macOS/linux, per-SID named pipe on windows);
    /// call `start()` at the post-auth hook and `unprovision()` on sign-out /
    /// account-switch. `identity_secret` (32 bytes) is used **in-app only** — to
    /// sign the renewal grant and derive the actor id — and is never sent to the
    /// agent (the agent is bearer-only).
    ///
    /// No content keys are provisioned: the agent resolves every set's keys from
    /// this account's custody itself (`sync-agent-credentials.md` § Credential
    /// model), and the loop provisions only an agent that advertises doing so.
    /// `predecessor_backup_keys` — resolve ONCE post-auth via
    /// `FfiAccountRegistry::predecessor_backup_keys` and pass the same list
    /// here and to any label-custody equivalent (`sync-agent.md` § Credential
    /// model → *Retired owner keys after an identity succession*); empty for
    /// every identity that never succeeded. `predecessor_actor_ids` is its
    /// attested-ids sibling — resolve via
    /// `FfiAccountRegistry::attested_predecessor_actor_ids` off THIS
    /// session's own actor (`account-data-taxonomy.md` § The generation
    /// machinery → *The source of `prior`*). `start_account_runtime` resolves
    /// the same walk itself, off the registry it is handed.
    ///
    /// `accounts` is that same registry, handed over as an object: with it
    /// the capability carries the retired keys **paired with their
    /// identities** (`SyncCapability::predecessor_keys_by_actor`), resolved
    /// in Rust off the one walk, so the agent offers a row a retired identity
    /// signed that identity's own root (`writer-signed-change-records.md`
    /// ruling (8)(c)). `None` sends no pairs — the agent then note-skips such
    /// a row, fail-closed.
    #[uniffi::method(default(accounts = None))]
    #[allow(clippy::too_many_arguments)] // UniFFI export — capability inputs + three Swift hooks
    pub fn sync_agent_provisioner(
        &self,
        identity_secret: Vec<u8>,
        backup_key: Vec<u8>,
        predecessor_backup_keys: Vec<Vec<u8>>,
        predecessor_actor_ids: Vec<Vec<u8>>,
        device_id: String,
        device_label: String,
        spawner: Arc<dyn crate::FfiAgentSpawner>,
        bearer_source: Arc<dyn crate::FfiProvisioningBearerSource>,
        reachability_observer: Option<Arc<dyn crate::FfiAgentReachabilityObserver>>,
        accounts: Option<Arc<crate::accounts_registry::FfiAccountRegistry>>,
    ) -> Result<Arc<crate::FfiSyncAgentProvisioner>, FfiError> {
        crate::FfiSyncAgentProvisioner::build(
            self.nest_arc(),
            identity_secret,
            backup_key,
            predecessor_backup_keys,
            predecessor_actor_ids,
            accounts.as_deref(),
            device_id,
            device_label,
            spawner,
            bearer_source,
            reachability_observer,
        )
    }
}

/// A reconnect-event subscription for the UniFFI apps — the UniFFI-friendly
/// twin of consuming the shared [`NestClient::subscribe_reconnects`] `watch`
/// directly (as the Rust-native Linux app does in `apps/fauna-linux`). The
/// native apps (windows/apple/android) drive a loop
/// `while let Some(_) = sub.next().await { /* re-fetch visible surfaces */ }`;
/// each [`next`](Self::next) resolves on a reconnect (a `Connected` transition
/// AFTER the first connect — the initial connect never bumps it). Built via
/// [`FfiNestClient::subscribe_reconnects`]. Single-consumer: serialize `next()`
/// calls from one task (each call locks the inner receiver).
#[derive(uniffi::Object)]
pub struct FfiReconnectSubscription {
    rx: tokio::sync::Mutex<watch::Receiver<u64>>,
}

impl FfiReconnectSubscription {
    pub(crate) fn new(rx: watch::Receiver<u64>) -> Arc<Self> {
        Arc::new(Self {
            rx: tokio::sync::Mutex::new(rx),
        })
    }
}

#[fauna_uniffi_async::export]
impl FfiReconnectSubscription {
    /// Await the next reconnect; returns the new reconnect counter on each bump,
    /// or `None` once the client is torn down (the `watch` sender dropped) so the
    /// consumer's `while let Some(_) = sub.next().await` loop terminates. On each
    /// `Some`, the client re-fetches its visible snapshot surfaces — the
    /// transport.md § Push events "application observers re-pull on reconnect"
    /// contract (the feed has no poll backstop, so it would otherwise stay stale
    /// after a reconnect until a manual refresh).
    pub async fn next(&self) -> Option<u64> {
        let mut rx = self.rx.lock().await;
        match rx.changed().await {
            Ok(()) => Some(*rx.borrow_and_update()),
            Err(_) => None,
        }
    }
}

/// Connection-state value for the UniFFI apps — the UniFFI-exported mirror of
/// [`fauna_client::ConnectionState`] (which is a plain enum, not `uniffi::Enum`).
/// The native apps (windows/apple/android) render their global
/// `connection-status` indicator off this, driven by
/// [`FfiConnectionStateSubscription`] — the same four states the transport
/// emits (a `Reconnecting` state is still deliberately not modelled; a *swap*
/// shows as `Connecting`, never an error).
///
/// `Unreachable` (added 2026-07-29) is the one that *is* an error reading: the
/// connection has failed to establish
/// `fauna_core::format::CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES` times
/// running, so the client stops calling the gap transient. Full rationale on
/// `fauna_ws_substrate::supervisor::ConnectionState::Unreachable`.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiConnectionState {
    Connecting,
    Connected,
    Disconnected,
    Unreachable,
}

/// UniFFI face of [`fauna_client::SessionEndingVerdict`] — a post-auth verdict
/// a signed-in session cannot survive, each routed by the apps to the launch
/// surface rather than a banner (`security.md` § Post-auth surfacing).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiSessionEndingVerdict {
    /// The nest's pinned deployment identity changed.
    NestIdentityChanged,
    /// This identity was succeeded (`fauna.auth.superseded`).
    Superseded,
    /// The nest stopped signing this identity in — suspended or removed.
    SignInRefused,
}

impl From<fauna_client::SessionEndingVerdict> for FfiSessionEndingVerdict {
    fn from(v: fauna_client::SessionEndingVerdict) -> Self {
        use fauna_client::SessionEndingVerdict as V;
        match v {
            V::NestIdentityChanged => Self::NestIdentityChanged,
            V::Superseded => Self::Superseded,
            V::SignInRefused => Self::SignInRefused,
        }
    }
}

impl From<ConnectionState> for FfiConnectionState {
    fn from(s: ConnectionState) -> Self {
        match s {
            ConnectionState::Connecting => FfiConnectionState::Connecting,
            ConnectionState::Connected => FfiConnectionState::Connected,
            ConnectionState::Disconnected => FfiConnectionState::Disconnected,
            ConnectionState::Unreachable => FfiConnectionState::Unreachable,
        }
    }
}

impl From<FfiConnectionState> for ConnectionState {
    /// The inverse, so the exports below can route through the shared enum's own
    /// [`ConnectionState::as_wire_word`] instead of each restating the mapping.
    fn from(s: FfiConnectionState) -> Self {
        match s {
            FfiConnectionState::Connecting => ConnectionState::Connecting,
            FfiConnectionState::Connected => ConnectionState::Connected,
            FfiConnectionState::Disconnected => ConnectionState::Disconnected,
            FfiConnectionState::Unreachable => ConnectionState::Unreachable,
        }
    }
}

/// Resolve an [`FfiConnectionState`] to its `connection-status` indicator label
/// — the UniFFI-exported mirror of [`fauna_core::format::connection_state_label`]
/// (already consumed by web over wasm as `connectionStateLabel`). Routes the
/// UniFFI apps (windows/apple/android) through the one state → key decision
/// instead of each hand-rolling its own match (transport.md § Connection-status
/// indicator, `## Implementation status today` gap 3).
///
/// **Gated `value-format` because it returns a `fauna_core` type.** A
/// `#[uniffi::export]` whose signature carries a cross-namespace type makes
/// `uniffi-bindgen-go` emit an uncompilable bare `import "fauna_core"`, and
/// `mod nest_client` is ungated, so without this the export reaches the Go
/// mail-bridge's `--no-default-features` binding and breaks its build — which
/// **no gate catches**, since `mail-bridge-ffi-check` only diffs the regenerated
/// bindings against the tracked ones and nothing compiles the Go bridge. Same
/// reason as `render` / `folders` / `mail-admin` (Cargo.toml `value-format`);
/// the feature is default-on, so the UniFFI apps this export exists for
/// (windows/apple/android) are unaffected.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn connection_state_label(state: FfiConnectionState) -> LocalizedText {
    fauna_core::format::connection_state_label(connection_state_word(state).as_str())
}

/// The lowercase wire word for a connection state — the string
/// [`crate::offline::offline_affordance`] takes as its `connection_state`.
///
/// Exists so a UniFFI app never hand-rolls the enum→word `match`. That copy is
/// not merely duplicative, it is **silently dangerous**: the offline gate's
/// ruling 3 reads an unrecognised word as *online* (deliberately — an older app
/// meeting a future state word must keep its controls live), so one stale or
/// mistyped arm ungates every online-only control on that app with nothing
/// failing. Routing through [`ConnectionState::as_wire_word`] makes the mapping
/// a compile-checked property of the enum instead.
///
/// Gated `value-format` for the same reason as its neighbours — it is only ever
/// wanted by the UI clients, and the Go mail-bridge build has no UI.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn connection_state_word(state: FfiConnectionState) -> String {
    ConnectionState::from(state).as_wire_word().to_string()
}

/// A connection-state subscription for the UniFFI apps — the UniFFI-friendly
/// twin of observing the shared [`NestClient::connection_state`] `watch`
/// directly (as the Rust-native Linux app does for its top-of-sidebar
/// `connection-status` indicator). The native apps (windows/apple/android)
/// drive a loop `while let Some(s) = sub.next().await { /* update the indicator
/// */ }`; the FIRST [`next`](Self::next) resolves immediately with the *current*
/// state (so the indicator seeds without a separate snapshot read + its
/// subscribe/read race), and each subsequent one resolves on a transition
/// (Connecting/Connected/Disconnected). Built via
/// [`FfiNestClient::subscribe_connection_state`]. Single-consumer: serialize
/// `next()` calls from one task (each call locks the inner receiver).
#[derive(uniffi::Object)]
pub struct FfiConnectionStateSubscription {
    rx: tokio::sync::Mutex<watch::Receiver<ConnectionState>>,
}

impl FfiConnectionStateSubscription {
    pub(crate) fn new(mut rx: watch::Receiver<ConnectionState>) -> Arc<Self> {
        // Mark the current value unseen so the first `next()` delivers it
        // immediately — the indicator must show the live state on subscribe,
        // not wait for the next transition (unlike the reconnect subscription,
        // which intentionally skips the initial connect).
        rx.mark_changed();
        Arc::new(Self {
            rx: tokio::sync::Mutex::new(rx),
        })
    }
}

#[fauna_uniffi_async::export]
impl FfiConnectionStateSubscription {
    /// Await the next connection-state value — the *current* state on the first
    /// call, then each transition — or `None` once the client is torn down (the
    /// `watch` sender dropped) so the consumer's
    /// `while let Some(s) = sub.next().await` loop terminates.
    pub async fn next(&self) -> Option<FfiConnectionState> {
        let mut rx = self.rx.lock().await;
        match rx.changed().await {
            Ok(()) => Some((*rx.borrow_and_update()).into()),
            Err(_) => None,
        }
    }
}

// ── knock pushes (gated `conversations-session`) ────────────────────────────
// Unlike the sibling `subscribe_reconnects` (ungated, so it lands in the Go
// mail-bridge `--no-default-features` bindings too), the knock seam is gated to
// the `conversations-session` feature — the same default-on / Go-build-off gate
// the conversations methods below use. The Go MTA/MDA bridge has no knocks roster
// and no OS-toast surface, so excluding it from `libs/fauna-mail-go` keeps that
// binding (a) free of an unused export and (b) untouched by this change. The
// native apps (windows/apple/android) build with default features, so they
// still get it. Its own `#[uniffi::export]` impl block (not a per-method `#[cfg]`)
// so the whole thing is cfg-stripped before the uniffi macro runs.

/// A decoded contact-request (knock) push, flattened for UniFFI.
#[cfg(feature = "conversations-session")]
#[derive(uniffi::Record)]
pub struct FfiKnock {
    /// Hex actor id of the knock sender.
    pub sender_id: String,
    /// The knocker's own message text (not a sentence to paint on its own —
    /// [`knock_text_for`] decides what a knock toast says).
    pub summary: String,
    /// The knock row's sentence as a catalog key plus data args — the same
    /// `notifications.row_knock` body the knock's notification row carries —
    /// or `None` (the toast falls back to the sender-named text, as for an
    /// unknown key; `behavior/notifications.md` § Localized body).
    #[uniffi(default = None)]
    pub body: Option<fauna_core::localized::LocalizedText>,
}

#[cfg(feature = "conversations-session")]
impl From<FfiKnock> for fauna_protocol::push_events::KnockPayload {
    fn from(k: FfiKnock) -> Self {
        fauna_protocol::push_events::KnockPayload {
            sender_id: k.sender_id,
            summary: k.summary,
            body: k.body.map(|b| {
                let mut wire = fauna_protocol::LocalizedText::new(b.key);
                wire.args = b.args.into_iter().collect();
                wire
            }),
            ..Default::default()
        }
    }
}

/// What an OS-level knock toast says — the knock row's own localized sentence
/// when the push carries one this build knows, else the knock toast's catalog
/// sentence naming the sender — decided once in shared Rust.
///
/// A **pure** function over one knock, the twin of `notification_text_for`:
/// the native apps call it from their `FfiKnockSubscription` loop, exactly as
/// the Rust-native linux app calls `fauna_client_notifications::knock_push_text`.
/// Paint the `Localized` arm through the app's own i18n pipeline.
#[cfg(feature = "conversations-session")]
#[uniffi::export]
pub fn knock_text_for(knock: FfiKnock) -> crate::FfiNotificationText {
    fauna_client_notifications::knock_push_text(&knock.into()).into()
}

/// A knock-push subscription for the UniFFI apps — the UniFFI-friendly twin of
/// consuming the shared broker's `subscribe_kind("fauna.knock")` directly (as the
/// Rust-native Linux app does in `apps/fauna-linux`, see `app.rs`
/// `PushEvent::Knock(p) => notify_knock`). The native apps (windows/apple/
/// android) drive a loop `while let Some(k) = sub.next().await { /* OS knock toast
/// + refresh the knocks roster */ }`; each [`next`](Self::next) resolves the next
/// decoded knock. Built via [`FfiNestClient::subscribe_knocks`]. Single-consumer:
/// serialize `next()` calls from one task (each call locks the inner receiver).
#[cfg(feature = "conversations-session")]
#[derive(uniffi::Object)]
pub struct FfiKnockSubscription {
    inner: tokio::sync::Mutex<KindSubscriber>,
}

#[cfg(feature = "conversations-session")]
impl FfiKnockSubscription {
    pub(crate) fn new(sub: KindSubscriber) -> Arc<Self> {
        Arc::new(Self {
            inner: tokio::sync::Mutex::new(sub),
        })
    }
}

#[cfg(feature = "conversations-session")]
#[fauna_uniffi_async::export]
impl FfiKnockSubscription {
    /// Await the next contact-request (knock) push, returning the decoded sender +
    /// summary, or `None` once the push source closes (broker dropped on client
    /// teardown) so the consumer's `while let Some(_) = sub.next().await` loop
    /// terminates. A transient lag (the consumer fell behind the broker's bounded
    /// channel) is skipped rather than surfaced — a missed knock is re-derivable
    /// from the roster refresh the consumer runs on every knock.
    pub async fn next(&self) -> Option<FfiKnock> {
        let mut sub = self.inner.lock().await;
        loop {
            match sub.recv().await {
                Ok(PushEvent::Knock(k)) => {
                    return Some(FfiKnock {
                        sender_id: k.sender_id,
                        summary: k.summary,
                        body: k
                            .body
                            .map(|b| fauna_core::localized::LocalizedText::key_args(b.key, b.args)),
                    });
                }
                // The KindSubscriber filters to "fauna.knock", so a non-Knock event
                // is unreachable; ignore defensively rather than mis-yield.
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// A decoded `fauna.notification` push, flattened for UniFFI.
///
/// The field-for-field mirror of `fauna_protocol::push_events::NotificationPayload`,
/// minus its `extra` forward-compat catch-all (a `BTreeMap<String, fauna_cbor::Value>`
/// UniFFI cannot express; a client that needs a future field reads it off the
/// re-fetched notification row, which is the authoritative record — the push is only
/// the hint that one arrived).
#[cfg(feature = "push-subscription")]
#[derive(uniffi::Record, Debug, Clone, PartialEq)]
pub struct FfiNotification {
    /// Nest-assigned row id of the unified notification record.
    pub notification_id: i64,
    /// Notification category (e.g. `"mention"`, `"reply"`, `"test"`) — the
    /// wire string of the shared `NotifType`. Classify it with
    /// `notification_glyph_for_type`; never match it in app code.
    pub notif_type: String,
    /// What produced it (e.g. `"knock"`, `"test-hooks"`).
    pub source: String,
    /// Hex actor id of the originating actor, when there is one.
    pub sender_id: Option<String>,
    /// Hex content id the notification points at, when there is one.
    pub content_id: Option<String>,
    /// Human-facing one-line summary the nest produced.
    pub summary: String,
    /// Unix **seconds**.
    pub timestamp: u64,
}

/// A decoded push event, flattened for UniFFI — the client-actionable subset of
/// [`fauna_protocol::PushEvent`], shaped for a `match` in client glue.
///
/// Deliberately **not** a 1:1 mirror of the full `PushEvent` enum. Two reasons:
/// several variants wrap cross-crate types uniffi-bindgen cannot emit (the
/// `bridge_routing::*` bridge pushes), and clients act on only a handful of
/// kinds — the rest are
/// no-ops in linux's central dispatch too (`app.rs`), or are handled *inside*
/// shared Rust (the conversations rail owns its own `subscribe_kind` for
/// `Welcome`/`ChannelMessage`/`MailReceived`, so joining here would double-spend
/// the MLS init key on the one shared engine).
///
/// Everything not modelled arrives as [`FfiPushEvent::Other`] carrying its wire
/// kind string, so the enum is **forward-compatible by construction**: a kind
/// added to `PushEvent` surfaces here as `Other` rather than silently vanishing
/// (transport.md § Schema and forward-compat discipline). Promoting a kind to its
/// own variant is then a one-line change here plus a `match` arm per client —
/// never a new FFI method and three new pumps.
#[cfg(feature = "push-subscription")]
#[derive(uniffi::Enum, Debug, Clone, PartialEq)]
pub enum FfiPushEvent {
    /// `fauna.notification` — re-fetch the notifications surface (and fire an OS
    /// toast, as linux does via `notify_unified(notif_type, summary)`).
    Notification { notification: FfiNotification },
    /// `fauna.account.update` — re-fetch the account snapshot. `changes` names the
    /// fields the nest touched; `timestamp` is unix **seconds**.
    AccountUpdated {
        changes: Vec<String>,
        timestamp: u64,
    },
    /// `fauna.calendar.changed` — a durable write landed in one of the actor's
    /// calendars (Fauna-app or external-MUA-via-MDA; put/delete/provision);
    /// re-fetch the events surface. Own-device fanout; best-effort nudge — the
    /// quick-appearance poll and the reconnect re-pull remain the backstop
    /// (transport.md § Push events, ratified 2026-07-17; the dead
    /// `fauna.event.rsvp` kind it superseded left the wire 2026-09-24 with the
    /// compat-remnant sweep — a push of that kind arrives as
    /// [`FfiPushEvent::Other`] and is ignored, like every unmodelled kind).
    CalendarChanged {
        actor_id: String,
        calendar_id: String,
    },
    /// `fauna.addressbook.changed` — the carddav twin of `CalendarChanged`: a
    /// durable card or book write landed in one of the actor's address books
    /// (a Fauna app or an external contacts app); a showing Address Book
    /// re-reads its books and the open book's cards
    /// ([`FfiStaleSurfaces::address_book`], `transport.md` § Push events).
    /// Own-device fanout; best-effort nudge.
    AddressBookChanged {
        actor_id: String,
        addressbook_id: String,
    },
    /// `fauna.protocol.resync_required` — the nest dropped `dropped_count` pushes
    /// under backpressure; sweep every visible snapshot surface (linux re-fetches
    /// knocks/contacts/notifications/account).
    ResyncRequired { dropped_count: u64 },
    /// `fauna.sync.changed` — a record landed in a folder the actor
    /// participates in; re-fetch that set's rendered listing/content off its
    /// rescan cadence instead of waiting out the interval
    /// (file-sync.md § Remote-change nudge). `folder` names which set
    /// changed; a client with no per-set filtering wired yet may do a
    /// blanket re-fetch instead (linux/tui nudge a resident local-mirror
    /// engine to pull — a shape android does not have; android's consumers
    /// re-fetch their own machine snapshot).
    ///
    /// `folder_hash` is the set's hash address — the one address a sealed
    /// set's nudge carries once its name leaves the nest (`folder` is then
    /// blank). Relay it to `pull_folder_now` as received, and match a row by
    /// [`sync_changed_names_set`], never by comparing `folder` alone.
    SyncChanged {
        folder: String,
        folder_hash: Option<Vec<u8>>,
    },
    /// Any other kind, carrying its wire kind string — the bridge/p2p/conversations
    /// kinds a UI client does not act on, plus any kind added since this enum was
    /// written. Ignore it (linux logs and ignores `PushEvent::Unknown` likewise).
    Other { kind: String },
}

/// Whether a [`FfiPushEvent::SyncChanged`] names the set this app knows as
/// `name` — by `folder_hash` when present, else by `folder`
/// (`SyncChangedPayload::names_set`, the one match every receiver makes). The
/// apps' expanded-row gates call this instead of comparing `folder`, which is
/// blank on a sealed set's nudge.
#[cfg(feature = "push-subscription")]
#[uniffi::export]
pub fn sync_changed_names_set(folder: String, folder_hash: Option<Vec<u8>>, name: String) -> bool {
    fauna_protocol::push_events::SyncChangedPayload {
        folder,
        folder_hash: folder_hash.map(fauna_protocol::ByteBuf::from),
        ..Default::default()
    }
    .names_set(&name)
}

#[cfg(feature = "push-subscription")]
impl From<PushEvent> for FfiPushEvent {
    fn from(ev: PushEvent) -> Self {
        // `kind()` is the wire-kind source of truth, so the `Other` arm cannot
        // drift out of sync with the enum it mirrors.
        let kind = ev.kind().to_string();
        match ev {
            PushEvent::Notification(p) => FfiPushEvent::Notification {
                notification: FfiNotification {
                    notification_id: p.notification_id,
                    notif_type: p.notif_type.as_wire().to_string(),
                    source: p.source,
                    sender_id: p.sender_id,
                    content_id: p.content_id,
                    summary: p.summary,
                    timestamp: p.timestamp,
                },
            },
            PushEvent::AccountUpdated(p) => FfiPushEvent::AccountUpdated {
                changes: p.changes,
                timestamp: p.timestamp,
            },
            PushEvent::CalendarChanged(p) => FfiPushEvent::CalendarChanged {
                actor_id: p.actor_id,
                calendar_id: p.calendar_id,
            },
            PushEvent::AddressBookChanged(p) => FfiPushEvent::AddressBookChanged {
                actor_id: p.actor_id,
                addressbook_id: p.addressbook_id,
            },
            PushEvent::ResyncRequired(p) => FfiPushEvent::ResyncRequired {
                dropped_count: p.dropped_count,
            },
            // W2.3 (account-data-plane.md § Workstreams)'s `scope` tag is deliberately NOT forwarded across the FFI:
            // its one reader is the account runtime's own push arm, which
            // subscribes to the same broker Rust-side
            // (`crate::account_runtime` → `with_session_wakes`;
            // `fauna_sync_engine::account_runtime::nudge_scope_for_push`), so
            // no shell needs the tag — a typed field here would be surface
            // with no reader, and a UniFFI field nobody uses costs a
            // Go/Kotlin regen on every app. Adding it later is additive, as
            // `AddressBookChanged` was promoted off `Other` with its first
            // page consumer; this is the wiring point.
            PushEvent::SyncChanged(p) => FfiPushEvent::SyncChanged {
                folder: p.folder,
                folder_hash: p.folder_hash.map(|h| h.into_vec()),
            },
            _ => FfiPushEvent::Other { kind },
        }
    }
}

/// Which client surfaces a push has made stale, flattened for UniFFI — the
/// native twin of [`fauna_protocol::StaleSurfaces`] (`transport.md` § Which
/// surfaces a push invalidates). A local mirror rather than a UniFFI derive on
/// the shared type directly: a `uniffi::Record` added to `fauna_protocol` or
/// `fauna_core` reaches the Go mail-bridge binding tree regardless of any
/// feature gating the *functions* that return it (the `OrphanedStoreDisplay`
/// lesson), so the boundary type lives here,
/// behind the same `push-subscription` feature the Go build already excludes.
#[cfg(feature = "push-subscription")]
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiStaleSurfaces {
    pub feed: bool,
    pub notifications: bool,
    pub knocks: bool,
    pub contacts: bool,
    pub account: bool,
    pub atproto: bool,
    pub events: bool,
    pub media: bool,
    /// The ward's supervision read — reconnect-only, like `feed`
    /// (`StaleSurfaces::family`). The three UniFFI apps fire that read from
    /// their own reconnect triggers today and may serve this flag instead.
    pub family: bool,
    /// The Address Book page's books and open book's cards — page-gated, like
    /// `media`: re-read only while the Address Book is showing, since a contacts
    /// app's first sync is one push per card (`StaleSurfaces::address_book`).
    pub address_book: bool,
}

#[cfg(feature = "push-subscription")]
impl From<fauna_protocol::StaleSurfaces> for FfiStaleSurfaces {
    fn from(s: fauna_protocol::StaleSurfaces) -> Self {
        Self {
            feed: s.feed,
            notifications: s.notifications,
            knocks: s.knocks,
            contacts: s.contacts,
            account: s.account,
            atproto: s.atproto,
            events: s.events,
            media: s.media,
            family: s.family,
            address_book: s.address_book,
        }
    }
}

/// Which surfaces a push `kind` has made stale — the UniFFI twin of
/// [`fauna_protocol::StaleSurfaces::for_kind`] and the wasm client's
/// `staleSurfacesForPushKind` (`transport.md` § Which surfaces a push
/// invalidates). android/windows/apple call this with the wire kind string
/// (directly for [`FfiPushEvent::Other`], or the kind a modeled variant is
/// known to carry) instead of hand-deriving a per-app refresh table. An
/// unrecognized kind answers all-`false`.
#[cfg(feature = "push-subscription")]
#[uniffi::export]
pub fn stale_surfaces_for_push_kind(kind: String) -> FfiStaleSurfaces {
    fauna_protocol::StaleSurfaces::for_kind(&kind).into()
}

/// The full reconnect sweep
/// ([`fauna_protocol::StaleSurfaces::on_reconnect`]) — every surface a dropped
/// push might have staled, since a reconnect resets the push `seq` to 0. Call
/// once on reconnect instead of hand-listing every surface to re-pull; the app
/// still owns folding these logical flags onto whatever it actually re-reads.
#[cfg(feature = "push-subscription")]
#[uniffi::export]
pub fn stale_surfaces_on_reconnect() -> FfiStaleSurfaces {
    fauna_protocol::StaleSurfaces::on_reconnect().into()
}

/// Which surfaces a decoded [`FfiPushEvent`] has made stale — the twin of
/// [`stale_surfaces_for_push_kind`] for a caller that already has the
/// flattened event rather than a bare kind string (`transport.md` § Which
/// surfaces a push invalidates). **Exhaustive over `FfiPushEvent`'s variants,
/// no wildcard arm** — the same guarantee [`fauna_protocol::PushEvent::
/// invalidates`] keeps for the untouched enum, so a variant promoted onto
/// `FfiPushEvent` later (today only `Other` carries its own kind string) is a
/// compile error here until this match is updated, rather than a client that
/// silently stops reacting to it.
#[cfg(feature = "push-subscription")]
#[uniffi::export]
pub fn stale_surfaces_for_push_event(event: &FfiPushEvent) -> FfiStaleSurfaces {
    let kind = match event {
        FfiPushEvent::Notification { .. } => "fauna.notification",
        FfiPushEvent::AccountUpdated { .. } => "fauna.account.update",
        FfiPushEvent::CalendarChanged { .. } => "fauna.calendar.changed",
        FfiPushEvent::AddressBookChanged { .. } => "fauna.addressbook.changed",
        FfiPushEvent::ResyncRequired { .. } => "fauna.protocol.resync_required",
        FfiPushEvent::SyncChanged { .. } => "fauna.sync.changed",
        FfiPushEvent::Other { kind } => {
            return fauna_protocol::StaleSurfaces::for_kind(kind).into();
        }
    };
    fauna_protocol::StaleSurfaces::for_kind(kind).into()
}

/// A generic push subscription for the UniFFI apps — the UniFFI-friendly twin
/// of consuming the shared [`NestClient::subscribe_pushes`] broadcast directly (as
/// the Rust-native Linux app does in `apps/fauna-linux/src/client.rs`, feeding
/// its central `app.rs` `WsEvent::Push(e) => match e { … }` dispatch). The native
/// apps (windows/apple/android) drive a loop
/// `while let Some(e) = sub.next().await { /* match e { … } re-fetch */ }`.
/// Built via [`FfiNestClient::subscribe_pushes`]. Single-consumer: serialize
/// `next()` calls from one task (each call locks the inner receiver).
///
/// Distinct from [`FfiKnockSubscription`], which stays as the dedicated
/// `fauna.knock` seam windows already consumes; a `fauna.knock` push arrives here
/// as [`FfiPushEvent::Other`]. A client should drive one or the other for knocks,
/// not both.
#[cfg(feature = "push-subscription")]
#[derive(uniffi::Object)]
pub struct FfiPushSubscription {
    rx: tokio::sync::Mutex<tokio::sync::broadcast::Receiver<PushEvent>>,
}

#[cfg(feature = "push-subscription")]
impl FfiPushSubscription {
    pub(crate) fn new(rx: tokio::sync::broadcast::Receiver<PushEvent>) -> Arc<Self> {
        Arc::new(Self {
            rx: tokio::sync::Mutex::new(rx),
        })
    }
}

#[cfg(feature = "push-subscription")]
#[fauna_uniffi_async::export]
impl FfiPushSubscription {
    /// Await the next push, decoded, or `None` once the push source closes (broker
    /// dropped on client teardown) so the consumer's
    /// `while let Some(_) = sub.next().await` loop terminates.
    ///
    /// A transient lag (this consumer fell behind the broker's bounded channel) is
    /// **skipped rather than surfaced** — the same call the knock seam makes, and
    /// sound for the same reason: a push is a *hint*, never the only path to a
    /// value (transport.md § Push events). Every consumer re-fetches on each push
    /// and re-pulls on `subscribe_reconnects`, so a skipped hint costs at most a
    /// delayed refresh, never a lost value.
    pub async fn next(&self) -> Option<FfiPushEvent> {
        let mut rx = self.rx.lock().await;
        loop {
            match rx.recv().await {
                Ok(ev) => return Some(FfiPushEvent::from(ev)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

// The money plane's accessor — gated on its OWN `impl` block for the same
// reason as the sibling blocks: `#[uniffi::export]` does not honor a
// method-level `#[cfg]`.
//
// This accessor is the whole reachable surface of the payments plane from a
// shell, so excising it (rather than leaving a vestigial constructor) is what
// makes `dynamic-features.md` § What "completely compiled away" means item 5 —
// no re-enable path — true of the artifact: a store-safe build's generated
// Swift/Kotlin/C# face has no `payments()` to call at all.
// The gated-feature plane's transparency read — its own `impl` block for the
// same reason as the sibling blocks: `#[uniffi::export]` does not honor a
// method-level `#[cfg]` (found the hard way here too, 2026-08-11 — a
// method-level cfg compiled fine in the default flavor and broke only the
// mail-bridge's `--no-default-features` build).
//
// Gated on `value-format` — which is about the module's LocalizedText returns,
// NOT about any registry member. The read answers for whichever members a build
// ships, so unlike `payments()` below this accessor survives every app
// flavor including store-safe; only the Go mail-bridge, which has no
// feature-limits surface at all, drops it.
#[cfg(feature = "value-format")]
#[uniffi::export]
impl FfiNestClient {
    /// Typed-call client for the `fauna.features.*` kinds over this connection
    /// — the gated-feature plane's transparency read (dynamic-features.md
    /// § Transparency & auditability).
    pub fn features(&self) -> Arc<FfiFeaturesClient> {
        FfiFeaturesClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the Status snapshot's node read (`fauna.nest.info`)
    /// over this connection — `ui/status.md` § State & data shape.
    pub fn status(&self) -> Arc<crate::status::FfiStatusClient> {
        crate::status::FfiStatusClient::from_nest(Arc::clone(&self.nest))
    }
}

#[cfg(feature = "payments")]
#[uniffi::export]
impl FfiNestClient {
    /// Typed-call client for the `fauna.payments.*` kinds (payment-provider
    /// config + claim-code redemption — monetization.md § Pillar 3) over
    /// this connection.
    pub fn payments(&self) -> Arc<FfiPaymentsClient> {
        FfiPaymentsClient::from_nest(Arc::clone(&self.nest))
    }
}

// The NIP-57 trust root's accessor — the `zaps` registry feature (a subset
// member of `payments`: `zaps = ["payments"]`, so this block exists only in
// builds that also have the money plane). Its own `impl` block for the same
// macro reason as `payments()` above, and for the same item-5 reason: with it
// excised, a Damus-flavor build's generated Swift/Kotlin/C# face has no
// `nostrZapSigners()` to call, so there is no re-enable path from a shell.
//
// `nostr_bunker()` deliberately stays in the ungated block above — NIP-46
// signing is not a registry member, and only the zap surfaces excise.
#[cfg(feature = "zaps")]
#[uniffi::export]
impl FfiNestClient {
    /// Typed-call client for the `fauna.nostr.zap_signers.*` kinds (the NIP-57
    /// zap trust root — `monetization.md` § Zap receipts — the trust model)
    /// over this connection. Deliberately its own plane rather than a row on
    /// the generic `fauna.bridges.*` surface, exactly like `nostr_bunker`.
    pub fn nostr_zap_signers(&self) -> Arc<FfiNostrZapSignerClient> {
        FfiNostrZapSignerClient::from_nest(Arc::clone(&self.nest))
    }
}

// Gated on its OWN `impl` block for the same reason as the sibling blocks below:
// `#[uniffi::export]` does not honor a method-level `#[cfg]`.
#[cfg(feature = "push-subscription")]
#[uniffi::export]
impl FfiNestClient {
    /// Subscribe to **all** inbound pushes on the authenticated socket — each
    /// [`next`](FfiPushSubscription::next) resolves the next decoded
    /// [`FfiPushEvent`], so the client drives one central dispatch and re-fetches
    /// the surface each kind touches (the UniFFI twin of linux's `app.rs`
    /// `WsEvent::Push` match; transport.md § Push events and `seq` numbering).
    /// Cheap; each call gets its own receiver off the long-lived broker, so the
    /// subscription survives reconnects.
    pub fn subscribe_pushes(&self) -> Arc<FfiPushSubscription> {
        FfiPushSubscription::new(self.nest.subscribe_pushes())
    }
}

// Gated on its OWN `impl` block, not on the method: `#[uniffi::export]` emits
// scaffolding for every method in the block it decorates and does not honor a
// method-level `#[cfg]`, so a gated method inside an ungated exported block still
// gets scaffolded — and the Go mail-bridge's `--no-default-features` build then
// fails to resolve the types the feature would have brought in. Same shape as the
// `drafts` / `conversations-session` blocks below.
#[cfg(feature = "sync-engine-host")]
#[uniffi::export]
impl FfiNestClient {
    /// Build the in-process **file-sync engine host** for this actor — the
    /// construct-run-drop engine work an app runs in its own process (photo /
    /// watched-directory ingress, `MacRestoreView` restore, per-file state and
    /// backlog reads; `sync-engine-deployments.md` § Apple apps — convergence
    /// design). It hosts **no resident sync engine**: residency is the external
    /// `fauna-sync-agent`'s (`sync-agent.md` § Control plane split). The client
    /// calls this once at login and holds the handle.
    ///
    /// Reuses this client's already-connected WS-RPC control plane and pairs it
    /// with a fresh self-authenticating HTTP client for the chunk transport. The
    /// actor's shared per-actor `MlsEngine` is taken from the conversations rail
    /// if it is up; if it is not, a **bound** (cross-user shared) set simply
    /// **fails closed** (never plaintext).
    ///
    /// `secret` is the 32-byte ed25519 secret (it derives the owner `BackupKey`
    /// that seals every owner-only chunk, and unseals the content-key
    /// custody on `fauna.state.folder-keys`); `device_id` is the 32-byte device id — the same id across all of
    /// this device's engines, so the nest sees one device rather than N;
    /// `state_dir` holds the per-set state DBs + `device.db`; `device_label` is
    /// what the nest's device list shows (`fauna-macos` / `fauna-ios` /
    /// `fauna-android`).
    ///
    /// `accounts` is the app's account registry, handed over as an object —
    /// as `start_account_runtime` takes it — so this seat resolves the
    /// account's attested predecessors and their retired owner keys in Rust,
    /// off one walk: a row a retired identity signed then verifies as this
    /// account's and opens under that identity's own root
    /// (`writer-signed-change-records.md` ruling (8)(b) source (ii), (8)(c)).
    /// `None` binds nothing: the host's engines prove the ids by the
    /// statement walk and open under no retired root.
    #[uniffi::method(default(accounts = None))]
    pub fn sync_engine_host(
        &self,
        secret: Vec<u8>,
        device_id: Vec<u8>,
        state_dir: String,
        device_label: String,
        accounts: Option<Arc<crate::accounts_registry::FfiAccountRegistry>>,
    ) -> Result<Arc<crate::FfiSyncEngineHost>, FfiError> {
        let secret: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::General {
                msg: "secret must be 32 bytes".into(),
            })?;
        let device_id: [u8; 32] =
            device_id
                .as_slice()
                .try_into()
                .map_err(|_| FfiError::General {
                    msg: "device_id must be 32 bytes".into(),
                })?;

        // The conversations rail's per-actor engine (ONE engine over one
        // mls_state.db, never two — `folders_author.rs`), if login has wired it:
        // an owner-only photo ingest needs no MLS, but a bound set reached via a
        // one-shot must fail closed rather than seal plaintext.
        let mls = self
            .scheduling_session
            .lock()
            .unwrap()
            .as_ref()
            .map(|session| session.engine());

        Ok(crate::FfiSyncEngineHost::start(
            crate::sync_engine_host::host_context(
                state_dir,
                secret,
                device_id,
                device_label,
                Arc::clone(&self.nest),
                mls,
                crate::account_runtime::folder_key_store(),
                accounts.map_or_else(Vec::new, |a| a.predecessor_seal_keys(secret)),
            ),
        ))
    }
}

#[cfg(feature = "drafts")]
#[uniffi::export]
impl FfiNestClient {
    /// Typed-call client for the `fauna.drafts.{get,put}` kinds (the `__drafts`
    /// reserved-folder compose-draft persistence v2 plane) over this connection.
    /// `secret` is the actor's 32-byte ed25519 seed — the at-rest `BackupKey` is
    /// derived from it inside `DraftsClient` (the seal key, NOT a signing key;
    /// drafts are owner-only). Construct one per actor; cheap (a transport handle +
    /// the derived key). The per-app legs restore on launch + save (debounced)
    /// after a compose change, off this shared seam. Gated like `subscribe_knocks`
    /// (own `#[uniffi::export]` block) so the Go bridge `--no-default-features`
    /// build drops it cleanly.
    pub fn drafts(&self, secret: Vec<u8>) -> Result<Arc<crate::FfiDraftsClient>, FfiError> {
        let keypair = crate::keypair_from_bytes(&secret)?;
        Ok(crate::FfiDraftsClient::from_nest(
            Arc::clone(&self.nest),
            &keypair,
        ))
    }

    /// Per-rail draft **autosync** — the canonical stateful wrapper over
    /// [`Self::drafts`]'s client (`fauna.drafts.{get,put}` + client-side seal under
    /// the owner's `BackupKey`, draft-persistence v2 `file-sync.md` § Drafts Sync).
    /// Wraps the shared `DraftsSync` (launch gate + last-saved baseline), so the
    /// caller builds it once at login and keeps the handle for the session; a fresh
    /// handle per call would re-close the gate. `rail` is `"conversations"` (later
    /// `"posts"` / `"events"`). The identity is taken from the live connection, so
    /// no secret is re-passed. The Kotlin/Swift glue reads
    /// `ConversationsManager.identityEpoch()`, calls `load()` on launch →
    /// `ConversationsManager.restoreDraftsAt(epoch, bytes)`, and a debounced
    /// `saveIfChanged(draftsSnapshotBytes())` after compose edits.
    pub fn drafts_sync(&self, rail: String) -> Arc<crate::FfiDraftsSync> {
        crate::FfiDraftsSync::from_nest(Arc::clone(&self.nest), rail)
    }

    /// The events-rail draft autosync, typed rather than raw bytes — the
    /// events twin of [`Self::drafts_sync`] (`rail = "events"`, pre-bound).
    /// The Events page has no manager to hold the canonical encoding on any
    /// app, so this face carries the record itself; see
    /// [`crate::FfiEventDraftsSync`]. Build once at login and hold for the
    /// session, exactly like `drafts_sync`.
    pub fn event_drafts(&self) -> Arc<crate::FfiEventDraftsSync> {
        crate::FfiEventDraftsSync::from_nest(Arc::clone(&self.nest))
    }
}

#[cfg(feature = "conversations-session")]
#[uniffi::export]
impl FfiNestClient {
    /// Subscribe to inbound contact-request (knock) pushes — each
    /// [`next`](FfiKnockSubscription::next) on the returned handle resolves the
    /// next decoded `fauna.knock`, so the client fires its OS knock toast +
    /// refreshes its knocks roster. The UniFFI-friendly twin of linux consuming
    /// the broker's `subscribe_kind("fauna.knock")` directly. Cheap; each call
    /// gets its own filtered receiver.
    pub fn subscribe_knocks(&self) -> Arc<FfiKnockSubscription> {
        FfiKnockSubscription::new(self.nest.subscribe_kind("fauna.knock"))
    }
}

#[uniffi::export]
impl FfiNestClient {
    /// Build a WS-RPC client for `nest_url` (e.g. `wss://nest.example`)
    /// using the actor's 32-byte ed25519 secret. Does not open the socket —
    /// call [`FfiNestClient::connect`].
    #[uniffi::constructor]
    pub fn new(nest_url: String, secret: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let keypair = crate::keypair_from_bytes(&secret)?;
        let nest = NestClient::new(nest_url, keypair);
        Ok(Arc::new(Self {
            nest,
            scheduling_session: Arc::new(Mutex::new(None)),
            #[cfg(feature = "conversations-session")]
            index_arm: Arc::new(Mutex::new(None)),
            #[cfg(feature = "conversations-session")]
            succession_report: Arc::new(Mutex::new(None)),
            #[cfg(feature = "recovery-aftermath")]
            aftermath_sink: Arc::new(Mutex::new(None)),
            #[cfg(feature = "recovery-aftermath")]
            ledger_registry: Arc::new(Mutex::new(None)),
            #[cfg(feature = "subscriptions-author")]
            subscriptions_connect_pass: Default::default(),
        }))
    }

    /// Typed-call client for the `fauna.account.*` / `fauna.quota.get` /
    /// `fauna.profile.handle.change` kinds over this connection.
    pub fn account(&self) -> Arc<FfiAccountClient> {
        FfiAccountClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.bridges.*` kinds over this
    /// connection. Cheap — a thin wrapper over the shared `NestClient`.
    pub fn bridges(&self) -> Arc<FfiBridgesClient> {
        FfiBridgesClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.admin.*` kinds (the admin surface
    /// the consolidated `admin-users` hub drives) over this connection. Cheap —
    /// a thin wrapper over the shared `NestClient`; all kinds are Admin-gated
    /// nest-side.
    pub fn admin(&self) -> Arc<FfiAdminClient> {
        FfiAdminClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.family.*` kinds over this connection
    /// (the Family surface + supervised indicator — family-safety.md).
    pub fn family(&self) -> Arc<FfiFamilyClient> {
        FfiFamilyClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.email.*` kinds over this connection.
    pub fn email(&self) -> Arc<FfiEmailClient> {
        FfiEmailClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.spam.*` preferences kinds (the
    /// Settings → Privacy spam/phishing thresholds)
    /// over this connection — the native-client seam onto the WS-RPC kinds the
    /// Rust-native Linux app calls `fauna_client_spam::SpamClient` for
    /// directly, replacing the `GET|PUT /api/v1/spam/preferences` HTTP twin.
    pub fn spam(&self) -> Arc<FfiSpamClient> {
        FfiSpamClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.subscriptions.*` kinds (tier CRUD,
    /// subscribe/unsubscribe, the author request queue, encrypted-mode key
    /// material) over this connection.
    pub fn subscriptions(&self) -> Arc<FfiSubscriptionsClient> {
        FfiSubscriptionsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.{knocks,contacts,inbox.mode}.*` kinds
    /// (knocks inbox, contact roster, inbox-acceptance policy) over this
    /// connection.
    pub fn contacts(&self) -> Arc<FfiContactsClient> {
        FfiContactsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.nostr.bunker.*` kinds (the NIP-46
    /// *Connected apps* roster — nostr.md § The nest as the user's NIP-46
    /// signer) over this connection.
    pub fn nostr_bunker(&self) -> Arc<FfiNostrBunkerClient> {
        FfiNostrBunkerClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.notifications.*` kinds (list /
    /// mark-read / unread count) over this connection.
    pub fn notifications(&self) -> Arc<FfiNotificationsClient> {
        FfiNotificationsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the fauna-native inbox delivery-queue kinds
    /// `fauna.inbox.{fetch,ack}` (the store-and-forward drain) over this
    /// connection. Caller-scoped — no `actor_id` param.
    pub fn inbox(&self) -> Arc<FfiInboxClient> {
        FfiInboxClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `bluesky.feed.thread` kind (the one
    /// protocol-unique consume-side Bluesky surface — the crossposted-post
    /// thread view the post-detail surface shows) over this connection,
    /// replacing the deleted `GET /api/v1/bluesky/{feed/thread,thread}` HTTP
    /// twins. Caller-scoped (the handler restores the caller's Bluesky OAuth
    /// agent). `User`-gated nest-side.
    pub fn bluesky(&self) -> Arc<FfiBlueskyClient> {
        FfiBlueskyClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the client-facing `fauna.moderation.*` kinds
    /// (queue read, training correction, report-/signal-sharing opt-ins) over
    /// this connection. Gated `User | Admin` nest-side.
    pub fn moderation(&self) -> Arc<FfiModerationClient> {
        FfiModerationClient::from_nest(Arc::clone(&self.nest))
    }

    /// Encrypted-CalDAV Events client (`fauna.bridges.*` calendar RPCs +
    /// client-side seal/unseal over the `bridge_caldav_*` store) — the
    /// Decision-B target the native-client Events page reads/writes, the same
    /// store the mail-bridge MDA serves to Apple Calendar. Replaces the legacy
    /// plaintext [`Self::calendars`] / [`Self::events`] path. Cheap — a thin
    /// wrapper over the shared `NestClient`.
    pub fn caldav(&self) -> Arc<FfiCaldavClient> {
        FfiCaldavClient::from_nest(Arc::clone(&self.nest), Arc::clone(&self.scheduling_session))
    }

    /// Encrypted-CardDAV Address Book client (`fauna.bridges.*` CardDAV RPCs +
    /// client-side unseal over the `bridge_carddav_*` store) — the read side the
    /// Contacts page's "Address Book" segment renders, the same store the
    /// mail-bridge MDA serves to Apple Contacts. Read-only (slice 4b). Cheap — a
    /// thin wrapper over the shared `NestClient`; no scheduling session needed.
    pub fn carddav(&self) -> Arc<crate::FfiCarddavClient> {
        crate::FfiCarddavClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.feed.*` kinds (feed CRUD, feed/local
    /// post queries, discovery contributors) over this connection.
    pub fn feed(&self) -> Arc<FfiFeedClient> {
        FfiFeedClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.posts.*` kinds (post create / get /
    /// interact) over this connection.
    pub fn posts(&self) -> Arc<FfiPostsClient> {
        #[cfg(feature = "conversations-session")]
        {
            // The shared arm holder rides along so `posts_create` can feed the
            // index trickle once a conversations session exists — the same
            // late-population contract as `attach_local_search_index`.
            FfiPostsClient::from_nest(Arc::clone(&self.nest), Arc::clone(&self.index_arm))
        }
        #[cfg(not(feature = "conversations-session"))]
        {
            FfiPostsClient::from_nest(Arc::clone(&self.nest))
        }
    }

    /// Typed-call client for the `fauna.search.query` kind (the search bar's
    /// full-text query) over this connection — the native-client seam onto the
    /// WS-RPC kind the Rust-native Linux app calls directly, replacing the
    /// `GET /api/v1/search` HTTP twin.
    pub fn search(&self) -> Arc<FfiSearchClient> {
        FfiSearchClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.push.{vapid_key,subscribe,unsubscribe}`
    /// kinds (per-device push-subscription management) over this connection —
    /// the native-client seam onto the WS-RPC kinds, replacing the deleted
    /// `/api/v1/push/{vapid-key,subscribe}` HTTP twins.
    pub fn push(&self) -> Arc<FfiPushClient> {
        FfiPushClient::from_nest(Arc::clone(&self.nest))
    }

    /// This install's push registration under the connection's actor — the
    /// shared `fauna_client_push::registration` machine. `intent_path` is the
    /// install-scoped intent file (never an account scope), `actor_id` the
    /// signed-in actor (hex), `device_id` the install's derived device id for
    /// that actor (`FfiAccountRegistry::device_id_for_actor`) — the id the rows
    /// are keyed under and the connection announces.
    pub fn push_registration(
        &self,
        intent_path: String,
        actor_id: String,
        device_id: String,
    ) -> Arc<FfiPushRegistration> {
        FfiPushRegistration::from_nest(Arc::clone(&self.nest), intent_path, actor_id, device_id)
    }

    /// Typed-call client for the `fauna.conversations.keypackage.{upload,count}`
    /// kinds (the Encryption-settings MLS key-package pool) over this connection
    /// — the native-client seam onto the WS-RPC kinds the Rust-native Linux app
    /// calls directly, replacing the `POST|GET /api/v1/keypackage/{actor}` HTTP
    /// twins (deleted at T8).
    pub fn conversations(&self) -> Arc<FfiConversationsClient> {
        FfiConversationsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.folders.*` kinds (the Devices /
    /// Backups page folder control plane: list / create / update / delete /
    /// devices / members / schedule) over this connection — the shared seam
    /// the native one-shot consumers (BackupManagementVM / SyncSettingsView /
    /// MenuBarView) migrate their `/api/v1/file-sets/` HTTP call-sites onto.
    pub fn folders(&self) -> Arc<FfiFoldersClient> {
        FfiFoldersClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.filesync.snapshot.*` kinds (the
    /// Backups page snapshot table + create / delete / prune / check) over
    /// this connection.
    pub fn snapshots(&self) -> Arc<FfiSnapshotsClient> {
        FfiSnapshotsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.stats.get` kind (the Backups stats
    /// popover's repository storage stats) over this connection.
    pub fn stats(&self) -> Arc<FfiStatsClient> {
        FfiStatsClient::from_nest(Arc::clone(&self.nest))
    }

    /// Typed-call client for the `fauna.sync.*` media/engine/backups kinds
    /// (register / changes / status / files / backup_status) over this
    /// connection — the shared seam the native apps migrate their
    /// `/api/v1/sync/` HTTP call-sites onto (S3b). The Devices-page surface
    /// (`devices.{list,delete}`) lives on `fauna-devices-machine` instead.
    //
    // NB: keep the literal slash-star sequence OUT of `///` doc comments on
    // UniFFI-exported items. Kotlin and Swift block comments NEST, so a `/*`
    // inside the emitted `/** … */` opens a nested comment that the doc's own
    // closing delimiter only half-closes — silently commenting out the rest of
    // the generated binding (this broke android + apple codegen once).
    pub fn sync(&self) -> Arc<FfiSyncClient> {
        FfiSyncClient::from_nest(Arc::clone(&self.nest))
    }

    /// Actor id (hex) this client authenticates as.
    pub fn actor_id_hex(&self) -> String {
        self.nest.actor_id_hex()
    }

    /// Subscribe to WS reconnects — each [`next`](FfiReconnectSubscription::next)
    /// on the returned handle resolves on a reconnect (a `Connected` after the
    /// first connect), so the client re-fetches its visible snapshot surfaces
    /// (transport.md § Push events: the reconnect re-pull contract; the feed has
    /// no poll backstop). The UniFFI-friendly twin of linux consuming
    /// [`NestClient::subscribe_reconnects`] directly. Cheap; each call gets its
    /// own receiver.
    pub fn subscribe_reconnects(&self) -> Arc<FfiReconnectSubscription> {
        FfiReconnectSubscription::new(self.nest.subscribe_reconnects())
    }

    /// Subscribe to connection-state changes — the first
    /// [`next`](FfiConnectionStateSubscription::next) yields the *current* state,
    /// then each transition (Connecting/Connected/Disconnected) — so the native
    /// app renders its global `connection-status` indicator live (and shows a
    /// Watchtower-swap gap as Connecting, never an error). The UniFFI-friendly
    /// twin of linux observing [`NestClient::connection_state`] directly. Cheap;
    /// each call gets its own receiver.
    pub fn subscribe_connection_state(&self) -> Arc<FfiConnectionStateSubscription> {
        FfiConnectionStateSubscription::new(self.nest.connection_state())
    }

    /// Why this client's supervisor stopped for good, when the reason is a
    /// session-ending verdict — read on a `Disconnected` from
    /// [`Self::subscribe_connection_state`] (the stop is recorded before that
    /// state is announced). `None` while the supervisor runs, and for any other
    /// stop (a clean close, a version skew, a mint fault), which stays the
    /// connection indicator's business. The UniFFI twin of tui's and linux's
    /// pumps reading `NestClient::supervisor_stop` through the one shared
    /// classifier, so no app matches wire codes of its own.
    pub fn session_ending_verdict(&self) -> Option<FfiSessionEndingVerdict> {
        self.nest
            .supervisor_stop()?
            .session_ending_verdict()
            .map(Into::into)
    }
}

// Gated `feed-manager` (default-on; OFF in the Go mail-bridge `--no-default-features`
// build) as its own `#[uniffi::export]` impl block so the whole thing is cfg-stripped
// *before* the uniffi macro runs — its `FfiFeedManager` return wraps the cross-crate
// `fauna_feed` snapshot types uniffi-bindgen-go can't emit. Same gating shape as the
// `conversations-session` block below. See fauna-ffi/Cargo.toml § feed-manager.
#[cfg(feature = "feed-manager")]
#[uniffi::export]
impl FfiNestClient {
    /// Build the shared, stateful Feed-page manager (`fauna_feed::FeedManager`)
    /// over this connection + the actor's 32-byte ed25519 `secret` (held for the
    /// manager's lifetime so `submit_post` can build + sign posts). The native
    /// twin of the Rust-native Linux app's `FeedManager<Arc<NestClient>>` and
    /// the web `fauna-wasm` `WasmFeedManager` (`docs/goal/ui/feed.md` § State &
    /// data shape). `secret: Vec<u8>` matches the `mls` / `backup_coordinator`
    /// sub-client constructors that also need the actor secret.
    pub fn feed_manager(&self, secret: Vec<u8>) -> Result<Arc<crate::FfiFeedManager>, FfiError> {
        let secret: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::General {
                msg: "secret must be 32 bytes".into(),
            })?;
        Ok(crate::FfiFeedManager::new(Arc::clone(&self.nest), secret))
    }
}

// Gated `search-manager` (default-on; OFF in the Go mail-bridge `--no-default-features`
// build) as its own `#[uniffi::export]` impl block so the whole thing is cfg-stripped
// *before* the uniffi macro runs — its `FfiSearchManager` return wraps the cross-crate
// `fauna_client_search` snapshot types uniffi-bindgen-go can't emit. Same gating shape
// as the `feed-manager` block above. See fauna-ffi/Cargo.toml § search-manager.
#[cfg(feature = "search-manager")]
#[uniffi::export]
impl FfiNestClient {
    /// Build the shared, stateful Search-page manager
    /// (`fauna_client_search::SearchManager`) over this connection. The native
    /// twin of the `SearchManager<Arc<NestClient>>` that tui and the Rust-native
    /// Linux app drive directly (`docs/goal/ui/search.md` § State & data
    /// shape).
    ///
    /// Takes **no actor secret** — unlike `feed_manager`, searching signs
    /// nothing — so it is infallible and needs no `Result`. The manager runs
    /// nest-only through this façade; registering the local sealed index
    /// (backend 2) is a separate leg, per `crate::search_manager`'s module docs.
    pub fn search_manager(&self) -> Arc<crate::FfiSearchManager> {
        crate::FfiSearchManager::new(Arc::clone(&self.nest))
    }
}

// Gated `profile-client` (default-on; OFF in the Go mail-bridge
// `--no-default-features` build) as its own `#[uniffi::export]` impl block so the
// whole accessor is cfg-stripped *before* the uniffi macro runs — a per-method
// `#[cfg]` inside the shared exported impl above still leaves uniffi emitting
// scaffolding that references the cfg'd-out `FfiProfileClient` type (same gating
// shape as the `feed-manager` block above + the `conversations-session` block
// below). See fauna-ffi/Cargo.toml § profile-client.
#[cfg(feature = "profile-client")]
#[uniffi::export]
impl FfiNestClient {
    /// Typed-call client for the `fauna.profile.{get,set}` kinds (the Profile
    /// page detail read + the owner's edit-form publish) over this connection —
    /// the native-client seam onto the shared `fauna_client_profile::ProfileClient`
    /// the Rust-native Linux app calls directly.
    pub fn profile(&self) -> Arc<crate::FfiProfileClient> {
        crate::FfiProfileClient::from_nest(Arc::clone(&self.nest))
    }
}

// Gated `task-delegation` (default-on; OFF in the Go mail-bridge
// `--no-default-features` build) as its own `#[uniffi::export]` impl block so the
// whole constructor is cfg-stripped *before* the uniffi macro runs — a per-method
// `#[cfg]` inside the shared exported impl above still leaves uniffi emitting
// scaffolding that references the cfg'd-out `FfiTaskDelegationView` type (same
// gating shape as the `feed-manager` / `profile-client` blocks). See
// fauna-ffi/Cargo.toml § task-delegation.
#[cfg(feature = "task-delegation")]
#[uniffi::export]
impl FfiNestClient {
    /// Build the shared Task-delegation surface view-model
    /// (`fauna_client_delegation::TaskDelegationView`) over this connection — the
    /// native twin of the wasm SPA's binding all seven apps render
    /// (`participants.md` § Task delegation; ui.yaml page `task-delegation`).
    ///
    /// `device_id` is the 32-byte device id, hex-encoded into this device's
    /// `ParticipantRef::Device` exactly as the lease loop heartbeats it
    /// (matching `backup_coordinator`'s encoding). `capability` says whether
    /// this client may ever be pinned as a runner (native desktops pass
    /// `Runner`; the mobiles pass `ViewerOnly`). The pins rest sealed on
    /// `fauna.state.delegation` in the account store of this process's
    /// runtime, so no key crosses the boundary.
    pub fn task_delegation_view_for_device(
        &self,
        device_id: Vec<u8>,
        capability: crate::FfiHeavyTaskCapability,
    ) -> Result<Arc<crate::FfiTaskDelegationView>, FfiError> {
        let device_id: [u8; 32] =
            device_id
                .as_slice()
                .try_into()
                .map_err(|_| FfiError::General {
                    msg: "device_id must be 32 bytes".into(),
                })?;
        let self_ref = fauna_core::data::ParticipantRef::Device {
            device_id: hex::encode(device_id),
        };
        let view = fauna_client_delegation::TaskDelegationView::for_nest(
            Arc::clone(&self.nest),
            self_ref,
            capability.into(),
        );
        Ok(crate::FfiTaskDelegationView::new(view))
    }
}

// Gated `conversations-session` (default-on; OFF in the Go mail-bridge
// `--no-default-features` build) as its own `#[uniffi::export]` impl block so the
// whole thing is cfg-stripped *before* the uniffi macro runs — a per-method
// `#[cfg]` inside the shared exported impl still leaves uniffi emitting scaffolding
// for the cfg'd-out method. The method's `fauna_conversations::ConversationsSession`
// return is a cross-crate UniFFI Object uniffi-bindgen-go would emit as an
// uncompilable bare `fauna_conversations` import; the bridge has no
// client-conversations surface. See fauna-ffi/Cargo.toml § conversations-session.
#[cfg(feature = "conversations-session")]
#[uniffi::export]
impl FfiNestClient {
    /// A dual-rail conversations session for the native UniFFI apps — the
    /// native twin of the wasm `fauna-wasm` wrapper's `with_conversations`. Wires
    /// **both** rails on a fresh manager over this one WS-RPC connection: the
    /// FaunaMls rail (E2E MLS DMs — `from_parts` builds a persistent MLS engine
    /// `MlsEngine::new` over `mls_db_path` + a [`NestConversationsRpc`]) and the
    /// SMTP rail ([`Self::register_smtp`] with a [`NestOutboundMailSink`] over
    /// `EmailClient`). It also wires the inbound push source ([`conv_push_source`],
    /// a `NestConversationsPush` unless e2e-suppressed) over the same
    /// connection so the session's `start_receive_loop` can subscribe the inbound
    /// `welcome.received` + `channel.message` pushes. It also registers the inbound
    /// **mail** read-feeds ([`Self::register_mail_receive`] with two
    /// [`NestMailInboundSource`]s — `INBOX` + `Sent` over `EmailClient`), so the
    /// same `start_receive_loop` ticker delivers inbound mail into the unified view.
    /// The returned [`ConversationsSession`] exposes the wired
    /// [`ConversationsManager`] (via `manager()`) — the single observable surface
    /// the client drives for snapshot / send / `send_new_thread` — plus the
    /// welcome-ingest / inbound-poll receive drivers and `start_receive_loop` (the
    /// detached receive task driving both conv and mail). All MLS + mail crypto
    /// stays in shared Rust (`docs/goal/ui/conversations.md` § Architectural rules
    /// #2, § Receiving into the conversations view); the SMTP sink + mail sources
    /// are crypto-free transport (the one `open_inbound_record` decrypt aside).
    ///
    /// `self_address` is the logged-in `<handle>@<domain>` (the FaunaMls
    /// self-handle, which derives `self_domain`, and the SMTP `From:`);
    /// `self_secret` is the 32-byte Ed25519 actor secret the MLS engine builds its
    /// credential + signer from.
    ///
    /// `index_lease_device` is this device's stable 32-byte sync device id — the
    /// **same** id the Devices page rosters and `backup_coordinator` /
    /// `task_delegation_view_for_device` take — which seats this login at the advisory
    /// `index` lease (`participants.md` § Coordination primitive → *The `index`
    /// kind under the lease*). It is the one input this factory cannot derive: a
    /// lease holder is a *device*, and a device id is app-owned state. `None` is a
    /// supported wiring, not a stub — the builder then runs uncoordinated exactly
    /// as it did before the lease existed; see [`crate::index_launch`]'s
    /// `index_launcher`. The phones pass `None`.
    ///
    /// `predecessor_backup_keys` — resolve ONCE post-auth via
    /// `FfiAccountRegistry::predecessor_backup_keys` and pass the same list
    /// here and to [`Self::sync_agent_provisioner`] (`succession-aftermath.md`
    /// § Re-key scope owns the `__mls` re-seal this feeds); empty for every
    /// identity that never succeeded, which costs nothing.
    ///
    /// `recording_device` (hex) is the app's recording device — the same id it
    /// passes [`crate::folders_author::folders_serve_set`]'s `device_id`. Unlike
    /// `index_lease_device` the phones pass it too: with it, the launch-time
    /// folder resume also finishes a served-set walk a crash or unsynced
    /// custody interrupted (`webdav-server.md` § Key model (c)); `None` (the
    /// UniFFI default, so an unwired caller compiles) leaves that walk to the
    /// next serve.
    #[uniffi::method(default(recording_device = None))]
    pub fn conversations_session(
        &self,
        self_address: String,
        self_secret: Vec<u8>,
        mls_db_path: String,
        index_lease_device: Option<Vec<u8>>,
        predecessor_backup_keys: Vec<Vec<u8>>,
        recording_device: Option<String>,
    ) -> Result<Arc<ConversationsSession>, FfiError> {
        self.build_conversations_session(
            None,
            self_address,
            self_secret,
            mls_db_path,
            index_lease_device,
            predecessor_backup_keys,
            recording_device,
        )
    }

    /// Same session, built over a manager the **caller already owns** rather than a
    /// fresh one — the native twin of linux `conv_backend::start_conversations_session`,
    /// which passes its process-wide `host::manager()` into
    /// [`ConversationsSession::from_manager`].
    ///
    /// Prefer this whenever the client holds a manager *before* login. Adopting the
    /// live manager registers the real FaunaMls + SMTP backends **onto it** (per-rail
    /// `register_backend` overwrites, so any pre-existing mock rail survives for the
    /// rails the session does not serve), instead of handing back a *different*
    /// manager the client must swap in — a swap that silently discards every thread
    /// the old manager held. Apple hit exactly that: `ConversationsVM.activate`
    /// replaced its manager at login, so e2e-injected threads vanished the moment the
    /// real session activated, with no error anywhere (`ingest_inbound` only fails
    /// when a rail has *no* backend; a swap fails nothing at all).
    ///
    /// `index_lease_device` is the same input [`Self::conversations_session`]
    /// documents; so are `predecessor_backup_keys` and `recording_device`.
    #[uniffi::method(default(recording_device = None))]
    #[allow(clippy::too_many_arguments)] // the UniFFI surface is positional; a record would break every caller
    pub fn conversations_session_over_manager(
        &self,
        manager: Arc<fauna_conversations::ConversationsManager>,
        self_address: String,
        self_secret: Vec<u8>,
        mls_db_path: String,
        index_lease_device: Option<Vec<u8>>,
        predecessor_backup_keys: Vec<Vec<u8>>,
        recording_device: Option<String>,
    ) -> Result<Arc<ConversationsSession>, FfiError> {
        self.build_conversations_session(
            Some(manager),
            self_address,
            self_secret,
            mls_db_path,
            index_lease_device,
            predecessor_backup_keys,
            recording_device,
        )
    }

    /// Release every account-scoped store this client holds, **before** the shell
    /// erases them. Idempotent, and safe to call when no session was ever built.
    ///
    /// <div class="warning">
    ///
    /// Call this on **sign-out, account switch and factory reset** — any path that
    /// is about to delete this actor's directories. It is the erase's precondition,
    /// not a courtesy.
    ///
    /// </div>
    ///
    /// **Why disposing the shell's handles is not enough.** The hand-over this
    /// performs already existed — [`Self::build_conversations_session`] does it
    /// when a *successor* is built — but nothing performed it when an account
    /// simply went away, so the last session of a login stayed open forever. A
    /// shell that "released" its session by dropping its wrapper released nothing:
    /// this client stashes the session in `scheduling_session` (the organizer iMIP
    /// send rail) and the manager in `index_arm` (the Search page's local arm), and
    /// both stashes outlive any number of dropped foreign wrappers. Waiting for the
    /// last `Arc` is not available as a mechanism either — they live in three
    /// languages, and the ones in a Swift or C# object graph are invisible to Rust
    /// (`storage.rs`'s [`SqliteStorage::retire`] doc states the same rule for the
    /// role lock).
    ///
    /// So this is **refcount-independent**: it hands the engine's role over
    /// explicitly, which closes `mls.db` and releases `mls.db.lock` no matter who
    /// still holds a pointer. Anything still holding one gets the typed
    /// `MlsStateRetired` refusal rather than a second writer — the posture that
    /// was already ruled for the successor case.
    ///
    /// **What it cost to not have this**: on Windows an open handle makes a file
    /// undeletable, so a sign-out's `remove_dir_all` aborted `os error 32` on the
    /// first actor scope and left the signed-out user's conversations *and* — the
    /// sweep never reaching the second root — their account store readable on disk,
    /// with the failure swallowed as a warning. POSIX `unlink` tolerates open
    /// handles, so linux/tui/apple deleted the directory and never noticed they
    /// were doing it around a live engine. This is the shared fix for all of them
    /// (`account-scoping.md` § Erasure follows scope; `principles.md` § The user
    /// always controls their data).
    //
    // Provenance. Deliberately a `//` line and NOT
    // part of the `///` run above: this item is UniFFI-EXPORTED, the UniFFI API
    // checksum covers docstrings, and the publish transform excises the  span —
    // so a provenance span inside the doc comment changes the published
    // docstring, and the curated tree then dies on a checksum-mismatch panic at
    // runtime init, which compiles clean and passes every build-shaped gate.
    pub fn release_account_scoped_stores(&self) {
        // The session first, so its manager is reachable: taking it out of the
        // stash is also what makes a second call a no-op.
        let session = self.scheduling_session.lock().unwrap().take();
        if let Some(session) = session {
            // Drops the rail registration AND retires the engine behind it —
            // flushing the provider snapshot, releasing the role lock, and
            // closing the database. The receive loop selects on the same retire
            // signal, so it leaves at the event rather than at its next tick.
            session.manager().retire_conversations_engine();
        }
        // The Search page's local arm holds the same manager; a live arm would
        // keep the content index's own handles open past the erase.
        #[cfg(feature = "conversations-session")]
        {
            *self.index_arm.lock().unwrap() = None;
        }
    }
}

#[cfg(feature = "conversations-session")]
impl FfiNestClient {
    /// The shared body of both `conversations_session*` entry points. `manager`
    /// `Some` ⇒ adopt it (`from_manager`); `None` ⇒ build a fresh one
    /// (`from_parts`, which is itself just `from_manager` over a new manager).
    #[allow(clippy::too_many_arguments)] // mirrors the two exported factories' inputs
    fn build_conversations_session(
        &self,
        manager: Option<Arc<fauna_conversations::ConversationsManager>>,
        self_address: String,
        self_secret: Vec<u8>,
        mls_db_path: String,
        index_lease_device: Option<Vec<u8>>,
        predecessor_backup_keys: Vec<Vec<u8>>,
        recording_device: Option<String>,
    ) -> Result<Arc<ConversationsSession>, FfiError> {
        // Validated up front, on every target, and a wrong length **fails the
        // call** rather than silently seating nothing (the same conversion `task_delegation_view_for_device` does).
        // Silently degrading to "no seat" is precisely the failure shape the
        // ruling rejected shape 2 over: an uncoordinated builder still indexes
        // everything, so the mistake would have no observable consequence to find
        // it by.
        let index_lease_device: Option<[u8; 32]> = index_lease_device
            .map(|id| {
                id.as_slice().try_into().map_err(|_| FfiError::General {
                    msg: "index_lease_device must be 32 bytes".into(),
                })
            })
            .transpose()?;
        // Same fail-loud discipline as `index_lease_device` just above: a
        // malformed entry here is an app-side bug (the caller resolved these
        // bytes itself, moments earlier, off its own account registry), and
        // silently dropping one would seat a successor's replica un-resealed
        // with nothing to observe the mistake by.
        let predecessor_backup_keys: Vec<fauna_client_mls_sync::BackupKey> =
            predecessor_backup_keys
                .into_iter()
                .map(|k| {
                    k.as_slice()
                        .try_into()
                        .map(fauna_client_mls_sync::BackupKey::from_bytes)
                        .map_err(|_| FfiError::General {
                            msg: "predecessor_backup_keys entries must be 32 bytes".into(),
                        })
                })
                .collect::<Result<_, _>>()?;
        let keypair = crate::keypair_from_bytes(&self_secret)?;
        let self_actor = keypair.actor_id();
        // ── Release BEFORE build: hand the conversations-engine role over ─────
        //
        // Every native app rebuilds this session for the same account on a
        // re-login, an account switch and a factory-reset re-onboard, and until
        // 2026-08-29 it did so **build-first**: `MlsEngine::new` below asked for
        // the `mls_state.db` role lock while the *previous* engine still held it,
        // so the successor was refused `ServedElsewhere` — "your conversations
        // are open in another instance of this app" — about an instance that was
        // this one. Measured on macOS as an order-dependent e2e failure (the
        // second real-conversations test in a module could not activate; either
        // test passed alone), but the user-visible twin is plain account
        // switching: A → B → A finds A's store held by A's own predecessor and
        // A's conversations rail silently dead, `activate`'s own "a failure
        // leaves Send disabled, never crashes the launch" comment being exactly
        // how it degrades unseen.
        //
        // Waiting for the predecessor's last `Arc` to drop is not the fix: those
        // references live in three languages at once — apple's
        // `ConversationsVM.session`, windows' `ConversationsManagerHost`, this
        // client's own `scheduling_session` stash below — and a release that
        // depends on counting them across an FFI boundary is a race, not a
        // design. So the release is **explicit, ordered, and here**, at the one
        // seam macOS / iOS / windows / android all route through, rather than
        // four shells each remembering (priority #2).
        //
        // Two halves, because there are two kinds of holder:
        //
        //  1. The caller's manager still has the previous rail registered, and
        //     the manager is a process-lifetime singleton on apple and windows —
        //     `clear_for_identity_change` deliberately preserves backends, so
        //     nothing else ever drops it. `retire_conversations_engine` drops the
        //     registration *and* releases the engine's role lock through
        //     `RailBackend::retire`, which is what makes the hand-over
        //     independent of every remaining `Arc`.
        //  2. This client's own `scheduling_session` stash (the organizer-side
        //     iMIP send rail) holds the previous session for its whole life. It
        //     is overwritten at the END of this function — long after the lock
        //     is asked for — so clear it here instead.
        //
        // Cross-process exclusivity is untouched: another *instance's* engine is
        // unreachable from this process, keeps its lock, and this build is still
        // refused honestly — which is the case that refusal was written for
        // (`account-data-plane.md` § Multi-instance concurrency).
        if let Some(m) = &manager {
            m.retire_conversations_engine();
        }
        *self.scheduling_session.lock().unwrap() = None;
        // Preserve the typed `ServedElsewhere` verdict across this boundary
        // (account-data-plane.md § Multi-instance concurrency, W5.6): a
        // caller-supplied manager is the one this app's UI is already bound
        // to (every app that reaches this seam — macOS/iOS *and* windows, whose
        // `NestRpcClient.BuildConversationsSessionAsync` builds OVER
        // `ConversationsManagerHost.Instance` rather than swapping in a fresh
        // one; linux/tui build the manager themselves and never reach it), so it
        // is the single place `error-message`'s honest standing refusal can
        // be armed — success clears it, `ServedElsewhere` sets it, any other
        // failure also clears it (mirrors linux's `app.rs` `AuthSuccess` arm
        // exactly; `manager` stays `None` in the `from_parts` fresh-manager
        // case, where nothing is yet observing the flag).
        let engine = match MlsEngine::new(keypair, Path::new(&mls_db_path)) {
            Ok(e) => {
                if let Some(m) = &manager {
                    m.set_engine_served_elsewhere(false);
                }
                e
            }
            Err(e) => {
                if let Some(m) = &manager {
                    m.set_engine_served_elsewhere(matches!(
                        &e,
                        fauna_mls::error::MlsError::ServedElsewhere
                    ));
                }
                return Err(FfiError::General {
                    msg: format!("mls engine init: {e}"),
                });
            }
        };
        let rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&self.nest)));
        // The inbound push source over the same connection — `start_receive_loop`
        // drives the welcome/channel-message subscriptions through it. `None` when
        // a test-capable build was told to suppress it (`FAUNA_E2E_SUPPRESS_CONV_PUSH`)
        // to force the durable inbox-apply drain backstop (layer-5 missed-push proof);
        // that read is compiled out of a production FFI flavor (convention 15).
        let push = conv_push_source(Arc::clone(&self.nest));
        let session = match manager {
            // Adopt the caller's live manager — linux's shape
            // (`conv_backend::start_conversations_session`). The real rails are
            // registered onto the manager the client is already observing, so nothing
            // it holds is lost and there is no manager to swap.
            Some(m) => ConversationsSession::from_manager(
                m,
                Arc::new(engine),
                rpc.clone(),
                self_address,
                self_actor,
                push,
            ),
            None => ConversationsSession::from_parts(
                Arc::new(engine),
                rpc.clone(),
                self_address,
                self_actor,
                push,
            ),
        };
        // Wire the home-nest link-preview seam (render-model.md § D4) with the SAME object
        // (`NestConversationsRpc` impls both `ConversationsRpc` and `LinkPreviewRpc`), so a
        // conversation bubble's bare-url `LinkPreview` resolves through the manager.
        session.manager().set_link_preview_rpc(rpc.clone());
        // The room plane's four nest-backed seams, the SAME object again,
        // registered as one bundle so no seam can be left out
        // (`fauna_conversations::backend::RoomSeams`): the floor-roster report
        // and read (`conversation-rooms.md` § The floor roster), and the
        // community class's generation read and ceremony (§ The three classes
        // → *Community*). The generation read's other half, the group-reception
        // keys, rests on the account plane and is registered at the
        // account-store-ready edge (`crate::account_runtime::
        // wire_conversation_seams`). Without the ceremony a community room
        // cannot be founded or joined; without the generation read its records
        // stay unopened.
        session.set_room_seams(fauna_conversations::backend::RoomSeams::from_rpc(&rpc));
        // Dual-rail: add the SMTP send rail over the same connection — the native
        // twin of the wasm wrapper's second `register_backend`.
        session.register_smtp(Arc::new(NestOutboundMailSink::new(Arc::clone(&self.nest))));
        // Mail receive: wire the `INBOX` + `Sent` read-feeds the session's
        // `start_receive_loop` polls (ticker + the `fauna.mail.received` wake). The
        // shared `fauna-client-conversations` `NestMailInboundSource` builds the pair
        // over one connection with a single lazily-derived key cache (only the first
        // poll loads the mail custody); it is what makes inbound mail — incl. the
        // detached iTIP-REPLY calendar merge — flow into the unified view on every
        // UniFFI app (macOS / iOS / Windows / Android), with no per-app glue.
        // ONE `MailKeyCache` for every MSEK-derived consumer this login wires —
        // the read-feeds here and the content-index launcher below — so the login
        // pays a single mail-custody read instead of one per consumer
        // (`inbox_and_sent_over` exists for exactly this; tui/linux wire it the
        // same way).
        let mail_keys =
            MailKeyCache::new(Arc::clone(&self.nest), crate::account_runtime::mail_store());
        let (inbox, sent) = NestMailInboundSource::inbox_and_sent_over(
            Arc::clone(&self.nest),
            Arc::clone(&mail_keys),
            session.manager().refused_changes(),
        );
        session.register_mail_receive(inbox, sent);
        // The bridged rail — one backend for every bridge serving the account,
        // third-party principals and the nest's in-process legs (Nostr) alike
        // (`conversations.md` § Where logic lives → *The `Bridged` adapter*).
        // The shared glue is both seams: it seals to the bridge's key and opens
        // under the SAME mail-key cache the read-feeds above use, so the MSEK
        // stays in shared Rust; the receive loop's ticker and the
        // `conversation_changed` push drive it — on every UniFFI app at once, as
        // tui and linux register it.
        let bridged = NestBridgedGlue::new(Arc::clone(&self.nest), Arc::clone(&mail_keys));
        session.register_bridged(bridged.clone(), bridged);
        // Scheduling drain: wire the calendar-apply sink for the mailbox-less
        // CalDAV iMIP rail. The loop drains every `WelcomeChannelKind::Scheduling`
        // channel to `NestSchedulingSink`, which applies the iMIP to the actor's
        // calendar via the shared `CalDavClient` (caldav-server.md § Server-side
        // auto-schedule, Half-1) — so a mailbox-less Fauna attendee receives invites
        // on every UniFFI app (macOS / iOS / Windows / Android) with no per-app
        // glue, exactly like the inbound-mail receive above.
        session.register_scheduling_sink(Arc::new(NestSchedulingSink::over(
            Arc::clone(&self.nest),
            Arc::clone(&mail_keys),
            Arc::downgrade(&session.manager()),
            session.manager().refused_changes(),
        )));
        // Durable inbox-apply backstop: the loop's ticker drains the per-actor
        // `fauna.inbox.*` queue, recovering a Welcome whose best-effort push was
        // missed (client offline at push time) — the missed-push delivery guarantee
        // (`api-layers.md` § Inbox & Messaging, layer 3). Holds a `Weak` session to
        // avoid the session→source cycle (the session owns the source). No per-app
        // glue — every UniFFI app (macOS / iOS / Windows / Android) inherits it.
        session.register_inbox_drain(Arc::new(NestInboxDrainSource::new(
            Arc::clone(&self.nest),
            Arc::downgrade(&session),
        )));
        // Recipient contact gate for cross-user shared folders: the receive rail
        // routes a `WelcomeChannelKind::Folder` welcome through `NestFolderGate`,
        // which reads the sharer's contact-status over `fauna.contacts.status` and
        // decides auto-join / knock-stage / suppress (`folders.md` § Sharing) — so
        // a shared set auto-appears for a contact and stays a pending-share for a
        // stranger, on every UniFFI app (macOS / iOS / Windows / Android) with no
        // per-app glue, exactly like the scheduling + inbox wiring above.
        session.register_folder_gate(Arc::new(NestFolderGate::new(Arc::clone(&self.nest))));
        // Member content-key custody ingest (Phase 0 — the read leg): on join and
        // on each rotation-commit receipt, the session fetches the owner's sealed
        // content-key envelope, opens it via the group epoch, and folds the
        // generations into this member's own folder-key custody — so a *member*
        // (not just the owner) can decrypt a shared set's content, on every UniFFI
        // app (macOS / iOS / Windows / Android) with no per-app glue. The
        // seam holds the folders client (fetch) + the folder-key store (persist);
        // the open runs in the session's engine (`folders.md` § Sharing). Gated
        // on `folders-author` (what pulls `fauna-client-folders/mls`, a default).
        // No app hop rides the ingest: its custody write is itself the
        // `state-fleet` nudge the desktop sync agent re-resolves on
        // (`on-demand-files.md` § Shared sets on a capability host → *One
        // mechanism*).
        #[cfg(feature = "folders-author")]
        session.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
            Arc::clone(&self.nest),
            crate::account_runtime::folder_key_store(),
        )));
        // The ACCOUNT-custody ceremony sink (T16 — a different plane from the
        // folder content-key custody above): received offers / accepts /
        // delivers / A7 receipts are captured into `fauna.state.custody-ceremony` and each moved
        // ceremony schedules a drive pass, all in shared Rust. Without it the
        // custody facet's consent cards never appear and receipts never fold on
        // any UniFFI app. The page re-reads the facet on its own load edge, so
        // there is no repaint nudge to deliver.
        #[cfg(feature = "custody")]
        fauna_client_custody::register_ceremony_sink(
            &session,
            Arc::clone(&self.nest),
            crate::crypto::secret32(&self_secret)?,
            crate::account_runtime::handle,
            Arc::new(|| {}),
        );
        // Stash the session so the organizer SEND side — `caldav().invite_attendee`
        // — can reach the mailbox-less WS-RPC iMIP rail
        // (`ConversationsSession::deliver_scheduling_imip`) with no per-app glue,
        // the send twin of the `NestSchedulingSink` receive wiring just above.
        *self.scheduling_session.lock().unwrap() = Some(Arc::clone(&session));
        // The session edge of the account-store conversations seams (read
        // positions, group-reception keys): a no-op until the store is
        // installed, whose own edge then wires the pair.
        #[cfg(feature = "account-runtime")]
        crate::account_runtime::wire_conversation_seams(&self.scheduling_session);
        // Cross-device MLS state-sync plane (`docs/goal/behavior/devices.md` §
        // Cross-device MLS group-state sync, slice 5) — the native (apple/windows/
        // android) leg. Build the launcher over the same nest connection + the
        // session's backend/manager/conv and inject it; `start_receive_loop` runs it
        // once, before the first poll, to restore the replica + inject the
        // device-owned-epoch gate/cursor + attach the debounced autosave (design §5
        // restore-before-first-poll). One injection, no per-app glue — all three
        // native legs inherit the plane exactly like the SMTP/mail/scheduling/inbox/
        // folder rails above (priority #2). A malformed secret leaves it unset and
        // the client single-device, exactly as linux `conv_backend.rs`.
        // Member-side in-group succession witness + its peer-anchor harvest
        // sweep (`succession-aftermath.md` § Propagation → *MLS groups*).
        // Without them every `GroupMetaMessage::Succession` this seat receives
        // degrades to the bare add: the audience of a real recovery ceremony
        // renders "a stranger joined" and the participant row keeps naming the
        // retired identity, with nothing anywhere reporting the omission.
        //
        // One injection, no per-app glue — the four FFI legs (macOS, iOS,
        // windows, android) inherit both exactly as they inherit the MLS
        // state-sync plane below. The policy, the anchors, the native dialer,
        // the sweep and the state renderer are all shared
        // (`fauna-client-recovery`); this supplies only the thread store the
        // anchors read handles from and the second handle the state provider
        // reads (the anchors' store is lent to the manager at the
        // account-store-ready edge, `conversation_seams::wire`).
        //
        // ⚠ The sweep is INJECTED, never spawned here: this is a synchronous
        // UniFFI export and `fauna-ffi` owns no fallback runtime on purpose
        // (`crate::account_runtime`), so `start_receive_loop` launches it first
        // in its own prologue, inside the runtime the app already drives it on.
        crate::succession_witness::wire(&session, Arc::clone(&self.nest), &self.succession_report);
        #[cfg(feature = "recovery-aftermath")]
        let mls_launcher = crate::mls_sync_launch::mls_sync_launcher(
            Arc::clone(&self.nest),
            &self_secret,
            session.backend(),
            session.manager(),
            predecessor_backup_keys,
            Arc::clone(&self.aftermath_sink),
            recording_device,
        );
        #[cfg(not(feature = "recovery-aftermath"))]
        let mls_launcher = crate::mls_sync_launch::mls_sync_launcher(
            Arc::clone(&self.nest),
            &self_secret,
            session.backend(),
            session.manager(),
            predecessor_backup_keys,
            recording_device,
        );
        if let Some(launcher) = mls_launcher {
            session.set_mls_sync_launcher(launcher);
        }
        // Content-index plane (`content-index.md` § Where the index is built) —
        // the native (apple/windows/android) leg, wired from one launcher because
        // one object holds the MSEK and therefore owns both directions.
        //
        // BUILD side, desktop targets only: injected like the MLS launcher above,
        // and for the same structural reason — `start_receive_loop` awaits it in
        // its async prologue and registers the observer it returns *before* the
        // first mail poll, so that launch's mailbox re-walk is indexed rather
        // than racing. Phones skip it per the ratified build-vs-query split
        // (`crate::index_launch::CLIENT_BUILDS_INDEX`), and skipping costs them
        // nothing they can see: the query side below is registered on every
        // target, so a phone searches the segments a desktop published and synced.
        //
        // QUERY side, every target: stash the launcher + the manager the hits
        // resolve against, for `attach_local_search_index`. Stashed rather than
        // registered here because the Search page's manager is built separately
        // (`search_manager()`), may be taken before login, and registering it is
        // async — the same reason `scheduling_session` is a late-populated holder.
        //
        // The advisory `index` lease seat rides the BUILD side, so it is built
        // only where a builder exists: a phone that passed an id anyway would
        // otherwise heartbeat as the runner-of-record for work it never does.
        // Gating it on the same constant the builder is gated on makes that
        // structural rather than a rule each of the four apps has to remember
        // (priority #1) — `CLIENT_BUILDS_INDEX` is already `false` on iOS/Android,
        // and this is what the ruling's "the phones pass `None`" means in code.
        //
        // `PluggedInDesktop`, always, and not a knob: no desktop ships an
        // AC-line monitor any more (the monitors went with the in-app backup
        // upload drivers at the slice-5 flip), so wiring a real power signal
        // into the index coordinator's `set_class` would be net-new plumbing,
        // a separate nice-to-have — and the unknown-power default across every seat
        // is deliberately *candidate*, so a lone desktop still builds rather than
        // standing down forever waiting for a plugged-in peer that does not exist
        // (`index_lease::IndexLeaseSeat::class`; tui/linux wire the same constant).
        let index_lease_seat = index_lease_device
            .filter(|_| crate::index_launch::CLIENT_BUILDS_INDEX)
            .map(|device_id| fauna_client_conversations::IndexLeaseSeat {
                device_id,
                class: fauna_core::delegation::ParticipantClass::PluggedInDesktop,
                pins: Arc::new(crate::account_runtime::handle_source()),
            });
        let index_launcher = crate::index_launch::index_launcher(
            Arc::clone(&self.nest),
            Arc::clone(&mail_keys),
            index_lease_seat,
        );
        if crate::index_launch::CLIENT_BUILDS_INDEX {
            // `.clone()` rather than `Arc::clone(&…)`: the setter takes an
            // `Arc<dyn IndexBuilderLauncher>`, and the turbofish-free
            // `Arc::clone` would infer `T = dyn …` and demand an already-coerced
            // argument. The method call resolves on the concrete type first, then
            // unsize-coerces at the argument position.
            session.set_index_builder_launcher(index_launcher.clone());
        }
        *self.index_arm.lock().unwrap() = Some(crate::index_launch::IndexArm {
            launcher: index_launcher,
            lookup: session.manager(),
        });
        Ok(session)
    }
}

/// The Search page's **local arm** (backend 2) — gated on both features it
/// bridges: the launcher comes from the `conversations-session` factory and the
/// manager is the `search-manager` façade.
#[cfg(all(feature = "conversations-session", feature = "search-manager"))]
#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Register this login's sealed local index on `manager`, so its Search page
    /// merges local rows with the nest's instead of running nest-only.
    ///
    /// Call once after [`Self::conversations_session`] and
    /// [`Self::search_manager`]; it is the FFI twin of the two lines tui runs at
    /// its post-auth hook (`apps/fauna-tui/src/search.rs` — `local_search_index`
    /// then `set_local_index`).
    ///
    /// **No key crosses the boundary.** The index is minted Rust-side by the
    /// launcher that holds the MSEK and handed to the manager as an opaque
    /// `LocalSearchIndex`; app glue never sees, supplies, or names one
    /// (`content-index.md` § Encryption posture). That is why this is a method on
    /// the client rather than a `set_local_index` taking a foreign trait.
    ///
    /// Returns whether an arm was registered. `false` is a **normal state, not an
    /// error** — no conversations session yet, or mail is not enabled on this
    /// actor (no `mail.msek`, so there is no sealed slice to open). The page
    /// renders the same either way: a missing local arm is *no local rows*
    /// (`ui/search.md` § The local/nest merge). A client that wants to reflect it
    /// in diagnostics can read `FfiSearchManager::has_local_index()`.
    pub async fn attach_local_search_index(&self, manager: Arc<crate::FfiSearchManager>) -> bool {
        // Clone the arm out before awaiting — the holder's `std::sync::Mutex`
        // guard is not `Send` and must never span the rail I/O below.
        let arm = self.index_arm.lock().unwrap().clone();
        let Some(arm) = arm else {
            return false;
        };
        // Register the **resolver** first, so this call is not one-shot: minting
        // the arm needs the MSEK, and a user who enables mail after login had
        // none when glue ran. Without this the page stayed nest-only for the
        // life of the process even once the builder was publishing that
        // session's mail (`content-index.md` § Ingest triggers, v1 → *An arm
        // attaches when its precondition arrives*); the manager now mints on the
        // first query that finds no arm.
        // `.clone()` rather than `Arc::clone(&…)` on the lookup, for the reason
        // the `set_index_builder_launcher` call above documents: the turbofish-free
        // `Arc::clone` infers `T` from the concrete argument and so blocks the
        // unsize coercion the `Arc<dyn MailContentLookup>` parameter needs.
        let resolver = fauna_client_conversations::LauncherLocalIndex::new(
            Arc::clone(&arm.launcher),
            arm.lookup.clone(),
        );
        manager.set_local_index_resolver(resolver);
        // The return value keeps its documented meaning — *is an arm registered
        // now* — so it still agrees with `has_local_index()`, which is the whole
        // point of the contract. `false` now means "not yet", where it used to
        // mean "not this process".
        match arm.launcher.local_search_index(arm.lookup).await {
            Some(local) => {
                manager.set_local_index(local);
                true
            }
            None => false,
        }
    }
}

#[cfg(all(test, feature = "conversations-session"))]
mod index_arm_tests {
    use super::*;

    /// Building a conversations session must stash this login's index arm — the
    /// one step `attach_local_search_index` cannot recover from if it is missed,
    /// because a missing arm and an actor without mail both answer `false` and
    /// are indistinguishable from the app.
    ///
    /// Runs offline on purpose: the factory opens a local MLS store and registers
    /// rails over an unconnected client, so the wiring is reachable without a
    /// nest. Deleting the stash line reddens exactly this.
    #[test]
    fn building_a_conversations_session_stashes_the_index_arm() {
        let dir = std::env::temp_dir().join(format!("fauna-ffi-index-arm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("mls.sqlite");

        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![9u8; 32]).expect("build client");
        assert!(
            client.index_arm.lock().unwrap().is_none(),
            "precondition: no arm before login"
        );

        client
            .conversations_session(
                "someone@example.test".into(),
                vec![9u8; 32],
                db.to_string_lossy().into_owned(),
                None,
                Vec::new(),
                None,
            )
            .expect("build session");

        assert!(
            client.index_arm.lock().unwrap().is_some(),
            "the factory must stash the launcher + lookup, or the Search page's \
             local arm can never be registered on this client"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The FFI entry point every UniFFI app calls before erasing** — wired
    /// onto apple's sign-out and still owed on android — actually reaches all the way down
    /// to the database file, not just to the manager's rail registration.
    ///
    /// `storage.rs`'s `retire_closes_the_database_so_the_account_scope_can_be_removed`
    /// already pins `SqliteStorage::retire` in isolation, and
    /// `receive_cycle_poke_tests.rs` pins that `retire_conversations_engine`
    /// ends the receive loop — but nothing pinned the seam apple/android/
    /// windows actually call, so a regression in the hand-over between them
    /// (a missed `#[cfg]` arm, a stash that stops being populated) would
    /// compile clean and pass every existing test while an app kept leaking
    /// the engine again. This closes that gap: build a session the ordinary
    /// way, call the real exported method, and assert the engine's
    /// conversations-engine role lock — the property every platform actually
    /// enforces — is released, so every UniFFI host — apple, android,
    /// windows — shares this one witness instead of each needing its own.
    ///
    /// **Not a `remove_dir_all` witness.** `mls.db` is deliberately kept off
    /// WAL (`fauna_mls::storage`'s ratified tripwire), so it never grows
    /// `-wal`/`-shm` sidecars and their absence proves nothing here — and on
    /// POSIX `remove_dir_all` itself succeeds whether or not the connection
    /// ever closed (`unlink` tolerates open handles), so it is vacuous as a
    /// witness everywhere but Windows. The role lock is what's actually held
    /// only while the engine is live, on every platform: a fresh
    /// `try_lock` over [`fauna_mls::storage::role_lock_path`] fails while
    /// this engine holds it and only succeeds once
    /// `release_account_scoped_stores` has freed it. `remove_dir_all` is kept
    /// below anyway — it is the real user-facing property on Windows, where
    /// an open handle makes the file undeletable.
    #[test]
    fn release_account_scoped_stores_closes_the_engine_so_the_account_scope_can_be_removed() {
        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-release-scoped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("mls.db");

        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![6u8; 32]).expect("build client");
        // Kept for the whole test body, standing in for a foreign wrapper like windows'
        // `ConversationsManagerHost.Instance` — release_account_scoped_stores must free
        // the role lock even though this external reference outlives the call, or the
        // client's own stash-clears below would be the only references in play and the
        // regression this test exists to catch would go unnoticed.
        let _external_wrapper = client
            .conversations_session(
                "someone@example.test".into(),
                vec![6u8; 32],
                db.to_string_lossy().into_owned(),
                None,
                Vec::new(),
                None,
            )
            .expect("build session");
        assert!(
            db.exists(),
            "the session build must have created the database"
        );
        assert!(
            client.scheduling_session.lock().unwrap().is_some(),
            "precondition: building the session must stash it, or its clearing below \
             witnesses nothing"
        );

        let lock_path = fauna_mls::storage::role_lock_path(&db);
        assert!(
            fauna_core::fs_lock::open_lock_file(&lock_path)
                .expect("the engine's open must have minted the role lock file")
                .try_lock()
                .is_err(),
            "precondition: the conversations-engine role lock at {} must be held while \
             this engine is live, or its release below cannot be witnessed on any \
             platform",
            lock_path.display()
        );

        client.release_account_scoped_stores();

        assert!(
            client.scheduling_session.lock().unwrap().is_none(),
            "release_account_scoped_stores must take the session out of the stash \
             (also what makes a second call a no-op) — a leftover Some here is exactly \
             the regression this test exists to catch"
        );
        assert!(
            fauna_core::fs_lock::open_lock_file(&lock_path)
                .expect("the lock file is never unlinked, only released")
                .try_lock()
                .is_ok(),
            "release_account_scoped_stores must free the conversations-engine role lock \
             at {} no matter who still holds a stash or wrapper pointing at the engine \
             — this is the property every platform actually enforces, where a \
             `remove_dir_all` witness is vacuous on POSIX",
            lock_path.display()
        );

        std::fs::remove_dir_all(&dir).expect(
            "the account scope must be removable straight after \
             release_account_scoped_stores — this is the exact call every UniFFI app \
             (apple, android, windows) makes before erasing, and an open connection would \
             fail this the same way os error 32 did on windows before the release existed",
        );
        assert!(!dir.exists());
    }

    /// **The measured macOS defect, reduced to one offline test.** A second
    /// login in one process — over the same account store and the same
    /// process-lifetime manager — must build its conversations session, even
    /// while the previous session is still strongly held by whatever the app
    /// shell parked it in.
    ///
    /// This is the exact shape apple runs: each login builds a *new*
    /// `FaunaClient` (hence a new `FfiNestClient`) but reuses one
    /// `ConversationsVM.manager`, and `ConversationsVM.session` is only replaced
    /// *after* the new session finishes building. Before the release-before-build
    /// hand-over, the second `MlsEngine::new` asked for a role lock its own
    /// predecessor still held and the whole rail died with `ServedElsewhere` —
    /// "conversations are open in another instance of this app", about this very
    /// instance. Measured 2026-08-28 as the second real-conversations test in a
    /// module failing while either test passed alone; the user-visible twin is an
    /// A → B → A account switch.
    ///
    /// `previous` is held across the second build on purpose — dropping it would
    /// test a lifetime coincidence instead of the hand-over, and the coincidence
    /// is exactly what could not be relied on across the FFI boundary.
    #[test]
    fn a_second_login_over_one_manager_and_store_hands_the_engine_role_over() {
        let dir = std::env::temp_dir().join(format!("fauna-ffi-relogin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("mls.sqlite").to_string_lossy().into_owned();
        // The one process-lifetime manager both logins are built over — apple's
        // `ConversationsVM.manager`, windows' `ConversationsManagerHost.Instance`.
        let manager = Arc::new(fauna_conversations::ConversationsManager::new());

        let first_client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![4u8; 32]).expect("build client");
        let previous = first_client
            .conversations_session_over_manager(
                Arc::clone(&manager),
                "someone@example.test".into(),
                vec![4u8; 32],
                db.clone(),
                None,
                Vec::new(),
                None,
            )
            .expect("first login builds");

        // A *new* client, as every apple login makes — so the previous session's
        // release cannot come from this client's own stash.
        let second_client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![4u8; 32]).expect("build client");
        let result = second_client.conversations_session_over_manager(
            Arc::clone(&manager),
            "someone@example.test".into(),
            vec![4u8; 32],
            db,
            None,
            Vec::new(),
            None,
        );
        assert!(
            result.is_ok(),
            "the second login must take the conversations-engine role over from \
             the first, not be refused by it: {:?}",
            result.err()
        );
        assert!(
            !manager.engine_served_elsewhere(),
            "and the standing `error-message` refusal must not be armed — it \
             claims another *instance* is serving, which would be a lie here"
        );

        drop(previous);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A well-formed 32-byte predecessor `BackupKey` must not fail the build —
    /// the FFI call-site wiring
    /// (`mls_sync_launcher`'s `predecessors` param, no longer a bare
    /// `Default::default()`).
    #[test]
    fn conversations_session_accepts_a_well_formed_predecessor_backup_key() {
        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-predecessor-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![7u8; 32]).expect("build client");
        client
            .conversations_session(
                "someone@example.test".into(),
                vec![7u8; 32],
                dir.join("mls.sqlite").to_string_lossy().into_owned(),
                None,
                vec![vec![3u8; 32]],
                None,
            )
            .expect("a well-formed 32-byte predecessor key must not fail the build");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same fail-loud discipline as `index_lease_device`: a malformed
    /// predecessor key length must fail the call, not silently seat a
    /// successor's replica un-resealed with nothing to observe the mistake by.
    #[test]
    fn conversations_session_rejects_a_malformed_predecessor_backup_key() {
        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-predecessor-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![8u8; 32]).expect("build client");
        let result = client.conversations_session(
            "someone@example.test".into(),
            vec![8u8; 32],
            dir.join("mls.sqlite").to_string_lossy().into_owned(),
            None,
            vec![vec![1u8; 16]],
            None,
        );
        match result {
            Ok(_) => panic!("a 16-byte predecessor key must not be silently accepted"),
            Err(err) => assert!(
                format!("{err:?}").contains("32 bytes"),
                "the error must name the actual violation, got: {err:?}"
            ),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attaching before there is a session answers `false` rather than panicking
    /// or pretending — the honest-`false` contract app glue branches on, and the
    /// one arm of `attach_local_search_index` reachable without a nest.
    ///
    /// It matters because the *same* `false` also means "this actor has no mail",
    /// so a client must treat it as a normal state. A version that returned
    /// `true` optimistically would make `has_local_index()` disagree with the
    /// call that supposedly registered the arm.
    #[cfg(feature = "search-manager")]
    #[tokio::test]
    async fn attaching_before_login_registers_nothing_and_says_so() {
        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![3u8; 32]).expect("build client");
        let manager = client.search_manager();

        assert!(
            !client.attach_local_search_index(Arc::clone(&manager)).await,
            "no conversations session yet ⇒ no arm to register"
        );
        assert!(
            !manager.has_local_index(),
            "and the manager must agree — a `false` return with an arm attached \
             would be worse than either alone"
        );
    }

    /// **The pin that matters**: this build's *builder* and its Task-delegation
    /// *picker* must agree about the `index` kind.
    ///
    /// The two are compared against each other through **two independent
    /// readers** — the session the factory actually wired (`builds_content_index`,
    /// observed on a real session, not re-derived) and the capability mapping the
    /// picker consumes. Either one drifting alone reddens this, which is the
    /// defect class the 2026-08-03 picker session found: a per-(client, kind)
    /// rule encoded as independent bits rots silently in *both* directions — a
    /// client offering a self-pin for a kind it cannot run (linux's shipped
    /// `backup-upload` stranding) and one withholding a kind it can (tui's stale
    /// `ViewerOnly`).
    ///
    /// Deliberately **not** asserted against a hardcoded expectation: the answer
    /// legitimately differs by target (desktops build, phones query — module docs
    /// on `CLIENT_BUILDS_INDEX`), so a fixed `true` would just be a second place
    /// to update, and the property worth protecting is the agreement itself.
    #[cfg(feature = "task-delegation")]
    #[test]
    fn the_index_builder_and_the_index_pin_agree_on_this_build() {
        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-index-agree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let client =
            FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![5u8; 32]).expect("build client");
        let session = client
            .conversations_session(
                "someone@example.test".into(),
                vec![5u8; 32],
                dir.join("mls.sqlite").to_string_lossy().into_owned(),
                None,
                Vec::new(),
                None,
            )
            .expect("build session");

        let runs: fauna_core::delegation::HeavyTaskCapability =
            crate::task_delegation::FfiHeavyTaskCapability::IndexOnly.into();

        assert_eq!(
            session.builds_content_index(),
            runs.runs(fauna_core::delegation::KIND_INDEX),
            "a client that builds the index must be able to self-pin it, and one \
             that never builds must never be offered it — the picker would then \
             hand the user a pin nothing can ever honour"
        );
        // The old assertion here was the inverse — that the desktop arm still
        // declared `backup-upload`. It flipped with the slice-5 flip (2026-08-16):
        // no app ships an in-app upload driver, the source nest is the writer, and
        // the `Runner` arm that declared the kind is retired.
        assert!(
            !runs.runs(fauna_core::delegation::KIND_BACKUP_UPLOAD),
            "the `index` derivation must not resurrect the retired backup-upload \
             client declaration"
        );

        let viewer: fauna_core::delegation::HeavyTaskCapability =
            crate::task_delegation::FfiHeavyTaskCapability::ViewerOnly.into();
        assert!(
            !viewer.runs(fauna_core::delegation::KIND_INDEX),
            "a viewer-only client runs no heavy kind — querying the synced index \
             is not one and needs no pin"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Authenticate, start the reconnect supervisor, and wait until the WS
    /// reaches `Connected` (bounded by [`CONNECT_TIMEOUT`]). After this
    /// returns `Ok`, `bridges()`/`email()` calls can be issued.
    pub async fn connect(&self) -> Result<(), FfiError> {
        self.nest.connect().await.map_err(|e| e.to_string())?;

        let mut rx = self.nest.connection_state();
        let wait = async {
            loop {
                if *rx.borrow_and_update() == ConnectionState::Connected {
                    return Ok(());
                }
                if rx.changed().await.is_err() {
                    return Err("connection-state channel closed before connect".to_string());
                }
            }
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, wait).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(msg)) => Err(FfiError::General { msg }),
            Err(_) => Err(FfiError::General {
                msg: format!(
                    "WS connect did not reach Connected within {}s",
                    CONNECT_TIMEOUT.as_secs()
                ),
            }),
        }
    }

    /// Stop the reconnect supervisor and drop the connection. Pending RPCs
    /// see a disconnect error. The auth token is not cleared.
    pub async fn disconnect(&self) {
        self.nest.disconnect().await;
    }

    /// `fauna.setup.status` → [`FfiSetupStatus`] — the authed setup-wizard
    /// progress read (the authed twin of the onboarding machine's anonymous
    /// `probeSetupStatus`). The `admin-settings` page reads the reply's `mode`
    /// to render the read-only `nest-mode-indicator` (admin.md § 3 Settings).
    /// Issued directly via the `RpcRequester` (no dedicated `SetupClient`
    /// crate — a single replay-safe read, mirroring the web wasm passthrough
    /// `libs/fauna-wasm/src/rpc.rs::setup_status`).
    pub async fn setup_status(&self) -> Result<FfiSetupStatus, FfiError> {
        let reply: SetupStatusReply = self
            .nest
            .request("fauna.setup.status", SetupStatusRequest::default())
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply.into())
    }
}

// The W3 account-store runtime this app hosts (src/account_runtime.rs). Its own
// gated export block for the same reason the `conversations-session` one is: a
// per-method `#[cfg]` inside a shared exported impl still leaves uniffi emitting
// scaffolding for the stripped method.
#[cfg(feature = "account-runtime")]
#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Post-auth hook: host this account's W3 account-store runtime in **this**
    /// process — the windows/macOS/iOS seat of the assembly tui and linux
    /// already consume (`account-data-plane.md` § The account store → *The
    /// client-side lifecycle*).
    ///
    /// Returns as soon as the assembly is spawned. Every I/O-bound step (the
    /// T10 credential-slot read, the store open, the co-located agent's
    /// enrollment-target probe) sits inside phase 2 on a spawned task, so a
    /// wedged agent cannot hold up the sign-in that called this. A failed
    /// assembly is logged and leaves the app without a runtime; it never fails
    /// the login, so the only `Err` here is the one pre-flight refusal below.
    ///
    /// ## What the app supplies, and why these three
    ///
    /// Everything else the assembly needs it already holds or performs itself —
    /// these are the values no shared code can derive:
    ///
    /// * `app_data_dir` — **unread**: the assembly no longer takes the app's
    ///   own data dir (it was the device-local replica base, which
    ///   nothing reads since the succession ledger's cut). The parameter stays
    ///   only so the Swift/Kotlin/C# call sites keep their signature. ⚠ It is
    ///   **not the store root**: after W6 path unification the account store is
    ///   a *sibling* of the app's data dir under the per-user root, never a
    ///   child of it, which is also why an erase that iterates only that dir
    ///   misses the store (`account-scoping.md` § Erasure follows scope).
    /// * `store_container_dir` — `None` on windows and macOS, where the store
    ///   root is a per-OS constant the assembly resolves itself. A **sandboxed
    ///   shell** (iOS) passes its container, because there the per-app
    ///   container *is* the per-user root and the desktop derivation would
    ///   resolve a path inside the sandbox no sibling process can reach.
    /// * `own_device_id` — this machine's stable 32-byte sync device id, the
    ///   **same** id `conversations_session`'s `index_lease_device` seats at the
    ///   advisory `index` lease and the Devices page rosters. It decides which
    ///   `sync_devices` row this machine enrolls on, and getting it wrong
    ///   displaces a live renewer. `None` (a profile that has never registered
    ///   one) fails the runtime start: the machine's named row is the one
    ///   enrollment target.
    /// * `accounts` — the app's account registry. This seat resolves the
    ///   account's **attested** succeeded-from identities off it in Rust, for
    ///   THIS session's own actor — never the active account, which can
    ///   disagree during an append-mode sign-in or a switch race: their ids
    ///   are the fleet view's `prior` (`account-data-taxonomy.md` § The
    ///   generation machinery → *The source of `prior`*, ruled 2026-09-13),
    ///   and their delegable schedules are what the walk carries a
    ///   predecessor's preference rows under (`succession-aftermath.md`
    ///   § Re-key scope). The app passes the registry object rather than a
    ///   list: the schedules derive from seeds, which do not cross this call.
    ///   `None` attests nothing, which is fail-safe: a successor's
    ///   predecessor-signed enrollments drop out of this device's fleet view
    ///   and no predecessor row is carried; nothing is admitted.
    ///
    /// ## Ordering against `conversations_session` — there is none
    ///
    /// Call this before or after; both work, and neither is a contract. The
    /// membership source reads the client's stashed session on **every** pump
    /// pass, so a runtime started first answers *cannot tell* until the session
    /// lands and then starts answering — and an app with no conversations rail
    /// at all still hosts a perfectly correct runtime over its own-actor scopes.
    /// That independence is why this is a separate call rather than a parameter
    /// on the session factory.
    ///
    /// Calling it twice supersedes: the second assembly installs and the first
    /// runtime is shut down.
    pub async fn start_account_runtime(
        &self,
        app_data_dir: String,
        store_container: Option<crate::cloud_backup::FfiStoreContainer>,
        own_device_id: Option<Vec<u8>>,
        accounts: Option<Arc<crate::accounts_registry::FfiAccountRegistry>>,
    ) -> Result<(), FfiError> {
        // Kept for the exported signature only (see the doc above).
        let _ = app_data_dir;
        // Validated up front and a wrong length FAILS THE CALL rather than
        // silently starting with no row to enroll on — the same posture as `index_lease_device`, which
        // this is the same device id as. Degrading to `None` would hide a real
        // wiring bug behind a supported state.
        let own_device_id_hex = own_device_id
            .map(|id| {
                let bytes: [u8; 32] = id.as_slice().try_into().map_err(|_| FfiError::General {
                    msg: "own_device_id must be 32 bytes".into(),
                })?;
                Ok::<_, FfiError>(fauna_core::hex32::encode(&bytes))
            })
            .transpose()?;
        crate::account_runtime::install(
            Arc::clone(&self.nest),
            crate::account_runtime::HostInputs {
                // The container and its cloud-backup exclusion cross together
                // (`crate::cloud_backup::FfiStoreContainer`): a sandboxed shell
                // that names its container is the one party that can say how
                // it stays out of the backup its keychain rows are kept out of.
                store_container: store_container.map(|c| {
                    fauna_client_account_runtime::SandboxedStoreContainer {
                        dir: std::path::PathBuf::from(c.dir),
                        exclusion: c.exclusion.into(),
                    }
                }),
                own_device_id_hex,
                accounts,
                #[cfg(feature = "recovery-aftermath")]
                ledger_pass: self.ledger_pass_seams(),
            },
            crate::account_runtime::membership_source(Arc::clone(&self.scheduling_session)),
            Arc::clone(&self.scheduling_session),
        )
    }

    /// Teardown for an **account switch** — and for any other stop after
    /// which this account's credential slot survives (a factory-reset
    /// re-claim that keeps the identity, a nest-trust escalation): stop this
    /// process's account runtime deterministically, so it stops writing as the
    /// old account before the next one signs in. **The machine stays
    /// enrolled** — its writer key will be loaded again at the next sign-in
    /// as this account. A sign-out, after which the slot is erased, is
    /// [`Self::stop_account_runtime_for_sign_out`]; calling this one there
    /// leaves the machine's named row granted to a credential nobody holds
    /// (`sync-agent-credentials.md` § Credential model → *The signed-out
    /// reconcile*, the nest-side leg).
    ///
    /// Awaits the pump's in-flight pass, so by the time it returns nothing is
    /// still writing — which is what keeps a mid-pass sign-out from leaving the
    /// outbox half-drained. A plain **quit** must NOT call this: the process is
    /// ending, the handle drops, and the co-located agent (where there is one)
    /// takes the pump role over.
    ///
    /// Idempotent, and safe with no runtime installed.
    pub async fn stop_account_runtime(&self) {
        crate::account_runtime::teardown(fauna_client_account_runtime::StopReason::AccountSwitch)
            .await;
    }

    /// Teardown for a **sign-out** (and every reset that erases the
    /// credential namespace): [`Self::stop_account_runtime`] plus, first, the
    /// nest-side retirement of this machine's enrollment — while the runtime
    /// still holds the writer key and the shell still holds a session, both of
    /// which the erase that follows destroys. The nest forgets the credential,
    /// clears the named row's grant columns (keeping the row) and revokes
    /// every bearer it minted, so the next sign-in's fresh enrollment re-adopts
    /// the row rather than meeting a grant nobody holds. Best-effort and
    /// bounded; the stop runs regardless.
    ///
    /// Additive beside the switch-shaped stop (2026-09-14).
    pub async fn stop_account_runtime_for_sign_out(&self) {
        crate::account_runtime::teardown(fauna_client_account_runtime::StopReason::SignOut).await;
    }

    /// The Devices page's **standing enrollment notice** — the sentence to
    /// paint on Settings → Devices `error-message` while the nest refuses to
    /// enroll this machine, or `None` when nothing stands (`ui/devices.md`
    /// § Errors & edge cases; the one refusal today is the tier device cap,
    /// `devices.md` § Step 4). Read on every Devices hydrate, exactly as tui
    /// and linux read `AccountStoreHandle::enrollment_refusal`: the shared
    /// runtime records the refusal in the per-actor credential slot and
    /// `EnrollmentRefusal::notice` is the localized sentence — the same string
    /// `RpcError::localized` renders for the wire code — so no app composes
    /// its own copy. A local slot read; never a network call. `None` before
    /// the assembly lands (nothing to say yet, not an error).
    pub async fn account_enrollment_notice(&self) -> Option<String> {
        let handle = crate::account_runtime::handle()?;
        match handle.enrollment_refusal().await {
            Ok(refusal) => refusal.map(|r| r.notice().to_string()),
            Err(e) => {
                tracing::debug!("account_enrollment_notice: {e}");
                None
            }
        }
    }

    /// Run **one full account-pump pass now** — convention 14's `run_now`
    /// poke, the UniFFI apps' leg of the contract
    /// `fauna_e2e_agent::ACCOUNT_PUMP_NOW` owns, beside the
    /// `account_pump_cycles` counters this file publishes below.
    ///
    /// `AccountStoreHandle::reconcile_now` is *the ticker's own work on
    /// demand*, never a bypass — the same call tui's and linux's agent arms
    /// make — so a test that pokes and then waits on the counters observes
    /// exactly the production pass, which is what makes the barrier honest.
    ///
    /// **Awaited, not spawned, and that differs from tui/linux deliberately.**
    /// Their arms are fire-and-forget because a synchronous agent dispatch
    /// must not block its ack; every caller of this method is already an
    /// `async` UniFFI export whose own caller decides whether to await, so
    /// spawning here would only take the completion away from an app that
    /// wants it. The barrier stays the counters either way.
    ///
    /// No runtime yet (pre-auth, or an assembly that has not landed) is a
    /// legitimate quiet **no-op returning `false`** rather than an error:
    /// convention 11 is honoured (the command is not silently dropped — the
    /// caller learns it did nothing), and the consumer's own deadline poll on
    /// the counters is what fails, naming the app. `true` means a pass ran to
    /// completion.
    ///
    /// **Then one ceremony drive pass, as tui's arm does** — so an act the pass
    /// left owed (above all the A7 receipt the custody leg just minted, which
    /// only a drive posts to the owner) goes out without waiting for a
    /// production edge. Needed since these apps drive the host side of the
    /// custody ceremony (`ui/devices.md` § Custody facet, pieces 1 + 3): a
    /// minted-but-unposted receipt is exactly what a pass leaves owed, and the
    /// ceremony sink's own drive fires only on a *received* payload. Run
    /// whether or not the pass succeeded (tui's order); spawned, since the
    /// drive is fire-and-forget by design. Without a stashed conversations
    /// session the drive has no channel and returns at once.
    pub async fn account_pump_now(&self) -> bool {
        let Some(handle) = crate::account_runtime::handle() else {
            tracing::info!("account_pump_now: no account store yet (pre-auth)");
            return false;
        };
        let ran = match handle.reconcile_now().await {
            Ok(report) => {
                tracing::info!(?report, "account_pump_now: pass complete");
                true
            }
            Err(e) => {
                tracing::warn!("account_pump_now: {e}");
                false
            }
        };
        #[cfg(feature = "custody")]
        drive_custody_after_pump(&self.nest, &self.scheduling_session, handle);
        ran
    }
}

/// The ceremony drive [`FfiNestClient::account_pump_now`] chains — over that
/// connection, its own identity and the session the factory stashed. A free
/// function: a method in the exported impl block would be exported too.
#[cfg(all(feature = "custody", feature = "account-runtime"))]
fn drive_custody_after_pump(
    nest: &Arc<NestClient>,
    session: &crate::caldav_client::SchedulingSessionHolder,
    store: fauna_sync_engine::account_runtime::AccountStoreHandle,
) {
    let Some(secret) = nest.auth().keypair().map(|k| *k.secret_bytes()) else {
        tracing::info!("account_pump_now: no identity on this connection — no custody drive");
        return;
    };
    let session = session.lock().unwrap().clone();
    fauna_client_custody::spawn_drive(Arc::clone(nest), secret, session, Some(store));
}

/// The `device_set_state` e2e reader — the FFI twin of tui's `device_set_state`
/// automation arm (`apps/fauna-tui/src/automation.rs`) and linux's
/// `"device_set_state"` bridge command (`apps/fauna-linux/src/main.rs`) —
/// whether `device_id_hex`'s plane `fauna.state.device-set` row reads
/// Removed/Enrolled from THIS app's own account runtime
/// (`docs/goal/behavior/devices.md` § Removing a Device).
///
/// **Own impl block, own extra gate**, mirroring
/// [`FfiSyncAgentProvisioner::custodian_run_pass_now`]
/// (`sync_agent_provisioning.rs`): `account-runtime` alone gates the rest of
/// this surface, but the underlying
/// [`fauna_client_account_runtime::device_set_state_json`] carries its own
/// `debug_assertions OR e2e-agent` gate — a real plane-content read,
/// convention 15 rule (a). Reflecting that here as `any(test, feature =
/// "test-helpers")` rather than `debug_assertions` keeps an ordinary
/// non-test-helpers FFI debug build (e.g. an `apple-ffi-host` debug flavor)
/// free of a seam it doesn't need (testing.md convention 15); the crate-level
/// weak `fauna-client-account-runtime?/e2e-agent` forward on `test-helpers`
/// is what makes the underlying function present in a release-profile
/// `*-ffi-test` build.
//
#[cfg(all(feature = "account-runtime", any(test, feature = "test-helpers")))]
#[fauna_uniffi_async::export]
impl FfiNestClient {
    pub async fn device_set_state_json(&self, device_id_hex: String) -> String {
        fauna_client_account_runtime::device_set_state_json(
            crate::account_runtime::handle().as_ref(),
            &device_id_hex,
        )
        .await
        .to_string()
    }
}

// Sync half of the account-runtime surface — deliberately NOT in the async block
// above: convention 11's corollary is that the e2e state path does no blocking
// I/O, and this is a plain atomic read of two counters plus two booleans.
#[cfg(feature = "account-runtime")]
#[uniffi::export]
impl FfiNestClient {
    /// The `account_pump_cycles` e2e state value — this app's leg of the
    /// convention-11 contract `fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY` owns,
    /// beside the `account_pump_now` poke.
    ///
    /// The JSON shape is produced by the **shared**
    /// `fauna_client_account_runtime::account_pump_cycles_json`, so this app
    /// cannot publish a second shape of one cross-app contract (tui and linux
    /// read through the same function). Four states are distinguishable and
    /// conflating them is what made an earlier e2e fail on a healthy app:
    /// key absent = no leg at all; `runtime: false` = no assembled runtime;
    /// `runtime: true, holder: false` = assembled but another co-located
    /// process holds the W5.1 engine-singleton role, so these counters are
    /// frozen *correctly*; both true = assembled and pumping.
    pub fn account_pump_cycles_json(&self) -> String {
        fauna_client_account_runtime::account_pump_cycles_json(
            crate::account_runtime::handle().as_ref(),
        )
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The knock toast's façade decides exactly what the shared decision does
    /// over the wire payload it mirrors: a known key localizes with its args,
    /// a bodyless knock paints the toast's own sentence.
    #[cfg(feature = "conversations-session")]
    #[test]
    fn knock_text_for_matches_the_shared_decision() {
        let sender = "a1b2c3d4".repeat(8);
        let body = fauna_protocol::LocalizedText::new("notifications.row_knock")
            .with_arg("sender", &sender[..8])
            .with_arg("message", "hi");
        for body in [Some(body), None] {
            let wire = fauna_protocol::push_events::KnockPayload {
                sender_id: sender.clone(),
                summary: "hi".into(),
                body: body.clone(),
                ..Default::default()
            };
            let ffi = FfiKnock {
                sender_id: sender.clone(),
                summary: "hi".into(),
                body: body.map(|b| fauna_core::localized::LocalizedText::key_args(b.key, b.args)),
            };
            assert_eq!(
                knock_text_for(ffi),
                crate::FfiNotificationText::from(fauna_client_notifications::knock_push_text(
                    &wire
                ))
            );
        }
    }

    #[test]
    fn constructor_rejects_short_secret() {
        let err = FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![0u8; 16]);
        assert!(err.is_err());
    }

    #[cfg(feature = "feed-manager")]
    #[test]
    fn feed_manager_rejects_short_secret() {
        let nest = FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![7u8; 32]).unwrap();
        // A valid 32-byte secret builds a manager; a short one is rejected at the
        // boundary (mirrors `backup_coordinator`'s 32-byte validation).
        assert!(nest.feed_manager(vec![7u8; 32]).is_ok());
        assert!(nest.feed_manager(vec![0u8; 16]).is_err());
    }

    #[cfg(feature = "push-subscription")]
    #[test]
    fn stale_surfaces_for_push_event_matches_kind_for_every_modeled_variant() {
        let notification = FfiPushEvent::Notification {
            notification: FfiNotification {
                notification_id: 1,
                notif_type: "test".into(),
                source: "test-hooks".into(),
                sender_id: None,
                content_id: None,
                summary: "hi".into(),
                timestamp: 0,
            },
        };
        let cases = [
            (notification, "fauna.notification"),
            (
                FfiPushEvent::AccountUpdated {
                    changes: vec!["handle".into()],
                    timestamp: 0,
                },
                "fauna.account.update",
            ),
            (
                FfiPushEvent::CalendarChanged {
                    actor_id: "00".repeat(32),
                    calendar_id: "cal".into(),
                },
                "fauna.calendar.changed",
            ),
            (
                FfiPushEvent::AddressBookChanged {
                    actor_id: "00".repeat(32),
                    addressbook_id: "ab".repeat(32),
                },
                "fauna.addressbook.changed",
            ),
            (
                FfiPushEvent::ResyncRequired { dropped_count: 3 },
                "fauna.protocol.resync_required",
            ),
            (
                FfiPushEvent::SyncChanged {
                    folder: "Holiday Photos".into(),
                    folder_hash: None,
                },
                "fauna.sync.changed",
            ),
            (
                FfiPushEvent::Other {
                    kind: "com.acme.experimental".into(),
                },
                "com.acme.experimental",
            ),
        ];
        for (event, kind) in cases {
            assert_eq!(
                stale_surfaces_for_push_event(&event),
                stale_surfaces_for_push_kind(kind.to_string()),
                "stale_surfaces_for_push_event disagrees with stale_surfaces_for_push_kind for {kind:?}"
            );
        }
    }

    /// `fauna.addressbook.changed` reaches the UniFFI apps typed, not as
    /// `Other`, and flags the page-gated Address Book re-read.
    #[cfg(feature = "push-subscription")]
    #[test]
    fn addressbook_changed_is_typed_and_stales_the_address_book() {
        let ev =
            PushEvent::AddressBookChanged(fauna_protocol::push_events::AddressBookChangedPayload {
                actor_id: "11".repeat(32),
                addressbook_id: "ab".repeat(32),
                extra: Default::default(),
            });
        let ffi = FfiPushEvent::from(ev);
        assert_eq!(
            ffi,
            FfiPushEvent::AddressBookChanged {
                actor_id: "11".repeat(32),
                addressbook_id: "ab".repeat(32),
            }
        );
        let stale = stale_surfaces_for_push_event(&ffi);
        assert!(stale.address_book);
        assert_eq!(
            stale,
            FfiStaleSurfaces {
                address_book: true,
                ..FfiStaleSurfaces::from(fauna_protocol::StaleSurfaces::NONE)
            }
        );
    }

    #[test]
    fn setup_status_mirror_maps_mode_and_fields() {
        let reply = SetupStatusReply {
            domain: "nest.example".into(),
            dns_configured: true,
            tls_active: true,
            email_enabled: false,
            admin_exists: true,
            claimed: true,
            node_mode: "public".into(),
            version: "1.2.3".into(),
            mail_subsystem_ok: false,
            auto_enable_mail_for_new_users: false,
            registration_mode: None,
            max_free_users: None,
            subhandles: true,
            age_verification_required: true,
            max_storage_bytes: Some(8_000_000_000),
            cors_origins: vec!["https://app.example.com".into()],
            serving_port: 3443,
            fronted_by_router: true,
            dkim_records: Vec::new(),
            os_security_updates_pending: 3,
            os_reboot_pending: true,
            os_reboot_deferred_since: Some(1_719_500_000),
            os_last_patched_at: Some(1_719_400_000),
            web_app_origin: "bundled".into(),
            web_app_origin_target: None,
            web_app_origin_domainless: false,
            extra: Default::default(),
        };
        let ffi = FfiSetupStatus::from(reply);
        assert_eq!(ffi.domain, "nest.example");
        assert!(ffi.claimed);
        assert!(!ffi.email_enabled);
        assert_eq!(ffi.version, "1.2.3");
        // The new mail-health flag must survive the wire → FFI mirror.
        assert!(!ffi.mail_subsystem_ok);
        // As must the new-user auto-enable policy flag.
        assert!(!ffi.auto_enable_mail_for_new_users);
        // As must the age-verification require-knob (non-default in the
        // fixture, proving it is mirrored rather than defaulted).
        assert!(ffi.age_verification_required);
        // As must the client-set node-wide storage cap.
        assert_eq!(ffi.max_storage_bytes, Some(8_000_000_000));
        // As must the client-set CORS allow-list.
        assert_eq!(ffi.cors_origins, vec!["https://app.example.com"]);
        // As must the admin's chosen client-facing serving port.
        assert_eq!(ffi.serving_port, 3443);
        // As must the fronted-by-router wiring flag (gates the field read-only).
        assert!(ffi.fronted_by_router);
        // As must the host-OS-maintenance fields (the admin indicator reads them).
        assert_eq!(ffi.os_security_updates_pending, 3);
        assert!(ffi.os_reboot_pending);
        assert_eq!(ffi.os_reboot_deferred_since, Some(1_719_500_000));
        assert_eq!(ffi.os_last_patched_at, Some(1_719_400_000));
    }

    #[test]
    fn constructor_builds_and_vends_sub_clients() {
        // Smoke: prove the FFI surface compiles + wires against the real
        // NestClient. Round-trip conformance is the nest-side WS test
        // (`bins/fauna-nest/tests/conformance_{bridges_ui,email_filters}.rs`),
        // which the wrapper crates this forwards to already exercise.
        let client = FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![7u8; 32]).unwrap();
        let _ = client.bridges();
        let _ = client.email();
        let _ = client.subscriptions();
        #[cfg(feature = "payments")]
        let _ = client.payments();
        let _ = client.contacts();
        let _ = client.nostr_bunker();
        #[cfg(feature = "zaps")]
        let _ = client.nostr_zap_signers();
        let _ = client.notifications();
        let _ = client.feed();
        let _ = client.posts();
        let _ = client.snapshots();
        let _ = client.sync();
        let _ = client.search();
        let _ = client.push();
        assert!(!client.actor_id_hex().is_empty());
    }

    #[test]
    fn reconnect_subscription_yields_each_bump_then_none_on_drop() {
        use tokio::sync::watch;
        // The reconnect subscription is the UniFFI-friendly twin of consuming the
        // shared `NestClient::subscribe_reconnects()` watch directly (as linux
        // does): each `next()` resolves on a reconnect bump and returns the new
        // counter, and resolves `None` once the sender drops (client torn down) so
        // the consumer's `while let Some(_) = sub.next().await` loop terminates.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, rx) = watch::channel(0u64);
            let sub = FfiReconnectSubscription::new(rx);

            tx.send(1).unwrap();
            assert_eq!(sub.next().await, Some(1), "first reconnect bump");

            tx.send(2).unwrap();
            assert_eq!(sub.next().await, Some(2), "second reconnect bump");

            drop(tx);
            assert_eq!(sub.next().await, None, "sender dropped → loop terminates");
        });
    }

    #[cfg(feature = "conversations-session")]
    #[test]
    fn knock_subscription_yields_decoded_then_none_on_close() {
        use fauna_client::PushBroker;
        use fauna_protocol::PushEvent;
        use fauna_protocol::push_events::KnockPayload;
        use tokio::sync::broadcast;
        // FfiKnockSubscription is the UniFFI-friendly twin of consuming the shared
        // broker's `subscribe_kind("fauna.knock")` (as the Rust-native linux app
        // does): each `next()` resolves the decoded knock, and `None` once the push
        // source closes so the consumer's `while let Some(_) = sub.next().await` loop
        // terminates. Fed via the broker's public `bridge_from` (the only
        // out-of-crate path to push events in — mirrors push.rs's bridge test).
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let broker = PushBroker::new(16);
            let sub = FfiKnockSubscription::new(broker.subscribe_kind("fauna.knock"));
            let (src_tx, _) = broadcast::channel::<PushEvent>(16);
            let bridge = broker.bridge_from(src_tx.subscribe());

            src_tx
                .send(PushEvent::Knock(KnockPayload {
                    sender_id: "alice".into(),
                    summary: "hi".into(),
                    ..Default::default()
                }))
                .unwrap();
            let got = sub.next().await.expect("first knock");
            assert_eq!(got.sender_id, "alice");
            assert_eq!(got.summary, "hi");

            // Closing the source ends the bridge task → it drops its broker Arc;
            // dropping our handle drops the last sender → next() resolves None.
            drop(src_tx);
            drop(broker);
            let _ = bridge.await;
            assert!(
                sub.next().await.is_none(),
                "source closed → loop terminates"
            );
        });
    }

    #[cfg(feature = "push-subscription")]
    #[test]
    fn push_subscription_decodes_notification_then_none_on_close() {
        use fauna_client::PushBroker;
        use fauna_protocol::PushEvent;
        use fauna_protocol::push_events::NotificationPayload;
        use tokio::sync::broadcast;
        // FfiPushSubscription is the UniFFI-friendly twin of consuming the shared
        // `NestClient::subscribe_pushes()` broadcast (as the Rust-native linux app
        // does, feeding its central app.rs `WsEvent::Push` match). This pins the leg
        // windows/apple/android were blind to: a `fauna.notification` arrives decoded,
        // field-for-field, so the client re-fetches its notifications surface.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let broker = PushBroker::new(16);
            let sub = FfiPushSubscription::new(broker.subscribe());
            let (src_tx, _) = broadcast::channel::<PushEvent>(16);
            let bridge = broker.bridge_from(src_tx.subscribe());

            src_tx
                .send(PushEvent::Notification(NotificationPayload {
                    notification_id: 42,
                    notif_type: "mention".into(),
                    source: "test-hooks".into(),
                    sender_id: Some("alice".into()),
                    content_id: Some("beef".into()),
                    summary: "alice mentioned you".into(),
                    body: None,
                    timestamp: 1_700_000_000,
                    extra: Default::default(),
                }))
                .unwrap();

            match sub.next().await.expect("first push") {
                FfiPushEvent::Notification { notification } => {
                    assert_eq!(notification.notification_id, 42);
                    assert_eq!(notification.notif_type, "mention");
                    assert_eq!(notification.source, "test-hooks");
                    assert_eq!(notification.sender_id.as_deref(), Some("alice"));
                    assert_eq!(notification.content_id.as_deref(), Some("beef"));
                    assert_eq!(notification.summary, "alice mentioned you");
                    assert_eq!(notification.timestamp, 1_700_000_000);
                }
                other => panic!("expected Notification, got {other:?}"),
            }

            drop(src_tx);
            drop(broker);
            let _ = bridge.await;
            assert!(
                sub.next().await.is_none(),
                "source closed → loop terminates"
            );
        });
    }

    #[cfg(feature = "push-subscription")]
    #[test]
    fn push_subscription_maps_unmodelled_kind_to_other_carrying_its_wire_kind() {
        use fauna_protocol::PushEvent;
        use fauna_protocol::push_events::KnockPayload;
        // The forward-compat guarantee this enum is built on: a kind FfiPushEvent does
        // not model must arrive as `Other` carrying its wire kind string — never be
        // silently dropped. `fauna.knock` stands in for the whole unmodelled tail (the
        // bridge_routing/PeerSignal kinds uniffi cannot emit, plus any kind added to
        // PushEvent after this enum was written). If a future session promotes a kind
        // to its own variant, this test is what tells them the `Other` arm still holds
        // for everything else.
        let ev = FfiPushEvent::from(PushEvent::Knock(KnockPayload {
            sender_id: "alice".into(),
            summary: "hi".into(),
            ..Default::default()
        }));
        match ev {
            FfiPushEvent::Other { kind } => assert_eq!(
                kind, "fauna.knock",
                "Other must carry the wire kind string, so a client can log/ignore it"
            ),
            other => panic!("an unmodelled kind must map to Other, got {other:?}"),
        }
    }

    #[cfg(feature = "push-subscription")]
    #[test]
    fn sync_changed_flattens_with_its_folder_name() {
        use fauna_protocol::PushEvent;
        use fauna_protocol::push_events::SyncChangedPayload;
        // `fauna.sync.changed` (file-sync.md § Remote-change nudge) — android's
        // consumer is the FfiPushEvent::SyncChanged arm added alongside this test;
        // pins that the folder name survives the flatten, and that this kind no
        // longer falls into the Other catch-all above.
        let ev = FfiPushEvent::from(PushEvent::SyncChanged(SyncChangedPayload {
            folder: "photos".into(),
            scope: None,
            ..Default::default()
        }));
        assert_eq!(
            ev,
            FfiPushEvent::SyncChanged {
                folder: "photos".into(),
                folder_hash: None,
            }
        );
    }

    #[cfg(feature = "push-subscription")]
    #[test]
    fn push_subscription_passes_a_newer_nests_unknown_kind_through_as_other() {
        use fauna_protocol::PushEvent;
        use fauna_protocol::unknown::Unknown;
        // The version-compat path (version-compatibility.md: a client may be OLDER
        // than the nest it talks to). A kind this build has never heard of decodes to
        // `PushEvent::Unknown { kind, .. }`, whose `kind()` is the real wire string —
        // so it must reach the client as `Other` carrying that string verbatim, NOT
        // as a dropped event and not as a mislabelled known kind. This is what lets a
        // newer nest ship a new push kind without breaking an older native app.
        let ev = FfiPushEvent::from(PushEvent::Unknown(Unknown {
            kind: "fauna.kind.from.a.newer.nest".into(),
            payload: fauna_protocol::Value::Null,
        }));
        assert_eq!(
            ev,
            FfiPushEvent::Other {
                kind: "fauna.kind.from.a.newer.nest".into()
            },
            "an unknown kind must arrive as Other carrying its wire kind verbatim"
        );
    }
}

/// The member side of a succession, for the e2e state protocol
/// (`succession-aftermath.md` § Propagation → *MLS groups*).
#[cfg(feature = "conversations-session")]
#[uniffi::export]
impl FfiNestClient {
    /// The member-side succession report for the state provider's
    /// `data.succession_witness` key, as a JSON string — the receive-side twin
    /// of `FfiLandedSuccession::sweep_state_json`, and the four FFI apps' whole
    /// state-contract obligation for the witness.
    ///
    /// `None` until a conversations session has been built, and on a seat whose
    /// identity secret would not parse (which leaves the witness deliberately
    /// unwired). An app republishes `null` there rather than an empty report:
    /// "no session yet" and "a session whose witness has seen nothing" are
    /// different readings, and only the second indicts the inbound poll.
    ///
    /// Rendered by `fauna_client_recovery::witness::state_json`, so the shape
    /// and the order it must be read in have exactly one owner across all 7
    /// apps — an app's serializer republishes this string rather than
    /// re-encoding anything.
    ///
    /// **Every read behind this is a field read**, never a round trip: an app's
    /// state assembly is its agent's ack path (`e2e-conventions.md` point 11's
    /// second corollary), and one blocking call here would make *every* command
    /// on that app unackable while presenting as an unrelated feature's bug.
    pub fn succession_witness_state_json(&self) -> Option<String> {
        // The counts come off the session the factory already stashed for the
        // iMIP rail; taken first so no two of this client's holders are ever
        // locked at once.
        let counts = self
            .scheduling_session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.backend().succession_statement_counts())
            .unwrap_or_default();
        let guard = self.succession_report.lock().unwrap();
        let report = guard.as_ref()?;
        Some(
            fauna_client_recovery::witness::state_json(
                &report.witness.observation(),
                &report.harvest,
                &counts,
            )
            .to_string(),
        )
    }
}
