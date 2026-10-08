use fauna_client::{AuthClient, MultipartBlob, NestClient, NestClientError};
use fauna_client_caldav::{
    AnonAttendeeDiscovery, AttendeeInfo, CalDavClient, DavRecipientKeys, DecodedEventsPage,
    bridge_routing::{
        DeleteEventRequest, ListCalendarsRequest, ProvisionCalendarRequest, QueryEventsRequest,
    },
    delta_sync::{BackstopVerdict, CalendarSyncTokens, backstop_probe},
    dispatch_imip_request, imip_reply_for_rsvp, imip_request_for_invite, personal_calendar_id,
    uid_hash,
};
use fauna_client_carddav::{
    CardDavClient, DecodedCardsPage, addressbook_row,
    bridge_routing::{ListAddressbooksRequest, QueryCardsRequest},
    vcard_row,
};
use fauna_client_config::{DavStoreContext, dav_store_context};
use fauna_client_conversations::{ConversationsClient, NestImipDispatch};
use fauna_client_recovery::linked_fanout::native::NativeLinkedNestDial;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::rsvp::RsvpResponse;
use fauna_core::secret::SecretString;
use fauna_nest_http::paths;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc;

use crate::app::{ActionResult, DataMessage, UiMessage};
use crate::feed::host::LinuxFeedManager;
use crate::rows::{ContactRow, KnockRow};
use crate::views::events::caldav_backend;

// ---------------------------------------------------------------------------
// UiSender — wraps mpsc::Sender<UiMessage>, Send + Clone
// ---------------------------------------------------------------------------

/// A thread-safe sender for `UiMessage` values. Clone it freely and move
/// into tokio tasks or background threads.
#[derive(Clone)]
pub struct UiSender {
    tx: mpsc::Sender<UiMessage>,
}

impl UiSender {
    pub fn send(&self, msg: UiMessage) {
        let _ = self.tx.send(msg);
    }
}

/// Create a `(UiSender, mpsc::Receiver<UiMessage>)` pair. The receiver
/// should be drained on the GTK main thread (via `glib::timeout_add_local`
/// or similar).
pub fn ui_channel() -> (UiSender, mpsc::Receiver<UiMessage>) {
    let (tx, rx) = mpsc::channel();
    (UiSender { tx }, rx)
}

// ---------------------------------------------------------------------------
// FaunaClient — owns a tokio Runtime for background I/O
// ---------------------------------------------------------------------------

/// A once-derived actor id.
///
/// Its own type rather than a bare `OnceCell` so the "derived once, however
/// often it is read" contract has somewhere to be asserted without building a
/// whole [`FaunaClient`] — see this module's tests.
#[derive(Default)]
struct ActorIdMemo(std::cell::OnceCell<Option<String>>);

impl ActorIdMemo {
    fn get(&self, derive: impl FnOnce() -> Option<String>) -> Option<String> {
        self.0.get_or_init(derive).clone()
    }
}

pub struct FaunaClient {
    node_url: String,
    secret_hex: SecretString,
    /// The actor id derived from `secret_hex` — see [`FaunaClient::actor_id`].
    actor_id: ActorIdMemo,
    /// The background tokio runtime, in `RefCell<Option<…>>` so `shutdown()`
    /// can tear it down explicitly at sign-out/reset — even while the
    /// `Rc<FaunaClient>` itself is still referenced by leaked GTK signal
    /// closures (a known authenticated-window widget-tree reference cycle that
    /// keeps `Rc::strong_count` at ~101 after `window.destroy()`; tracked
    /// internally). Dropping the runtime here
    /// aborts every background task (WebSocket recv/reconnect, inbound poll,
    /// sync engine + its inotify watcher, P2P), releasing their fds and —
    /// critically — stopping the background work that otherwise starves the GTK
    /// main thread across the e2e reset→re-auth-per-test cycle. `Drop` also
    /// tears it down (idempotent with `shutdown()`); we use
    /// `shutdown_background()` rather than the default `Runtime::drop()`, which
    /// would block the GTK main thread waiting for tasks that never finish.
    runtime: RefCell<Option<tokio::runtime::Runtime>>,
    /// A handle to `runtime`, cloned once at construction. Stays valid to hold
    /// even after `shutdown()` drops the runtime (spawning on it then panics,
    /// but external callers — view/settings builders — only spawn while the
    /// client is live, before reset). Lets `runtime_handle()` stay infallible.
    runtime_handle: tokio::runtime::Handle,
    tx: UiSender,
    /// Raw HTTP client — kept for the calls that *don't* go through the
    /// bearer-authed nest surface: WebSocket connects, cross-nest
    /// requests to a remote nest, and unrelated services (GitHub
    /// releases). Everything that hits *our* nest with the user's bearer
    /// goes through `content_api` instead.
    http: reqwest::Client,
    /// Bearer-token lifecycle. Constructed by the launch flow (after the
    /// silent challenge succeeds) or by the wizard exit path (after
    /// LoggedIn). Replaces the previous ad-hoc `auth` Mutex and
    /// `do_authenticate` cache. The bearer is read via `content_api`
    /// (and via `nest_auth` for the WS-RPC handshake), which falls back
    /// to `machine.refresh_token()` when expired or near expiry.
    machine: Arc<fauna_launch_machine::LaunchMachine>,
    /// Bearer-authed REST surface to our nest. The single chokepoint for
    /// the session bearer and the 401-reactive token refresh — see
    /// `crate::nest_content_api`.
    content_api: Arc<dyn crate::nest_content_api::NestContentApi>,
    /// WS-RPC façade authentication state — built over the same
    /// `LaunchMachineBearer` as `content_api`, so the HTTP layer and
    /// the WS handshake share one `fauna.auth.handshake` bearer cache + one
    /// TTL-pre-expiry refresh path + one 4xx-reactive invalidation
    /// path. Stored on `FaunaClient` so feature client crates can
    /// reuse it (and so `nest_rpc` is guaranteed to outlive it).
    /// Reachable via `nest_rpc.auth()` once `start_ws_rpc()` fires.
    #[allow(dead_code)]
    nest_auth: Arc<AuthClient>,
    /// Long-lived WS-RPC client. Constructed by `FaunaClient::new` but
    /// not connected; `start_ws_rpc()` drives `nest_rpc.connect()` from
    /// the `DataMessage::AuthSuccess` arm post-login and spawns the
    /// connection-state + push pumps (WS-RPC adoption, tracked internally).
    nest_rpc: Arc<NestClient>,
    /// Path of the file the user staged in the feed composer (via the attach
    /// button, paste, or the `compose.file` test hook), pending the next
    /// `create_post`. The blob is uploaded at *post* time, not attach time —
    /// matching Windows' `ComposePostAsync(bytes, …)` — so the upload and the
    /// post build run sequentially in one task and there is no
    /// attach-then-immediately-submit race. `RefCell` because only the GTK
    /// main thread touches it.
    staged_attachment: RefCell<Option<String>>,
    /// The **persistent** admin-dns `DnsManagementMachine`, built lazily on the
    /// first DNS method call and reused across every subsequent one
    /// (`dns_machine()`). It MUST persist because the manual-paste issuance flow
    /// stashes a live, non-serializable `instant-acme` order in the machine's
    /// `Inner::pending_order` across `BeginManualIssueCert` → (admin pastes) →
    /// `CompleteManualIssueCert`; a fresh machine per action would drop that order
    /// and a fresh-machine snapshot would wipe `pending_cert` before the page
    /// renders the paste surface. Holding one instance aligns linux with web /
    /// windows (which hold the machine per-page). `RefCell` because only the GTK
    /// main thread builds/clones it; the `Arc` is what the background task holds.
    /// Cleared on `shutdown()` so a sign-out/reset rebuilds against the new
    /// connection. See `tls-certificates.md` § B tier 3 (S6a) + the cert-UI TODO.
    dns_machine: RefCell<Option<Arc<fauna_client_dns::DnsManagementMachine>>>,
    /// Lazily-resolved, then cached for the client's lifetime: the account's
    /// retired owner keys (`AccountRegistry::predecessor_backup_keys`), read
    /// by both `label_custody()` (this plane) and `sync_agent::install()`
    /// (the byte plane). One resolution shared by both, mirroring tui's
    /// single post-auth-hook resolve (`session.rs`'s `succession_predecessors`)
    /// — see `predecessor_backup_keys()` below for why this must not be a
    /// live re-walk on every call.
    predecessor_backup_keys_cache: RefCell<Option<Vec<fauna_core::crypto::BackupKey>>>,
    /// The same walk's **attested** actor ids — the generation machinery's fleet-view `prior`
    /// (`account-data-taxonomy.md` § The generation machinery → *The source of
    /// `prior`*), cached under the same one-resolution rule as the keys above
    /// and read by `account_runtime::install()` (this process's runtime) and
    /// `sync_agent::install()` (the agent's, which holds no registry).
    attested_predecessors_cache:
        RefCell<Option<fauna_client_account_runtime::AttestedPredecessors>>,
    /// The same walk's keys **paired with their identities**
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`), cached under the
    /// same one-resolution rule — the per-signer bound's input for the Media
    /// page and the agent (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*, ruling (8)(c)).
    predecessor_chain_cache:
        RefCell<Option<Vec<(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)>>>,
    /// Per-calendar RFC 6578 sync-tokens for the Events backstop poll
    /// (`fauna_client_caldav::delta_sync`). `Arc<Mutex<_>>` rather than the
    /// `RefCell` the rest of this struct uses because the only reader/writer is
    /// inside a `spawn_bg` task, across an await.
    ///
    /// What it buys: `fetch_events` asks "did anything actually change?" for one
    /// round trip, and on the steady-state answer skips the `query_events` +
    /// unseal + parse of the whole calendar entirely. The 10s poll while the
    /// Events page is visible used to pay that cost per calendar per tick.
    sync_tokens: Arc<std::sync::Mutex<CalendarSyncTokens>>,
}

impl Drop for FaunaClient {
    fn drop(&mut self) {
        // Idempotent with `shutdown()`: if the reset path already tore the
        // runtime down, this is a no-op; otherwise drop it now via
        // `shutdown_background()` (the default `Runtime::drop()` would block
        // the calling thread waiting for never-finishing tasks).
        self.shutdown();
    }
}

/// Build a reqwest client for nest HTTP (the residual content API + file-sync
/// data plane). TLS pinning semantics: [`fauna_client::pinned_http_client`].
fn build_http_client(node_url: &str) -> reqwest::Client {
    fauna_client::pinned_http_client(node_url)
}

#[allow(dead_code)]
impl FaunaClient {
    /// Create a new client. Spawns a multi-threaded tokio runtime, and
    /// spawns the LaunchMachine's TTL-scheduled refresh loop on it. The
    /// `machine` is expected to already be in `Online` state — the
    /// launch flow drives it there via silent challenge before
    /// constructing FaunaClient. The TTL loop wakes 60 s before each
    /// cached bearer's `expires_at` and proactively refreshes; the
    /// `content_api` (and the WS-RPC handshake via `nest_auth`) see the
    /// fresh bearer without paying the request-time refresh cost — and a stale
    /// bearer that slips through still recovers via `content_api`'s
    /// 401-reactive refresh.
    pub fn new(
        node_url: String,
        secret_hex: String,
        tx: UiSender,
        machine: Arc<fauna_launch_machine::LaunchMachine>,
    ) -> Self {
        // gtk-runtime-ok: neither cost of the GTK-thread rule applies here. This
        // runtime is long-lived and never `block_on`ed — work reaches it through
        // `runtime_handle()` — and `FaunaClient::Drop` retires it with
        // `shutdown_background()` precisely so the default `Runtime::drop`'s join
        // of the blocking pool never runs on the dropping thread.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");

        // Spawn the LaunchMachine's TTL-scheduled refresh loop on this
        // long-lived runtime. The loop owns its Arc clone and exits when
        // the machine moves out of Online (sign-out, terminal Offline).
        // The runtime's shutdown_background on Drop cancels any
        // outstanding sleep.
        runtime.spawn(Arc::clone(&machine).ttl_refresh_loop());

        // The **residual HTTP content API** below (`ReqwestNestContentApi`) rides
        // this reqwest client. A self-signed nest is authenticated by pinning the
        // SPKI the WS handshake graduated for this host (security.md
        // § Cross-connection binding) — the secure replacement for the retired
        // `FAUNA_INSECURE_TLS` accept-any path.
        let http = build_http_client(&node_url);
        let content_api: Arc<dyn crate::nest_content_api::NestContentApi> =
            Arc::new(crate::nest_content_api::ReqwestNestContentApi::new(
                node_url.clone(),
                http.clone(),
                crate::nest_content_api::LaunchMachineBearer(Arc::clone(&machine)),
            ));

        // Build the WS-RPC façade over the same LaunchMachineBearer the HTTP
        // layer uses, so both surfaces share one `fauna.auth.handshake` bearer cache.
        // The launch flow has already validated `secret_hex` to reach Online —
        // a malformed secret here is an unrecoverable programmer error.
        let keypair = ActorKeypair::from_secret_hex(&secret_hex)
            .expect("FaunaClient::new: secret_hex must be 32-byte hex");
        let nest_auth: Arc<AuthClient> = Arc::new(AuthClient::with_bearer_source(
            node_url.clone(),
            keypair,
            Arc::new(crate::nest_content_api::LaunchMachineBearer(Arc::clone(
                &machine,
            ))),
            http.clone(),
        ));
        // No `.connect()` — Step 4 wires that into the AuthSuccess arm
        // alongside the raw-push-socket replacement.
        let nest_rpc: Arc<NestClient> = NestClient::with_auth(Arc::clone(&nest_auth));

        // Inbound mail receive is no longer a bespoke loop started here — it is
        // folded into the shared `ConversationsSession::start_receive_loop` (both
        // conv + mail rails) that `app.rs` starts at AuthSuccess via
        // `conv_backend::start_conversations_session`, so it shares the one detached
        // receive task all native apps drive (the `fauna-ffi` factory twin).

        let runtime_handle = runtime.handle().clone();
        Self {
            node_url,
            secret_hex: SecretString::from(secret_hex),
            runtime: RefCell::new(Some(runtime)),
            runtime_handle,
            tx,
            http,
            machine,
            content_api,
            nest_auth,
            nest_rpc,
            staged_attachment: RefCell::new(None),
            dns_machine: RefCell::new(None),
            predecessor_backup_keys_cache: RefCell::new(None),
            attested_predecessors_cache: RefCell::new(None),
            predecessor_chain_cache: RefCell::new(None),
            sync_tokens: Arc::new(std::sync::Mutex::new(CalendarSyncTokens::new())),
            actor_id: ActorIdMemo::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Feed composer attachment staging
    // -----------------------------------------------------------------------

    /// Stage a file for the next `create_post`, and stage its hash-less handle
    /// onto the shared `FeedManager` immediately: a draft saved before the
    /// submit must carry the file by name, or a relaunch loses it with
    /// nothing left to refuse (`ui/feed.md` § Persistence → *Attachments by
    /// content address*). Called by the attach button, clipboard paste, the
    /// dialog composer's picker, and the `compose.file` test hook. The blob
    /// is *not* uploaded here — `create_post` uploads it at post time so the
    /// upload and post-build are sequential (no attach-then-submit race).
    pub fn stage_attachment(&self, file_path: &str) {
        *self.staged_attachment.borrow_mut() = Some(file_path.to_string());
        if let Some(manager) = crate::feed::host::manager()
            && let Some(handle) = picked_handle(file_path)
        {
            let snap = manager.snapshot();
            manager.update_compose(snap.compose.text, snap.compose.tags, Some(handle));
        }
    }

    /// The basename of the currently staged attachment, if any — for the
    /// `compose-file-ready` indicator.
    pub fn staged_attachment_name(&self) -> Option<String> {
        self.staged_attachment.borrow().as_ref().map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
    }

    /// Clear any staged attachment (e.g. when the composer is reset).
    pub fn clear_staged_attachment(&self) {
        *self.staged_attachment.borrow_mut() = None;
    }

    /// The WS-RPC client. Long-lived for the actor session; not yet
    /// connected at the moment `FaunaClient::new` returns —
    /// [`Self::start_ws_rpc`] spawns the reconnect supervisor + push pump
    /// (the app calls it once auth succeeds). Callers can hand the
    /// `Arc<NestClient>` to feature client crates that register their kinds via
    /// `NestClient::with_registry`; `request*` calls fail with
    /// `RpcDisconnected` until the supervisor has connected.
    pub fn nest_rpc(&self) -> &Arc<NestClient> {
        &self.nest_rpc
    }

    /// The launch machine that holds this session's bearer — the e2e
    /// `launch_token` state key and `launch_refresh_token` command read and
    /// drive it (the wrong-clock refresh witness, case M).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn launch_machine(&self) -> Arc<fauna_launch_machine::LaunchMachine> {
        Arc::clone(&self.machine)
    }

    /// The HTTP/bulk plane for blob uploads outside the feed composer — e.g.
    /// the profile edit form's avatar/banner picker
    /// (`views/profile/edit.rs::resolve_pending_image`), which rides the same
    /// `fauna_client::upload_public_post_blob` path `compose-file` uses
    /// internally (`media.md` § Encryption at rest: avatar/banner are "the
    /// same shape" as public-post attachments).
    pub fn content_api(&self) -> &Arc<dyn crate::nest_content_api::NestContentApi> {
        &self.content_api
    }

    /// The local actor's 32-byte Ed25519 signing secret (decoded from the hex
    /// the client holds). `build_main_window` passes it to
    /// `crate::feed::host::init` so the shared `FeedManager` can build + sign
    /// posts. Panics on a malformed secret — the same invariant `FaunaClient`
    /// already relies on at construction.
    pub fn secret_bytes(&self) -> [u8; 32] {
        fauna_core::hex32::decode(&self.secret_hex)
            .expect("FaunaClient secret_hex must be 32-byte hex")
    }

    /// The account's retired owner keys (`AccountRegistry::predecessor_backup_keys`),
    /// resolved once and cached for the client's lifetime — the SAME list
    /// `label_custody()` and `sync_agent::install()` both read, rather than two
    /// independent registry walks that can observe different states (the registry's list grows when predecessor material arrives
    /// mid-session — a recovery-kit restore or a seed-escrow open — and a live
    /// re-walk here would let this plane see it while the agent's one-shot
    /// `install()`-time snapshot stayed stale until the next relaunch, the
    /// exact silent divergence the shared resolver exists to prevent).
    /// Mirrors tui's shape: `session.rs`'s `succession_predecessor_backup_keys`
    /// resolves once at the post-auth hook and every consumer reads that one
    /// value — new predecessor material is picked up on the next login, on
    /// both apps alike, never mid-session on either.
    pub fn predecessor_backup_keys(&self) -> Vec<fauna_core::crypto::BackupKey> {
        if let Some(cached) = self.predecessor_backup_keys_cache.borrow().as_ref() {
            return cached.clone();
        }
        let resolved = self
            .actor_id()
            .map(|actor_id| crate::account_registry().predecessor_backup_keys(&actor_id))
            .unwrap_or_default();
        *self.predecessor_backup_keys_cache.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// The account's retired owner keys **paired with the identities they
    /// belong to**, nearest hop first
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`) — resolved once
    /// and cached exactly as [`Self::predecessor_backup_keys`] is, and for the
    /// same reason. The per-signer bound's input for the Media page and the
    /// agent (`mls-group-key-material.md` § M2 → *Writer-signed change
    /// records*, ruling (8)(c)).
    pub fn predecessor_chain(
        &self,
    ) -> Vec<(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)> {
        if let Some(cached) = self.predecessor_chain_cache.borrow().as_ref() {
            return cached.clone();
        }
        let resolved = self
            .actor_id()
            .map(|actor_id| crate::account_registry().predecessor_backup_keys_by_actor(&actor_id))
            .unwrap_or_default();
        *self.predecessor_chain_cache.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// The account's **attested** succeeded-from identities — the registry
    /// rows whose seeds this device holds, each with its delegable schedule
    /// (`AttestedPredecessors::from_registry`) — resolved once and cached
    /// exactly as [`Self::predecessor_backup_keys`] is, and for the same
    /// reason: the runtime this process hosts and the agent it provisions
    /// must hand the fleet view one `prior`, never two walks that can
    /// disagree. Never a writer-asserted list
    /// (`account-data-taxonomy.md` § The generation machinery → *The source
    /// of `prior`*).
    pub fn attested_predecessors(&self) -> fauna_client_account_runtime::AttestedPredecessors {
        if let Some(cached) = self.attested_predecessors_cache.borrow().as_ref() {
            return cached.clone();
        }
        let resolved = self
            .actor_id()
            .map(|actor_id| {
                fauna_client_account_runtime::AttestedPredecessors::from_registry(
                    &crate::account_registry(),
                    &actor_id,
                )
            })
            .unwrap_or_default();
        *self.attested_predecessors_cache.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// This reader's **label-opening custody** — the owner backup key plus the
    /// shared-folder content-key resolver — for the sealed-first path render
    /// on the snapshot / conflict read surfaces
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
    ///
    /// One resolver, shared with the Media page's byte download
    /// (`views/media/mod.rs`), because the sealing ruling's own logic is that
    /// whoever can open a set's chunks renders its names — a second resolver
    /// here could drift and would fail *silently* (a wrong root degrades to
    /// "omit the row", not to an error).
    ///
    /// Carries the retired-owner-key fallback too (`sync-agent.md` § Credential
    /// model → *Retired owner keys after an identity succession*), read via
    /// [`Self::predecessor_backup_keys`] — the one cached resolution
    /// `sync_agent.rs` shares, mirroring tui's post-auth hook (`session.rs`'s
    /// single `succession_predecessor_backup_keys` resolve, shared with every
    /// consumer).
    pub fn label_custody(&self) -> fauna_core::label_custody::LabelCustody {
        let secret = self.secret_bytes();
        let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
            Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
                Arc::clone(&self.nest_rpc),
                crate::account_runtime::folder_key_store(),
            ));
        let predecessors = self.predecessor_backup_keys();
        fauna_core::label_custody::LabelCustody::new(
            Some(resolver),
            Some(fauna_core::crypto::BackupKey::derive(&secret)),
        )
        .with_predecessors(predecessors)
    }

    #[cfg(test)]
    fn offline_for_test() -> Self {
        let (tx, rx) = ui_channel();
        std::mem::forget(rx);
        let machine = fauna_launch_machine::LaunchMachine::new(
            std::sync::Arc::new(fauna_launch_machine::NullObserver),
            std::sync::Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
        );
        Self::new(
            "http://127.0.0.1:1".to_string(),
            "22".repeat(32),
            tx,
            machine,
        )
    }

    /// Build a dedicated `AuthClient` for the in-process file-sync engine.
    ///
    /// The engine runs on its own worker-thread current-thread runtime
    /// (`crate::sync`) because its `SyncEngine`/rusqlite state is intentionally
    /// `!Sync` (single-task model, like the headless daemon). This `AuthClient`
    /// gets its **own** reqwest pool so the engine's HTTP connections are born
    /// and driven on that runtime, never shared cross-runtime with the GTK
    /// runtime's pool — while still sharing the one `LaunchMachine` bearer cache
    /// (so it reuses the same `fauna.auth.handshake` mint + refresh, no second
    /// authentication).
    pub fn build_sync_auth(&self) -> Arc<AuthClient> {
        let keypair = ActorKeypair::from_secret_hex(&self.secret_hex)
            .expect("FaunaClient::build_sync_auth: secret_hex must be 32-byte hex");
        Arc::new(AuthClient::with_bearer_source(
            self.node_url.clone(),
            keypair,
            Arc::new(crate::nest_content_api::LaunchMachineBearer(Arc::clone(
                &self.machine,
            ))),
            build_http_client(&self.node_url),
        ))
    }

    // -----------------------------------------------------------------------
    // Authenticate
    // -----------------------------------------------------------------------

    /// Trigger a token refresh via the LaunchMachine and emit an
    /// AuthSuccess / AuthFailed UI message. Most callers don't need this —
    /// the launch flow drives the LaunchMachine to Online before
    /// constructing FaunaClient, so a fresh bearer is already in hand.
    /// The post-wizard exit path still calls it to surface the AuthSuccess
    /// signal to the UI.
    pub fn authenticate(&self) {
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let machine = Arc::clone(&self.machine);

        self.spawn_bg(async move {
            machine.refresh_token().await;
            let msg = if machine.current_bearer().is_some() {
                let actor_id = actor_id_from_secret_hex(&secret_hex).unwrap_or_default();
                UiMessage::Data(DataMessage::AuthSuccess {
                    actor_id,
                    handle: String::new(),
                })
            } else {
                let error = machine
                    .snapshot()
                    .last_error
                    .unwrap_or_else(|| crate::i18n::strings::errors::AUTH_FAILED.to_string());
                UiMessage::Data(DataMessage::AuthFailed { error })
            };
            tx.send(msg);
        });
    }

    /// Silent sign-in: the `fauna.auth.challenge` + `fauna.auth.verify`
    /// WS-RPC ceremony over a fresh anonymous connection (the shared
    /// `fauna-launch-machine` `WsAuthConnector` — the same path
    /// `LaunchMachine::start` drives). On success, refreshes the libsecret
    /// server-data cache (handle / domain / tier) and pushes
    /// `DataMessage::IdentityRefreshed` so the launch UI's status bar /
    /// settings page can update from the cached values to the
    /// just-fetched authoritative ones.
    ///
    /// Failures (network, nest down, actor-not-registered) are logged and
    /// swallowed — cached values stay visible. Cross-app convergence
    /// follow-up.
    ///
    /// **One exception, and it is the point of the exception rule**
    /// (`security.md` § Post-auth surfacing): a mid-session
    /// `IdentityChanged` verdict escalates to `DataMessage::NestIdentityChanged`
    /// instead of being swallowed. It is not a fault — it is the same
    /// possible-MITM signal the launch path blocks on, and the session is
    /// already de-facto dead because its connections can no longer graduate.
    pub fn silent_sign_in(&self) {
        let node_url = self.node_url.clone();
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();

        self.spawn_bg(async move {
            match do_silent_sign_in(&node_url, &secret_hex).await {
                Ok(SilentSignIn::Refreshed {
                    handle,
                    domain,
                    tier,
                }) => {
                    // `store_account_cache_async` skips an empty handle (verify
                    // returns `""` when no handle is set on the nest), so this
                    // never clobbers a previously-cached handle with a blank. We
                    // await the ASYNC core here — this runs on `self.runtime`, so
                    // the sync `store_account_cache` (which builds its own
                    // runtime + `block_on`) would panic with "Cannot start a
                    // runtime from within a runtime" on this worker thread.
                    if let Err(e) =
                        store_account_cache_async(Some(&handle), Some(&domain), Some(&tier)).await
                    {
                        tracing::error!("silent-sign-in cache refresh failed: {e:#}");
                    }
                    tx.send(UiMessage::Data(DataMessage::IdentityRefreshed {
                        handle,
                        domain,
                        tier,
                    }));
                }
                Ok(SilentSignIn::NotRegistered) => {
                    // Suspended or removed while signed in — this runs
                    // post-auth only, so the refusal is never onboarding's
                    // "not yet". The third escalating verdict, routed like the
                    // two below and for their reason: the session is already
                    // de-facto dead. The launch flow re-runs the challenge,
                    // earns the same refusal, and lands the previously-signed-in
                    // row's surface (`onboarding.md` § App-launch routing).
                    tracing::warn!(
                        "silent-sign-in: this nest no longer signs this identity in \
                         (fauna.auth.not_registered) — routing to the launch surface"
                    );
                    tx.send(UiMessage::Data(DataMessage::SignInRefused));
                }
                Ok(SilentSignIn::IdentityChanged) => {
                    // A dedicated message, NOT the generic error path: the
                    // receiver must be able to tell a MITM verdict from a
                    // network blip, and `AuthFailed`/an error banner would
                    // route it into the retry-shaped surfaces the goal doc
                    // forbids here (no retry CTA — a retry cannot change the
                    // verdict, and must never silently re-pin).
                    tx.send(UiMessage::Data(DataMessage::NestIdentityChanged));
                }
                Ok(SilentSignIn::Superseded { new_actor_id_hex }) => {
                    // The second escalating verdict, routed the same way and
                    // for the same reason as the arm directly above: this
                    // session cannot survive it, so it goes to the blocking
                    // launch surface rather than a toast the user cannot act
                    // on. The launch flow re-runs the challenge, earns the same
                    // refusal, and lands on the identity-import screen.
                    tracing::error!(
                        "[identity] this identity was succeeded (claimed successor \
                         {new_actor_id_hex}) — routing to the identity-import flow"
                    );
                    tx.send(UiMessage::Data(DataMessage::IdentitySuperseded));
                }
                Err(e) => {
                    tracing::error!("silent-sign-in failed: {e:#}");
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Contact / Knock API
    // -----------------------------------------------------------------------

    /// Fetch pending knocks for the authenticated actor via
    /// `fauna.knocks.list` (WS-RPC). The connection is actor-keyed, so the
    /// legacy `{actor_id}` path param is dropped.
    pub fn fetch_knocks(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.knocks_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::KnocksLoaded {
                    knocks: knock_rows_from_items(reply.knocks),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.knocks.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Accept a knock from `peer_actor_id` via `fauna.knocks.accept`
    /// (WS-RPC). The typed client controls the wire shape (`peer_id`), so the
    /// hex peer id is passed straight through — no JSON body is reconstructed.
    pub fn accept_knock(&self, peer_actor_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let peer_id = peer_actor_id.to_string();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.knocks_accept(peer_id).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "knock_accepted".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.knocks.accept".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Block a knock from `peer_actor_id` via `fauna.knocks.block` (WS-RPC).
    pub fn block_knock(&self, peer_actor_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let peer_id = peer_actor_id.to_string();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.knocks_block(peer_id).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "knock_blocked".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.knocks.block".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Dismiss a knock from `peer_actor_id` via `fauna.knocks.dismiss`
    /// (WS-RPC).
    pub fn dismiss_knock(&self, peer_actor_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let peer_id = peer_actor_id.to_string();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.knocks_dismiss(peer_id).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "knock_dismissed".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.knocks.dismiss".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the post-succession member-review roster from the succession
    /// ledger (`fauna_client_config::load_member_reviews`) into this window's one
    /// [`crate::settings::member_review::Roster`], which both the contacts badge
    /// and the conversations page's member-chip pair render
    /// (`identity-succession.md` § Propagation → *MLS groups*, item 3a). A
    /// failed read posts a failure and never an empty roster, so marks already
    /// on screen stand.
    ///
    /// Before the account store is up there is nothing to read, and the
    /// post-store-ready pass re-reads the roster the moment it lands
    /// (`crate::succession_aftermath::run_ledger`) — so a call ahead of it is a
    /// quiet no-op, never an error.
    pub fn fetch_member_reviews(&self) {
        if crate::account_runtime::handle().is_none() {
            return;
        }
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match crate::settings::member_review::load_reviews().await {
                Ok(reviews) => UiMessage::Data(DataMessage::MemberReviewsLoaded { reviews }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "member_reviews_load".into(),
                    error: e,
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the contacts list for the authenticated actor via
    /// `fauna.contacts.list` (WS-RPC).
    pub fn fetch_contacts(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.contacts_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::ContactsLoaded {
                    contacts: contact_rows_from_items(reply.contacts),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.contacts.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Re-read everything the contacts page renders: the roster, the pending
    /// knocks, and the post-succession review roster its badge reads.
    ///
    /// The ONE nav-edge door: the real sidebar switch (`app.rs`) and the test
    /// agent's navigate (`main.rs`) both call it, so an e2e navigate exercises
    /// the refresh a user gets. They used to differ — the sidebar skipped the
    /// knocks and the agent skipped the reviews — so returning to the page never
    /// re-read knocks for a user (only a push or a reconnect did), behind a green
    /// e2e whose navigate did.
    ///
    /// The review read rides here because the page is opened rarely, so a fresh
    /// read on arrival stands in for "after every adjudication" on a page with no
    /// reactive push of its own (`identity-succession.md` § Propagation).
    pub fn refresh_contacts_page(&self) {
        self.fetch_contacts();
        self.fetch_knocks();
        self.fetch_member_reviews();
    }

    /// Confirm a contact relationship with `peer_actor_id` via
    /// `fauna.contacts.confirm` (WS-RPC).
    pub fn confirm_contact(&self, peer_actor_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let peer_id = peer_actor_id.to_string();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.contacts_confirm(peer_id).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "contact_confirmed".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.contacts.confirm".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Resolve a handle on the local nest to actor info over the WS-RPC
    /// `fauna.actor.by_handle` kind (replacing the HTTP `/api/v1/actor/by-handle`
    /// twin). The reply is the same `ActorByHandleReply` the HTTP route serialized,
    /// so the `HandleResolved` consumer is unchanged; a not-found / unreachable
    /// resolve surfaces as `ActionResult::Failed` (the contacts error element),
    /// never a raw HTTP body.
    pub fn resolve_handle(&self, handle: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let handle = handle.to_string();
        self.spawn_bg(async move {
            // Bare-handle local resolve (the find-user no-`@` / `Invalid`
            // branch): there is no typed `@domain`, so the home nest reports its
            // canonical/identity domain, which `HandleResolved` then displays as
            // `handle@<primary>`. A typed `bob@domain2` never reaches here — it
            // classifies as `Handle{user,domain}` and routes through
            // `resolve_nest` → `resolve_handle_on_remote`, which threads the
            // domain. (mail-multidomain.md § Multi-domain handles § Resolution)
            tx.send(find_user_message(&*nest_rpc, &handle, None).await);
        });
    }

    /// Resolve a domain to a fauna nest URL via the home nest's SRV lookup over
    /// the anonymous `fauna.nest.resolve` discovery kind (replacing the
    /// deleted `GET /api/v1/resolve-node/{domain}` twin; api-layers.md
    /// discovery row). The result arrives as `DataMessage::NestResolved` with the
    /// original handle + domain, carrying `{"url": …}` so the `NestResolved`
    /// handler (which auto-chains `resolve_handle_on_remote`) is unchanged. An
    /// internal / unresolvable domain (`localhost`, bare IP, `.local`) comes back
    /// with an empty result → the handler renders "Could not resolve domain".
    pub fn resolve_nest(&self, domain: &str, handle: &str) {
        let home_url = self.node_url.clone();
        let domain_owned = domain.to_string();
        let handle_owned = handle.to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            use fauna_anon_client::AnonymousNestClient;
            use fauna_protocol::discovery::{NestResolveReply, NestResolveRequest};
            let result = match AnonymousNestClient::connect(&home_url).await {
                Ok(client) => {
                    let reply: Result<NestResolveReply, _> = client
                        .request(
                            "fauna.nest.resolve",
                            NestResolveRequest {
                                domain: domain_owned.clone(),
                                extra: Default::default(),
                            },
                        )
                        .await;
                    match reply {
                        Ok(r) => serde_json::json!({ "url": r.url }),
                        Err(_) => serde_json::Value::Null,
                    }
                }
                Err(_) => serde_json::Value::Null,
            };
            tx.send(UiMessage::Data(DataMessage::NestResolved {
                domain: domain_owned,
                handle: handle_owned,
                result,
            }));
        });
    }

    /// Resolve a handle on a (possibly remote) nest over the anonymous
    /// `fauna.actor.by_handle` discovery kind — an anonymous connection to
    /// `node_url` (the URL [`Self::resolve_nest`] returned; the local nest or a
    /// cross-nest peer), replacing the deleted
    /// `GET {node_url}/api/v1/actor/by-handle/{handle}` twin (api-layers.md
    /// discovery row). Mirrors the local [`Self::resolve_handle`]: the reply is
    /// the same `ActorByHandleReply` the `HandleResolved` consumer reads, and a
    /// not-found / unreachable resolve surfaces as `ActionResult::Failed` (the
    /// structured WS-RPC error message), never a raw HTTP body.
    ///
    /// `domain` is the typed `@domain` qualifier from the find-user input (the
    /// `Handle{user,domain}` branch). Given, the result names the TYPED
    /// `handle@domain` — the dial names the peer, and the reply's own echo is
    /// never read for identity (`foreign-handle-resolution.md` § Peer-auth
    /// model; see [`find_user_message`]) — and a nest that does not serve it
    /// refuses (`mail-multidomain.md` § Multi-domain handles § Resolution).
    pub fn resolve_handle_on_remote(&self, node_url: &str, handle: &str, domain: Option<&str>) {
        let node_url = node_url.to_string();
        let handle = handle.to_string();
        let domain = domain.map(|d| d.to_string());
        let tx = self.tx.clone();

        self.spawn_bg(async move {
            use fauna_anon_client::AnonymousNestClient;
            let msg = match AnonymousNestClient::connect(&node_url).await {
                Ok(client) => find_user_message(&client, &handle, domain.as_deref()).await,
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.actor.by_handle".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Send a knock (introductory message) to the given actor ID.
    ///
    /// This sends a minimal signed message to the target's inbox, which will
    /// appear as a knock if they have knock-mode enabled.
    pub fn send_knock(&self, peer_actor_id: &str) {
        // Wire sentinel, NOT a UI label — shared so it can't drift per client
        // (the `contacts.knock` button label is a separate i18n string).
        self.send_message(
            peer_actor_id,
            fauna_client_core::email::KNOCK_SUBJECT,
            fauna_client_core::email::KNOCK_BODY,
        );
    }

    /// Send a knock and hand its CLASSIFIED outcome to `on_done` on the GTK
    /// thread — the path every knock button with a guardian-ask affordance
    /// takes (the contacts page's Find User result, the profile page's
    /// `profile-request-contact-button`). Unlike the fire-and-forget
    /// [`Self::send_knock`], the caller learns whether the nest refused the
    /// knock with the TYPED guardian-approval error, which is the one failure
    /// that reveals `contact-request-guardian-button` (`family-safety.md`
    /// § Child-initiated contact requests → *App affordance*, rule (a)).
    ///
    /// `recipient_nest_url` `None` delivers on this nest; `Some(peer)` makes the
    /// home nest originate the federation leg (the profile page's
    /// `fauna_client_profile::knock_recipient_nest_url` route). Mirrors tui's
    /// `contacts::send_knock`.
    pub fn send_knock_classified(
        &self,
        peer_actor_id: &str,
        recipient_nest_url: Option<String>,
        on_done: impl FnOnce(KnockSend) + 'static,
    ) {
        let nest = Arc::clone(&self.nest_rpc);
        let node_url = self.node_url.clone();
        let secret = self.secret_bytes();
        let recipient = peer_actor_id.to_string();
        crate::async_helper::spawn_with_snapshot(
            &self.runtime_handle(),
            move || async move {
                knock_classified(nest, &node_url, secret, recipient, recipient_nest_url).await
            },
            on_done,
        );
    }

    /// `fauna.family.contact.request` — the supervised ward's ask, offered only
    /// after a knock came back [`KnockSend::RefusedByGuardian`]. On success it
    /// re-reads the ward's own asks in the same round trip, so what paints is
    /// what the NEST holds; a failed re-read is not a failed ask (the guardian
    /// has been rung), so it degrades to an empty list and the caller's local
    /// flag carries the render. Mirrors tui's `contacts::ask_guardian`.
    pub fn ask_guardian_for_contact(
        &self,
        peer_actor_id: &str,
        on_done: impl FnOnce(ContactAskResult) + 'static,
    ) {
        let nest = Arc::clone(&self.nest_rpc);
        let peer = peer_actor_id.to_string();
        crate::async_helper::spawn_with_snapshot(
            &self.runtime_handle(),
            move || async move { ask_contact(nest, peer).await },
            on_done,
        );
    }

    /// `fauna.family.feed_source.request` — the ward's "ask your guardian"
    /// beside a `feed_sources` refusal (`family-safety.md` § Feed-source
    /// approvals). Re-reads `status.feed_requests` on success, the shape
    /// [`Self::ask_guardian_for_contact`] uses. Mirrors tui's
    /// `bridges::Op::RequestFeedSource`.
    pub fn request_feed_source(
        &self,
        bridge_id: &str,
        operation: &str,
        target: &str,
        label: &str,
        on_done: impl FnOnce(FeedAskResult) + 'static,
    ) {
        let nest = Arc::clone(&self.nest_rpc);
        let (bridge_id, operation, target, label) = (
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
            label.to_string(),
        );
        crate::async_helper::spawn_with_snapshot(
            &self.runtime_handle(),
            move || async move { ask_feed_source(nest, bridge_id, operation, target, label).await },
            on_done,
        );
    }

    // -----------------------------------------------------------------------
    // Send message
    // -----------------------------------------------------------------------

    /// Build a signed email/v1 payload and hand it to our home nest over the
    /// `fauna.inbox.send` WS-RPC kind (the home nest local-delivers same-nest
    /// or originates the cross-nest federation leg).
    ///
    /// The work happens on the tokio runtime; results arrive via `UiSender`.
    pub fn send_message(&self, recipient_actor_hex: &str, subject: &str, body: &str) {
        let node_url = self.node_url.clone();
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let recipient_hex = recipient_actor_hex.to_string();
        let subject = subject.to_string();
        let body = body.to_string();

        self.spawn_bg(async move {
            let result = build_and_send(
                nest_rpc,
                &node_url,
                &secret_hex,
                &recipient_hex,
                &subject,
                &body,
            )
            .await;

            match result {
                Ok(()) => {
                    tx.send(UiMessage::Action(ActionResult::Success {
                        context: "message_sent".into(),
                    }));
                }
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "send_message".into(),
                        error: e.to_string(),
                    }));
                }
            }
        });
    }

    /// Export account data to a local file.
    pub fn export_account_data(&self, save_path: &str) {
        let path_str = save_path.to_string();
        self.api_get(
            paths::account::EXPORT_FULL,
            move |bytes| match std::fs::write(&path_str, &bytes) {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: format!("data_exported:{}", path_str),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "data_export".into(),
                    error: e.to_string(),
                }),
            },
        );
    }

    // -----------------------------------------------------------------------
    // Calendar / Event API
    // -----------------------------------------------------------------------

    /// The actor's own email (`<handle>@<domain>` from the account cache) — the
    /// `ORGANIZER` / self RSVP `CAL-ADDRESS` on the encrypted CalDAV path. Empty
    /// components yield a best-effort address; the e2e enables mail (which sets
    /// handle+domain) first.
    fn self_email(&self) -> String {
        let (handle, domain, _) = load_account_cache();
        format!(
            "{}@{}",
            handle.unwrap_or_default(),
            domain.unwrap_or_default()
        )
    }

    /// Fetch all calendars for the authenticated user from the encrypted
    /// `bridge_caldav_*` store via `fauna.bridges.list_calendars` (events.md
    /// Decision B). When the actor has no calendars yet, the Personal calendar
    /// is lazily provisioned (`personal_calendar_id()`, byte-identical to the
    /// MDA's) so the page always has at least one selectable calendar. Degrades
    /// to an empty list when the calendar key is unavailable (mail/CalDAV not
    /// enabled — see [`caldav_context`]).
    pub fn fetch_calendars(&self) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        self.spawn_bg(async move {
            let Some(DavStoreContext { actor_id, msek, prior_mseks }) = caldav_context(&nest, &secret_hex).await else {
                tx.send(UiMessage::Data(DataMessage::CalendarsLoaded {
                    calendars: vec![],
                }));
                return;
            };
            let client = CalDavClient::new(Arc::clone(&nest));
            let mut entries = match client
                .list_calendars(ListCalendarsRequest {
                    actor_id: actor_id.to_vec(),
                })
                .await
            {
                Ok(reply) => reply.calendars,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.list_calendars".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            // Lazy-provision Personal so a fresh actor always has a calendar.
            // Surface the provision/re-list errors instead of swallowing them
            // (`let _`): a silent failure here is indistinguishable from "no
            // calendars" and is exactly how the calendar-load gap stays opaque.
            if entries.is_empty() {
                match caldav_backend::seal_calendar_metadata_fields("Personal", "#3273dc", &msek) {
                    Ok(metadata) => {
                        if let Err(e) = client
                            .provision_calendar(ProvisionCalendarRequest {
                                actor_id: actor_id.to_vec(),
                                calendar_id: personal_calendar_id().to_vec(),
                                encrypted_metadata: metadata,
                                ..Default::default()
                            })
                            .await
                        {
                            tracing::error!(
                                "fetch_calendars: provision_calendar failed: {e}"
                            );
                        }
                        match client
                            .list_calendars(ListCalendarsRequest {
                                actor_id: actor_id.to_vec(),
                            })
                            .await
                        {
                            Ok(reply) => entries = reply.calendars,
                            Err(e) => tracing::error!(
                                "fetch_calendars: re-list after provision failed: {e}"
                            ),
                        }
                    }
                    Err(e) => tracing::error!(
                        "fetch_calendars: seal Personal metadata failed: {e}"
                    ),
                }
            }
            // Surface (don't silently `.ok()`-drop) an entry that fails to
            // decode: a calendar whose sealed metadata the client can't open —
            // e.g. a non-canonical-CBOR producer (the MDA lazy-Personal bug) or
            // an msek mismatch — would otherwise just vanish from the list.
            // Derived once for the whole list — every row's metadata reuses it
            // instead of paying its own X-Wing keygen .
            let dav_keys = DavRecipientKeys::from_mseks(&msek, &prior_mseks);
            let calendars: Vec<_> = entries
                .iter()
                .filter_map(
                    |e| match caldav_backend::calendar_row_from_entry(e, &dav_keys) {
                        Ok(row) => Some(row),
                        Err(err) => {
                            tracing::error!(
                                "fetch_calendars: decode entry (cal_id={}, meta_len={}) failed: {err}",
                                fauna_core::format::hex_full(&e.calendar_id),
                                e.encrypted_metadata.len()
                            );
                            None
                        }
                    },
                )
                .collect();
            tx.send(UiMessage::Data(DataMessage::CalendarsLoaded { calendars }));
        });
    }

    /// Create a new calendar in the encrypted store via
    /// `fauna.bridges.provision_calendar`. `visibility` is a local display
    /// toggle (events.md § State & data shape — not stored server-side); the
    /// sealed metadata carries only name + colour. The new calendar gets a fresh
    /// random 32-byte id (Personal alone has the deterministic
    /// `personal_calendar_id()`).
    pub fn create_calendar(&self, name: &str, _visibility: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            let Some(DavStoreContext { actor_id, msek, .. }) =
                caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "calendar_created".into(),
                    error: crate::i18n::strings::errors::CALENDAR_REQUIRES_MAIL.into(),
                }));
                return;
            };
            let metadata =
                match caldav_backend::seal_calendar_metadata_fields(&name, "#3273dc", &msek) {
                    Ok(m) => m,
                    Err(e) => {
                        tx.send(UiMessage::Action(ActionResult::Failed {
                            context: "calendar_created".into(),
                            error: e.to_string(),
                        }));
                        return;
                    }
                };
            // A fresh, unique calendar id (blake3 of a random UID). Personal is
            // the only deterministic one (shared with the MDA).
            let calendar_id = uid_hash(&format!("{}@fauna-desktop", uuid::Uuid::new_v4()));
            let client = CalDavClient::new(nest);
            let msg = match client
                .provision_calendar(ProvisionCalendarRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: calendar_id.to_vec(),
                    encrypted_metadata: metadata,
                    ..Default::default()
                })
                .await
            {
                Ok(_) => UiMessage::Action(ActionResult::Success {
                    context: "calendar_created".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.provision_calendar".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch events for a specific calendar from the encrypted store via
    /// `fauna.bridges.query_events` + client-side unseal/parse. The encrypted
    /// path serves no server-side date filtering (Decision 10 — bodies are
    /// opaque); the grids window locally.
    pub fn fetch_events(&self, calendar_id: &str) {
        self.fetch_events_inner(calendar_id);
    }

    /// Fetch events for a calendar; `start`/`end` are ignored on the encrypted
    /// path (the bodies are opaque server-side, so windowing is client-side in
    /// the grids). Kept for call-site compatibility with the date-ranged views.
    pub fn fetch_events_in_range(&self, calendar_id: &str, _start: &str, _end: &str) {
        self.fetch_events_inner(calendar_id);
    }

    fn fetch_events_inner(&self, calendar_id: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let sync_tokens = Arc::clone(&self.sync_tokens);
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Data(DataMessage::EventsLoaded {
                    calendar_id: cal_hex.clone(),
                    events: vec![],
                }));
                return;
            };
            let client = CalDavClient::new(nest);

            // Ask what changed before paying to re-read the calendar. The
            // Events-page backstop poll runs every 10s while the page is
            // visible, and its overwhelmingly common answer is "nothing" —
            // which `backstop_probe` settles in one round trip that unseals
            // nothing, where the full read below fetches and unseals every
            // event in the calendar. (Quick appearance is the `CalendarChanged`
            // push's job, not this poll's — `delta_sync`'s module docs.)
            //
            // The rule, the fail-safes and the "don't even ask without a
            // baseline" short-circuit all live in the shared seam; every app
            // owes this same decision, so none of them re-derives it. What is
            // local is only the two brief locks: a `std::sync::MutexGuard`
            // spanning the probe's awaits would make this future `!Send`, and
            // `spawn_bg` needs `Send`.
            //
            // The invariant that makes skipping safe is ours to keep: a token
            // exists for this calendar ONLY if `query_events_in_seeded`
            // completed an `Ok` read below, and that path always sends
            // `EventsLoaded`. So "we hold a token" implies "the UI already has
            // a full event list for this calendar", and returning early leaves
            // it displaying a list nest just told us is still current.
            let held = sync_tokens
                .lock()
                .expect("sync tokens")
                .token(&cal_id)
                .map(str::to_string);
            match backstop_probe(&client, &actor_id, &cal_id, held.as_deref(), 0).await {
                BackstopVerdict::Unchanged { next_token } => {
                    if let Some(t) = next_token {
                        sync_tokens
                            .lock()
                            .expect("sync tokens")
                            .set_token(&cal_id, t);
                    }
                    return;
                }
                BackstopVerdict::ReadRequired => {}
            }

            let events = match query_events_in_seeded(
                &client,
                &actor_id,
                &cal_id,
                &msek,
                &prior_mseks,
                &sync_tokens,
            )
            .await
            {
                Ok(decoded) => decoded
                    .iter()
                    .filter_map(|d| caldav_backend::event_from_decoded(d, &cal_hex).ok())
                    .map(|ev| ev.row)
                    .collect(),
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.query_events".into(),
                        error: e,
                    }));
                    return;
                }
            };
            tx.send(UiMessage::Data(DataMessage::EventsLoaded {
                calendar_id: cal_hex.clone(),
                events,
            }));
        });
    }

    /// Fetch the actor's CardDAV address books from the encrypted store via
    /// `fauna.bridges.list_addressbooks` + client-side metadata unseal
    /// (carddav-server.md § Independent enablement). CardDAV reads the SAME
    /// `cfg.mail.msek` as CalDAV, so [`caldav_context`] supplies both the actor
    /// id and the sealing key. Read-only (slice 4b): no lazy provisioning — the
    /// lazy "Contacts" book appears once a CardDAV MUA (or a future write slice)
    /// PUTs the first card. Degrades to an empty list when mail/CardDAV isn't
    /// enabled (no `msek` minted yet), mirroring `fetch_calendars`.
    pub fn fetch_addressbooks(&self) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        self.spawn_bg(async move {
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Data(DataMessage::AddressbooksLoaded {
                    addressbooks: vec![],
                }));
                return;
            };
            let client = CardDavClient::new(Arc::clone(&nest));
            let decoded = match client
                .list_addressbooks_decoded(
                    ListAddressbooksRequest {
                        actor_id: actor_id.to_vec(),
                    },
                    &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
                )
                .await
            {
                Ok(books) => books,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.list_addressbooks".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            let addressbooks = decoded.iter().map(addressbook_row).collect();
            tx.send(UiMessage::Data(DataMessage::AddressbooksLoaded {
                addressbooks,
            }));
        });
    }

    /// Fetch all cards in one address book from the encrypted store via
    /// `fauna.bridges.query_cards` + client-side unseal/parse. Like
    /// `fetch_events`, the encrypted path serves no server-side filtering (bodies
    /// are opaque); the whole book is returned and the list rendered locally.
    /// `AddressbookNotFound` (a stale book id) collapses to an empty list.
    pub fn fetch_cards(&self, addressbook_id: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let book_hex = addressbook_id.to_string();
        self.spawn_bg(async move {
            let Some(book_id) = hex32(&book_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Data(DataMessage::CardsLoaded {
                    addressbook_id: book_hex.clone(),
                    cards: vec![],
                }));
                return;
            };
            let client = CardDavClient::new(nest);
            let page = match client
                .query_cards_decoded(
                    QueryCardsRequest {
                        actor_id: actor_id.to_vec(),
                        addressbook_id: book_id.to_vec(),
                        since_modseq: None,
                        after_card_id: None,
                        limit: 0,
                    },
                    &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
                )
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.query_cards".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            let cards = match page {
                DecodedCardsPage::Ok { cards, .. } => cards.iter().map(vcard_row).collect(),
                DecodedCardsPage::AddressbookNotFound => vec![],
            };
            tx.send(UiMessage::Data(DataMessage::CardsLoaded {
                addressbook_id: book_hex.clone(),
                cards,
            }));
        });
    }

    /// Resolve a search hit's `uid_hash` to its Address Book `card_id` via the
    /// shared `CardDavClient::locate_card_by_uid_hash` — the id-space join a
    /// `SearchNav::Contact` deep link needs (`ui/search.md` § Where logic lives
    /// → Result navigation (deep link)). `uid_hash` (the index's dedup key) and
    /// `card_id` (the Address Book's server-assigned id) are deliberately
    /// different spellings of the same width, both hex — feeding one to the
    /// other consumer would open nothing and raise no error, which is why this
    /// is a lookup rather than a cast.
    ///
    /// One `list_addressbooks` plus one `query_cards` per book (bounded by book
    /// count, not card count — `locate_card_by_uid_hash`'s own doc comment owns
    /// the reasoning), so the reply carries BOTH the book-picker rows and the
    /// found card's own book's cards in one round trip — `DataMessage::
    /// CardLocated`'s handler (`views::contacts::address_book::open_located_card`)
    /// paints all of it at once rather than racing a second `CardsLoaded`.
    pub fn locate_card_by_uid(&self, uid_hash: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let uid_hash_hex = uid_hash.to_string();
        self.spawn_bg(async move {
            let Some(uid_hash) = hex32(&uid_hash_hex) else {
                tx.send(UiMessage::Data(DataMessage::CardLocated {
                    addressbooks: vec![],
                    open: None,
                }));
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Data(DataMessage::CardLocated {
                    addressbooks: vec![],
                    open: None,
                }));
                return;
            };
            let client = CardDavClient::new(nest);
            let located = match client
                .locate_card_by_uid_hash(actor_id.to_vec(), &msek, &prior_mseks, &uid_hash)
                .await
            {
                Ok(l) => l,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.locate_card".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            let addressbooks = located.books.iter().map(addressbook_row).collect();
            let open = located.card.map(|c| {
                (
                    hex::encode(&c.addressbook_id),
                    c.cards.iter().map(vcard_row).collect::<Vec<_>>(),
                    hex::encode(&c.card_id),
                )
            });
            tx.send(UiMessage::Data(DataMessage::CardLocated {
                addressbooks,
                open,
            }));
        });
    }

    /// Create a new event in the encrypted store: build the canonical VEVENT
    /// from the form params (the reused `fauna_core::ical` writer), seal it, and
    /// PUT it via `fauna.bridges.put_event_ciphertext`. `params` carries the
    /// `calendar_id` and a freshly-minted `uid` (its `blake3` is the dedup key).
    pub fn create_event(&self, params: serde_json::Value) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let self_email = self.self_email();
        self.spawn_bg(async move {
            let cal_hex = params
                .get("calendar_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let uid = params
                .get("uid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}@fauna-desktop", uuid::Uuid::new_v4()));
            let Some(cal_id) = hex32(&cal_hex) else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "event_created".into(),
                    error: crate::i18n::strings::errors::NO_CALENDAR_SELECTED.into(),
                }));
                return;
            };
            let Some(DavStoreContext { actor_id, msek, .. }) =
                caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "event_created".into(),
                    error: crate::i18n::strings::errors::CALENDAR_REQUIRES_MAIL.into(),
                }));
                return;
            };
            let fields = caldav_backend::event_fields_from_params(&params, &uid);
            let client = CalDavClient::new(nest);
            let msg = match client
                .seal_and_put_event(
                    &actor_id,
                    &cal_id,
                    &uid_hash(&uid),
                    &msek,
                    &fields,
                    &[],
                    &self_email,
                    None,
                    now_secs(),
                    None,
                )
                .await
            {
                Ok(_) => UiMessage::Action(ActionResult::Success {
                    context: "event_created".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.put_event_ciphertext".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Delete an event from the encrypted store via `fauna.bridges.delete_event`,
    /// addressing the row by its `uid_hash` (the UI passes `EventRow::id`, which
    /// is the hex `uid_hash`).
    pub fn delete_event(&self, calendar_id: &str, uid_hash_hex: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        self.spawn_bg(async move {
            let (Some(cal_id), Some(uh)) = (hex32(&cal_hex), hex32(&uid_hex)) else {
                return;
            };
            let Some(DavStoreContext { actor_id, .. }) = caldav_context(&nest, &secret_hex).await
            else {
                return;
            };
            let client = CalDavClient::new(nest);
            let msg = match client
                .delete_event(DeleteEventRequest {
                    actor_id: actor_id.to_vec(),
                    calendar_id: cal_id.to_vec(),
                    uid_hash: uh.to_vec(),
                    if_match: None,
                })
                .await
            {
                Ok(_) => UiMessage::Action(ActionResult::Success {
                    context: "event_deleted".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.delete_event".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// RSVP to an event (read-mutate-rewrite on the encrypted store): query the
    /// event by `uid_hash`, apply the RSVP (`PARTSTAT` + the asymmetric
    /// `interested` sidecar — caldav-server.md § RSVP semantics), and re-PUT it.
    /// `response` is the typed submission set ([`RsvpResponse`]) — going /
    /// interested / declined, the three answers a Fauna app offers; the
    /// inbound-only `tentative` is not expressible here
    /// (caldav-server.md § RSVP semantics). The updated
    /// roster is pushed straight to the detail panel via `EventAttendeesLoaded`
    /// (no separate fetch round-trip). When the event has a *different*
    /// organizer, an iMIP `REPLY` is dispatched to them (best-effort) so the
    /// response propagates (caldav-server.md § Server-side auto-schedule —
    /// Responding).
    pub fn rsvp_event(&self, calendar_id: &str, uid_hash_hex: &str, response: RsvpResponse) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let self_email = self.self_email();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        let response = response.as_str().to_string();
        self.spawn_bg(async move {
            let (Some(cal_id), Some(_uh)) = (hex32(&cal_hex), hex32(&uid_hex)) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                return;
            };
            let client = CalDavClient::new(Arc::clone(&nest));
            let decoded =
                match query_events_in(&client, &actor_id, &cal_id, &msek, &prior_mseks).await {
                    Ok(events) => events,
                    Err(e) => {
                        tx.send(UiMessage::Action(ActionResult::Failed {
                            context: "fauna.bridges.query_events".into(),
                            error: e,
                        }));
                        return;
                    }
                };
            let Some(target) = decoded
                .iter()
                .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hex)
            else {
                return;
            };
            let rw = match caldav_backend::apply_rsvp(
                &target.ics,
                target.fauna_ext.as_ref(),
                &self_email,
                &response,
            ) {
                Ok(rw) => rw,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "rsvp".into(),
                        error: e,
                    }));
                    return;
                }
            };
            let put = client
                .seal_and_put_event(
                    &actor_id,
                    &cal_id,
                    &uid_hash(&rw.fields.uid),
                    &msek,
                    &rw.fields,
                    &rw.attendees,
                    &rw.organizer_email,
                    rw.fauna_ext.as_ref(),
                    now_secs(),
                    None,
                )
                .await;
            if let Err(e) = put {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.put_event_ciphertext".into(),
                    error: e.to_string(),
                }));
                return;
            }
            // Notify the organizer with an iMIP REPLY (the "Responding" half of
            // caldav-server.md § Server-side auto-schedule). Best-effort: the
            // local RSVP already persisted, so a send failure is not surfaced —
            // an external (Apple/Gmail) organizer applies the REPLY natively, a
            // Fauna organizer's client merges it on next sync. The organizer-diff
            // + reply construction is the shared `imip_reply_for_rsvp`, so the
            // REPLY (and its DTSTAMP) is byte-identical across every app.
            if let Some(reply) = imip_reply_for_rsvp(&rw, &self_email, now_secs()) {
                let email = fauna_client_email::EmailClient::new(nest);
                let _ = email.send(reply.recipients, reply.raw_rfc5322).await;
            }
            // Push the new roster straight to the detail panel.
            tx.send(UiMessage::Data(DataMessage::EventAttendeesLoaded {
                event_id: uid_hex.clone(),
                attendees: attendee_rows(&rw.attendees),
            }));
            tx.send(UiMessage::Action(ActionResult::Success {
                context: "event_rsvp".into(),
            }));
        });
    }

    /// Add an attendee (when `attendee_email` is non-empty) and fan out an iMIP
    /// `REQUEST` to the event's roster (organizer auto-schedule, caldav-server.md
    /// § Scheduling & invitations). The combined "type an email → Invite" action:
    ///
    /// 1. If `attendee_email` is non-empty, [`caldav_backend::add_attendee`]
    ///    appends `ATTENDEE;mailto:<email>` to the canonical VEVENT and the event
    ///    is **re-PUT first**, so the roster persists even if the best-effort send
    ///    below fails (events.md § Errors); the updated roster is pushed straight
    ///    to the open detail panel via `EventAttendeesLoaded` (mirroring
    ///    `rsvp_event`). An empty field re-sends to the existing roster.
    /// 2. The scheduling message is built + **forked per attendee transport** via
    ///    the shared `fauna_client_caldav::dispatch_imip_request` (Slice 5): every
    ///    email-reachable attendee gets one fanned iMIP email (`fauna.email.send`),
    ///    every **mailbox-less** Fauna attendee (CalDAV on / email off) gets the iMIP
    ///    over the WS-RPC sealed MLS welcome rail (`NestImipDispatch` →
    ///    `ConversationsSession::deliver_scheduling_imip`, caldav-server.md §
    ///    Server-side auto-schedule). The routing lives in shared Rust so linux, the
    ///    native `FfiCaldavClient`, and the web seam fork identically (priority #2).
    ///
    /// Email is the universal attendee identifier (a `mailto:` CAL-ADDRESS that is
    /// handle == email == Fauna identity in production); the resolver falls back to
    /// it for every attendee that is not a positively-confirmed mailbox-less Fauna
    /// actor. The mailbox-less rail needs the logged-in conversations session (its
    /// loaded MLS engine); absent it (pre-login — should not happen once a calendar
    /// exists) the dispatch degrades to email-only.
    pub fn invite_to_event(&self, calendar_id: &str, uid_hash_hex: &str, attendee_email: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let self_email = self.self_email();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        let attendee_email = attendee_email.trim().to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "event_invited".into(),
                    error: crate::i18n::strings::errors::CALENDAR_REQUIRES_MAIL.into(),
                }));
                return;
            };
            let client = CalDavClient::new(Arc::clone(&nest));
            let Ok(events) =
                query_events_in(&client, &actor_id, &cal_id, &msek, &prior_mseks).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "event_invited".into(),
                    error: crate::i18n::strings::errors::EVENT_LOAD_FOR_INVITE_FAILED.into(),
                }));
                return;
            };
            let Some(decoded) = events
                .iter()
                .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hex)
            else {
                return;
            };

            // Compose the dispatch inputs. When an email was typed, add it to the
            // roster + re-PUT first so the attendee persists regardless of the
            // best-effort send; otherwise re-send to the existing roster.
            let (fields, roster, organizer) = if attendee_email.is_empty() {
                match caldav_backend::imip_inputs(&decoded.ics, &self_email) {
                    Ok(parts) => parts,
                    Err(e) => {
                        tx.send(UiMessage::Action(ActionResult::Failed {
                            context: "event_invited".into(),
                            error: e,
                        }));
                        return;
                    }
                }
            } else {
                let rw = match caldav_backend::add_attendee(
                    &decoded.ics,
                    decoded.fauna_ext.as_ref(),
                    &self_email,
                    &attendee_email,
                ) {
                    Ok(rw) => rw,
                    Err(e) => {
                        tx.send(UiMessage::Action(ActionResult::Failed {
                            context: "event_invited".into(),
                            error: e,
                        }));
                        return;
                    }
                };
                if let Err(e) = client
                    .seal_and_put_event(
                        &actor_id,
                        &cal_id,
                        &uid_hash(&rw.fields.uid),
                        &msek,
                        &rw.fields,
                        &rw.attendees,
                        &rw.organizer_email,
                        rw.fauna_ext.as_ref(),
                        now_secs(),
                        None,
                    )
                    .await
                {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.put_event_ciphertext".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
                // Push the updated roster straight to the open detail panel.
                tx.send(UiMessage::Data(DataMessage::EventAttendeesLoaded {
                    event_id: uid_hex.clone(),
                    attendees: attendee_rows(&rw.attendees),
                }));
                (rw.fields, rw.attendees, rw.organizer_email)
            };

            // The shared organizer dispatch fork (Slice 5): resolve each attendee's
            // transport and route email-reachable → the bridge MTA, mailbox-less
            // Fauna → the WS-RPC sealed MLS rail. The conversations session (started
            // at AuthSuccess) backs the mailbox-less rail via `NestImipDispatch`.
            let Some(session) = crate::conversations::conv_backend::active_session() else {
                // No conversations session (pre-login / receive-only). Defensive —
                // a calendar implies a logged-in session; degrade to email-only so
                // the invite still goes to email-reachable attendees.
                let msg = match imip_request_for_invite(&fields, &roster, &organizer, now_secs()) {
                    None => UiMessage::Action(ActionResult::Success {
                        context: "event_invited_empty".into(),
                    }),
                    Some(message) => match fauna_client_email::EmailClient::new(nest)
                        .send(message.recipients, message.raw_rfc5322)
                        .await
                    {
                        Ok(_) => UiMessage::Action(ActionResult::Success {
                            context: "event_invited".into(),
                        }),
                        Err(e) => UiMessage::Action(ActionResult::Failed {
                            context: "event_invited".into(),
                            error: e.to_string(),
                        }),
                    },
                };
                tx.send(msg);
                return;
            };
            let dispatch = NestImipDispatch::new(nest, session);
            let report = match dispatch_imip_request(
                &AnonAttendeeDiscovery,
                &dispatch,
                &fields,
                &roster,
                &organizer,
                now_secs(),
            )
            .await
            {
                Ok(report) => report,
                // A resolve fault (malformed CAL-ADDRESS / discovery transport
                // error) — nothing was sent; surface it (the caller retries).
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "event_invited".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            // Surface the outcome: nobody to notify → informational; a best-effort
            // rail failure → failed (the roster is already persisted); else success.
            let msg = if report.email_recipients == 0
                && report.mailboxless_delivered == 0
                && report.errors.is_empty()
            {
                UiMessage::Action(ActionResult::Success {
                    context: "event_invited_empty".into(),
                })
            } else if let Some(err) = report.errors.first() {
                UiMessage::Action(ActionResult::Failed {
                    context: "event_invited".into(),
                    error: err.clone(),
                })
            } else {
                UiMessage::Action(ActionResult::Success {
                    context: "event_invited".into(),
                })
            };
            tx.send(msg);
        });
    }

    /// Select (or clear) the event shown in the persistent detail panel. Emits a
    /// local `EventSelected` UI message — no nest round-trip — so the panel
    /// re-renders immediately; pair with `fetch_attendees` to fill the attendee
    /// list reactively. Pass `None` to clear (e.g. after delete).
    pub fn select_event(&self, event: Option<crate::rows::EventRow>) {
        self.tx
            .send(UiMessage::Data(DataMessage::EventSelected { event }));
    }

    /// Fetch attendees for a specific event from the encrypted store: query the
    /// calendar, find the row by `uid_hash`, and surface its parsed roster (with
    /// the asymmetric RSVP projection). `uid_hash_hex` is the `EventRow::id` the
    /// UI holds.
    pub fn fetch_attendees(&self, calendar_id: &str, uid_hash_hex: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                return;
            };
            let client = CalDavClient::new(nest);
            let Ok(decoded) =
                query_events_in(&client, &actor_id, &cal_id, &msek, &prior_mseks).await
            else {
                return;
            };
            let attendees = decoded
                .iter()
                .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hex)
                .and_then(|d| caldav_backend::event_from_decoded(d, &cal_hex).ok())
                .map(|ev| ev.attendees)
                .unwrap_or_default();
            tx.send(UiMessage::Data(DataMessage::EventAttendeesLoaded {
                event_id: uid_hex.clone(),
                attendees,
            }));
        });
    }

    // -----------------------------------------------------------------------
    // Calendar ICS Import/Export
    // -----------------------------------------------------------------------

    /// Export a calendar as an `.ics` file: the shared
    /// `CalDavClient::export_calendar_ics` builds the text, written to `save_path`.
    pub fn export_calendar_ics(&self, calendar_id: &str, save_path: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let path_str = save_path.to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "ics_export".into(),
                    error: crate::i18n::strings::errors::CALENDAR_REQUIRES_MAIL.into(),
                }));
                return;
            };
            let client = CalDavClient::new(nest);
            let ics = match client
                .export_calendar_ics(
                    &actor_id,
                    &cal_id,
                    &DavRecipientKeys::from_mseks(&msek, &prior_mseks),
                )
                .await
            {
                Ok(ics) => ics,
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "ics_export".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            let msg = match std::fs::write(&path_str, ics.as_bytes()) {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: format!("ics_exported:{}", path_str),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "ics_export".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Import a `.ics` file into a calendar via the shared
    /// `CalDavClient::import_ical_events` (parse + seal + PUT each VEVENT). v1
    /// imports event fields only (attendee rosters on imported events are a
    /// follow-up).
    pub fn import_calendar_ics(&self, calendar_id: &str, ics_data: Vec<u8>) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let self_email = self.self_email();
        let cal_hex = calendar_id.to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext { actor_id, msek, .. }) =
                caldav_context(&nest, &secret_hex).await
            else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "ics_import".into(),
                    error: crate::i18n::strings::errors::CALENDAR_REQUIRES_MAIL.into(),
                }));
                return;
            };
            let text = String::from_utf8_lossy(&ics_data).into_owned();
            let client = CalDavClient::new(nest);
            let outcome = client
                .import_ical_events(
                    &actor_id,
                    &cal_id,
                    &msek,
                    &self_email,
                    &text,
                    now_secs(),
                    |_| format!("{}@fauna-desktop", uuid::Uuid::new_v4()),
                )
                .await;
            tx.send(UiMessage::Action(ActionResult::Success {
                context: format!(
                    "ics_imported:Imported {} events ({} skipped)",
                    outcome.imported, outcome.skipped
                ),
            }));
        });
    }

    // -----------------------------------------------------------------------
    // Feed API
    // -----------------------------------------------------------------------

    /// Fetch all custom feeds for the authenticated user via
    /// `fauna.feed.list` (WS-RPC). The list view omits per-feed rules (they
    /// ride only on `fauna.feed.get`), matching the legacy JSON shape.
    /// Submit a composed post through the shared `FeedManager`: upload the
    /// staged blob (client glue — the platform HTTP/bulk plane), stage the
    /// resolved `AttachedFile` into the manager, and let `submit_post` validate,
    /// build + sign (tags → facets), create over `fauna.posts.create`, and
    /// reload the list. The whole thing runs in one background task on the
    /// manager Arc (`Send + Sync`); compose-level success/failure is reflected
    /// into the snapshot's `FeedComposeState`, not the `UiMessage` channel.
    ///
    /// Gate-to-tier (`ui/feed.md` § Encryption at rest; monetization.md
    /// § Pillars 2+3): when `gate_tier` is set, the manager builds + seals the
    /// gated post (`prepare_gated_blob`), this glue uploads the sealed
    /// full-body blob on the bulk plane (the shared
    /// `fauna_client::upload_gated_post_blob` sidecar), and
    /// `submit_gated_post` creates the post once the nest echoes the content
    /// address. An upload failure aborts the staged submit onto
    /// `compose-error`.
    ///
    /// **"Sell this post…"** (`monetization.md` § Per-post pay-to-unlock):
    /// the staged `sell` is the composer's select's third answer, mutually
    /// exclusive with `gate_tier` by the manager's own setters
    /// (`compose-gate-tier-select` is one control). When set, `prepare_sell_post` auto-mints a degenerate
    /// single-post tier and seals in one call, staging into the same
    /// `pending_gated` slot as an ordinary gated compose — so it finishes
    /// through the identical upload + `submit_gated_post` / `abort_gated_submit`
    /// pair below, no new glue needed (priorities #1/#2).
    ///
    /// **Room-restricted** (`ui/feed.md` § Encryption at rest → *Room-restricted
    /// — the app half*): `gate_room` is the select's room answer, a hex channel
    /// id from `own_rooms`, exclusive with the other two. The shared room arm of
    /// `prepare_gated_blob` seals under the room's key, and the upload below takes
    /// its class off the staged post (`gated_upload_sidecar`), so a room post
    /// needs no branch of its own here beyond staging the answer.
    ///
    /// The audience is the MANAGER's, never an argument: the composer forwards
    /// each answer as it is picked (so a draft carries it), and the manager
    /// holds a restored draft's answer even while the select cannot show it —
    /// a tier or room not yet in `own_tiers`/`own_rooms` paints as Public.
    /// Re-staging the control's answer here once published such a draft
    /// public; submitting the manager's lets the
    /// shared tier resolution fail closed on an unknown tier instead. tui's
    /// `Action::SetGateTier` and apple's `FeedVM.submitPost` are the same
    /// shape. What the shared `FeedComposeState::clear_sent` then decides the
    /// composer paints from the snapshot (`ui/feed.md` § User actions,
    /// `post-submit-button`).
    pub fn submit_post(&self, manager: Arc<LinuxFeedManager>, text: String, tags: String) {
        let nest = Arc::clone(&self.content_api);
        let staged = self.staged_attachment.borrow_mut().take();
        let Some(rt) = self.runtime.borrow().as_ref().map(|r| r.handle().clone()) else {
            return;
        };
        crate::async_helper::spawn_with_snapshot(
            &rt,
            move || async move {
                // Stage text + tags first: every validation below reads them from
                // the composer state (`stage_sell_tier` refuses an empty post
                // before it mints anything), and this app hands them in as
                // arguments rather than staging them as the user types. The
                // attachment is staged in a second `update_compose` once it is
                // sealed and uploaded.
                //
                // The manager's own `attached_file` rides through both stagings:
                // a restored draft's file is a handle whose bytes this device may
                // not hold (`ui/feed.md` § Persistence), and it must reach the
                // shared submit so the refusal there names it — `None` would
                // silently turn the draft into a text-only post. A fresh pick
                // replaces it below; `compose-file-remove` is the only other
                // gesture that clears it, and it writes straight through the
                // manager, so this read always sees the current answer.
                //
                // The audience is already staged — the composer forwards each
                // pick — and it is staged BEFORE the attachment is sealed or
                // uploaded: the ordering rule is the whole of this path
                // (`ui/media.md` § Encryption at rest). Until 2026-09-08 this
                // uploaded a `PublicPost` (plaintext) blob first and only then
                // looked at the gate, so a restricted post's photo was left
                // readable on the nest under a hash anyone can fetch: blob GET is
                // unauthenticated by design and the nest exposes no blob DELETE,
                // so the only fix is to never upload one.
                let compose = manager.snapshot().compose;
                let restored = compose.attached_file;
                let sell = compose.sell;
                manager.update_compose(text.clone(), tags.clone(), restored.clone());

                // A SOLD post's attachment seals under the tier the sale itself
                // mints, which does not exist yet — so the mint is split in two and
                // its first half runs before the seal can reach a period key. Only
                // when there is actually an attachment: with none,
                // `prepare_sell_post` runs this itself and the flow stays the
                // single call it has always been.
                if let (Some(sell), Some(_)) = (sell.as_ref(), staged.as_ref())
                    && let Err(e) = manager
                        .stage_sell_tier(
                            sell.subscribers_get_it_free,
                            sell.asking_price.trim().parse::<u64>().ok(),
                        )
                        .await
                {
                    // Already stamped on `compose-error`; nothing was minted,
                    // because every validation runs ahead of the mint.
                    tracing::warn!("submit_post: sell-tier stage failed: {e}");
                    return false;
                }

                let attached = match staged {
                    Some(path) => match seal_and_upload_attachment(&*nest, &manager, &path).await {
                        Ok(file) => Some(file),
                        Err(e) => {
                            // Never submit a post whose attachment silently
                            // vanished: the user asked for an image, so a failed
                            // upload fails the post (matching tui — priority #1).
                            tracing::warn!("submit_post: attachment upload failed: {e}");
                            manager.abort_gated_submit(e);
                            return false;
                        }
                    },
                    None => restored,
                };
                manager.update_compose(text, tags, attached);
                let sealed = if let Some(sell) = sell {
                    manager
                        .prepare_sell_post(
                            (!sell.price.trim().is_empty()).then_some(sell.price),
                            sell.subscribers_get_it_free,
                            // Empty or unparseable means no machine price — the
                            // tier stays a tip target forever (`monetization.md`
                            // § The asking price). `prepare_sell_post` owns the
                            // sats→msat conversion and the overflow refusal; this
                            // is a plain text→u64 parse and nothing else. It must
                            // be the SAME value `stage_sell_tier` got above:
                            // phase one decides the tier's rank.
                            sell.asking_price.trim().parse::<u64>().ok(),
                        )
                        .await
                        .map(Some)
                } else {
                    manager.prepare_gated_blob().await
                };
                match sealed {
                    Ok(Some(sealed)) => {
                        // The class comes off the staged post — `GroupRestrictedPost`
                        // for a room post, the tier class otherwise — and is never
                        // decided here (`FeedManager::gated_upload_sidecar`).
                        let sidecar = manager.gated_upload_sidecar();
                        match fauna_client::upload_sealed_post_blob(&*nest, sidecar, sealed).await {
                            Ok(hash) => manager.submit_gated_post(hash).await.is_ok(),
                            Err(e) => {
                                tracing::warn!("submit_post: gated blob upload failed: {e}");
                                manager.abort_gated_submit(e);
                                false
                            }
                        }
                    }
                    Ok(None) => manager.submit_post().await.is_ok(),
                    // Validation failure — already stamped on compose-error.
                    Err(_) => false,
                }
            },
            |_sent: bool| {},
        );
    }

    /// Unlock a gated post for the detail view: resolve its sealed-blob hash
    /// (`FeedManager::gated_blob_hash` — one lazy `fauna.posts.get`), fetch
    /// the blob bytes on the bulk plane, and hand them to
    /// `FeedManager::unlock_gated_post` (custody or KeyBlob decrypt). The
    /// manager notifies on success and the feed page's reactive repaint swaps
    /// the full body into the open detail. Best-effort: a locked post (not
    /// entitled / rotated-out period) just keeps showing its teaser.
    pub fn unlock_gated_post(&self, manager: Arc<LinuxFeedManager>, post_id: String) {
        let nest = Arc::clone(&self.content_api);
        self.spawn_bg(async move {
            let Some(hash) = manager.gated_blob_hash(post_id.clone()).await else {
                return;
            };
            let path = paths::blob::by_hash(&hash);
            match nest.get(&path).await {
                Ok(bytes) => {
                    if let Err(e) = manager.unlock_gated_post(post_id, bytes.to_vec()).await {
                        tracing::debug!("gated unlock (post stays teased): {e}");
                    }
                }
                Err(e) => tracing::warn!("gated blob fetch failed: {e}"),
            }
        });
    }

    /// Interact with a post via `fauna.posts.interact` (WS-RPC) — like / unlike
    /// / reply / repost / unrepost / quote. `interaction` is the action name
    /// (the kind's field is `action`; the legacy HTTP twin's `interaction`
    /// JSON key is gone).
    pub fn interact_with_post(&self, post_id: &str, interaction: &str, body: Option<&str>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let post_id = post_id.to_string();
        let action = interaction.to_string();
        let body = body.map(str::to_string);
        // Through the shared `FeedManager::interact`, never a bare
        // `PostsClient::posts_interact`: the manager folds the nest's post-act
        // counters into the loaded window, so the tapped count moves without a
        // feed re-query. linux used to make the raw call and then re-pull the
        // WHOLE feed from `app.rs` to see the new number — correct, but it
        // re-ranks the window under the user's finger (a like moves
        // `content_meta.score`, a ranking input), so a tap could reorder the
        // timeline. With no manager (not yet initialised) there is nothing to
        // fold into and nothing on screen to update.
        let manager = crate::feed::host::manager();
        self.spawn_bg(async move {
            let msg = match manager {
                // `reply`/`quote` are COMPOSED, not recorded: they create a post
                // referencing the target, and that is the only thing that moves
                // the target's counter. `interact` looks identical here and is
                // not the same call — on a native post its nest arm discards
                // `body` outright, so the reply typed into `build_reply_dialog`
                // was acked as success and thrown away
                // (`ui/feed.md` § Implementation status today). The manager
                // routes a *bridged* post back through interact itself, where
                // the body IS consumed, so this stays one call site.
                //
                Some(m) => {
                    let routed = match (action.as_str(), body.clone()) {
                        ("reply", Some(text)) => Some(m.reply(post_id.clone(), text).await),
                        ("quote", text) => {
                            Some(m.quote(post_id.clone(), text.unwrap_or_default()).await)
                        }
                        // `repost` is the manager's TOGGLE off `viewer_repost_id`
                        // (feed.md § Interaction bar → Repost, ratified
                        // 2026-08-10): off → composes the caller's empty-body
                        // `Reference::Repost` post; on → un-reposts it. The
                        // render leg (attribution, no bar, activation
                        // redirect) landed alongside this call in
                        // `post_list.rs`.
                        ("repost", _) => Some(m.repost(post_id.clone()).await),
                        // `like` is the manager's TOGGLE off `viewer_liked`
                        // (feed.md § Interaction bar): it composes nothing —
                        // both directions ride the same interact door on the
                        // same post id — but the bare call below can only ever
                        // like, never un-like, because the nest's like arm is
                        // idempotent per (actor, post).
                        ("like", _) => Some(m.like(post_id.clone()).await),
                        _ => None,
                    };
                    let outcome = match routed {
                        Some(r) => r,
                        None => m.interact(post_id, action, body).await,
                    };
                    match outcome {
                        Ok(()) => UiMessage::Action(ActionResult::Success {
                            context: "post_interaction".into(),
                        }),
                        // A STATED REFUSAL (words under a restricted post —
                        // `ui/feed.md` § Encryption at rest, ruling 6) reads in
                        // the user's language, recognized by the one shared
                        // `refusal_i18n_key`; every other failure keeps its
                        // own text behind the RPC context. tui's `refusal_copy`.
                        Err(e) => match fauna_feed::refusal_i18n_key(&e)
                            .and_then(crate::i18n::strings::lookup)
                        {
                            Some(sentence) => UiMessage::Action(ActionResult::FailedLocalized {
                                message: sentence.to_string(),
                            }),
                            None => UiMessage::Action(ActionResult::Failed {
                                context: "fauna.posts.interact".into(),
                                error: e,
                            }),
                        },
                    }
                }
                None => {
                    let posts = fauna_client_posts::PostsClient::new(nest_rpc);
                    match posts.posts_interact(post_id, action, body).await {
                        Ok(_reply) => UiMessage::Action(ActionResult::Success {
                            context: "post_interaction".into(),
                        }),
                        Err(e) => UiMessage::Action(ActionResult::Failed {
                            context: "fauna.posts.interact".into(),
                            error: e.to_string(),
                        }),
                    }
                }
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Moderation API — `fauna.moderation.*` WS-RPC kinds
    // -----------------------------------------------------------------------

    /// Fetch the connection actor's moderation queue: the **union** of the server
    /// `fauna.moderation.actions` obligation rows (why a piece of the caller's
    /// content was labeled / quarantined / rejected) and the client's own
    /// post-decrypt **local detections** — the encrypted-mode social-content signal
    /// the nest cannot produce (`moderation.md` § Layout & flow). The two sources
    /// merge + dedupe (by `content_id`, server row winning) through the shared
    /// `fauna_client_moderation::merge_queue`, so the dedupe rule never drifts per
    /// client. The local half comes from the conversations session's store
    /// (`ConversationsSession::moderation_local_detections`); with no session it is
    /// empty and the queue is the server rows alone.
    pub fn fetch_moderation_actions(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        // Read the local detections on the UI thread (the session lives here); the
        // async fetch only needs the server rows.
        let local = crate::conversations::conv_backend::active_session()
            .map(|s| s.moderation_local_detections())
            .unwrap_or_default();
        self.spawn_bg(async move {
            let client = fauna_client_moderation::ModerationClient::new(nest_rpc);
            let msg = match client.actions().await {
                Ok(reply) => UiMessage::Data(DataMessage::ModerationActionsLoaded {
                    rows: fauna_client_moderation::merge_queue(&reply.actions, &local),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.moderation.actions".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Apply a `train-correction-button` click for one queue row. A **server** row
    /// (carries an enforcement `action`) submits a ham training correction — the
    /// shared two-half flow (see [`Self::train_moderation`]). A **local** row
    /// (a client-side post-decrypt detection, blank action) has no nest obligation
    /// to train against — the content is client-only (MLS-sealed at rest) — so the
    /// correction removes the false-positive flag from the session store and
    /// repaints the queue; when the client write path is available
    /// (`spam-model-sealed-at-rest` + mail enabled) it *additionally* feeds the
    /// ham correction to the tier-1 model with the same post-decrypt text the
    /// classifier saw (`mail-spam.md` § Encrypted-mode interaction — only the
    /// client can train on content the nest cannot read).
    pub fn correct_moderation_row(&self, content_id: &str, is_local: bool) {
        if is_local {
            if let Some(session) = crate::conversations::conv_backend::active_session() {
                // Read the retained plaintext BEFORE removing the flag (the
                // detection row and the message store are independent, but keep
                // the read-then-clear order obvious). `None` once the message
                // aged out — the correction is then flag-removal only, as it
                // was before the client write path existed.
                let body = session.moderation_message_body(content_id.to_string());
                session.moderation_remove_local_detection(content_id.to_string());
                if let Some(text) = body {
                    self.train_spam_model_client_side(text, false);
                }
            }
            self.fetch_moderation_actions();
        } else {
            self.train_moderation(content_id, "ham");
        }
    }

    /// Feed one train event to the **client-side** sealed tier-1 model write
    /// (`MailSettingsMachine::train_spam_model_client`) for text the client
    /// already holds (a post-decrypt local detection). Fire-and-forget off the
    /// UI thread: a `sealed` outcome surfaces the standard `moderation_trained`
    /// toast; a `ServerPath` outcome is silent — for client-only content there
    /// is no server row to train against (the nest cannot read it), so the
    /// flag removal alone was the correction, exactly as before 1d.
    fn train_spam_model_client_side(&self, text: String, is_spam: bool) {
        let Ok(machine) = crate::mail_glue::build_mail_settings_machine(self) else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match machine.train_spam_model_client(text, is_spam).await {
                Ok(res) if res.sealed => {
                    tx.send(UiMessage::Action(ActionResult::Success {
                        context: "moderation_trained".into(),
                    }));
                }
                Ok(_) => {}
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "moderation_trained".into(),
                        error: e.to_string(),
                    }));
                }
            }
        });
    }

    /// Mark a received **conversation/mail message** as spam — the live `Insert`
    /// consumer (`mail-spam.md` § Encrypted-mode interaction + § Wire shapes
    /// `put_spam_model` `history_op`). The mail-surface twin of
    /// [`Self::train_spam_model_client_side`]: it trains the sealed tier-1 model
    /// over the message's retained decrypted `body` **and** writes a **sealed
    /// training-history row** (subject + n-gram delta sealed to the actor's own
    /// key) via `train_spam_model_client_mail`, so the `mail-spam` page renders it
    /// and offers the per-row undo. Fire-and-forget off the UI thread: a `sealed`
    /// outcome surfaces the standard `moderation_trained` toast; a `ServerPath`
    /// outcome is **silent** — a conversation message is client-only encrypted
    /// content the nest can't read, so (exactly as for a local moderation
    /// correction) there is no server train to degrade to; on a nest without
    /// sealed-at-rest support the sealed audit row simply isn't written.
    ///
    /// `message_id` is the conversation `MessageSnapshot.message_id` string, stored
    /// **opaque** as the row's reference (never decoded nest-side). `subject` falls
    /// back to a body snippet (derived in the shared façade) when the message
    /// carries no subject line. The mailbox
    /// is `INBOX` — a received conversation message has no IMAP mailbox; it is
    /// display metadata (`{subject} · INBOX`) on the sealed row only.
    pub fn mark_message_spam(&self, message_id: String, body: String, subject: Option<String>) {
        if body.trim().is_empty() {
            return;
        }
        let Ok(machine) = crate::mail_glue::build_mail_settings_machine(self) else {
            return;
        };
        // The subject fallback (a body snippet when the message carries no subject
        // line) now lives in the shared façade `train_spam_model_client_mail` — the
        // single home for it, so linux/android/web/apple all pass the raw subject.
        let subject = subject.unwrap_or_default();
        let message_id = message_id.into_bytes();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match machine
                .train_spam_model_client_mail(body, true, message_id, "INBOX".into(), subject)
                .await
            {
                Ok(res) if res.sealed => {
                    tx.send(UiMessage::Action(ActionResult::Success {
                        context: "moderation_trained".into(),
                    }));
                }
                Ok(_) => {}
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "moderation_trained".into(),
                        error: e.to_string(),
                    }));
                }
            }
        });
    }

    /// Submit a spam/ham training correction for one post — the shared flow
    /// `MailSettingsMachine::train_moderation_correction`: when a sealed write
    /// is possible, `fauna.posts.get` → train the sealed model client-side;
    /// then ALWAYS `fauna.moderation.train` (the read gate + report capture),
    /// whose error is the flow's. The queue's `train-correction-button` passes
    /// `verdict = "ham"` (the queued item was flagged — the correction is "this
    /// was a false positive").
    pub fn train_moderation(&self, content_id: &str, verdict: &str) {
        let tx = self.tx.clone();
        let content_id = content_id.to_string();
        let is_spam = verdict == "spam";
        let machine = crate::mail_glue::build_mail_settings_machine(self);
        self.spawn_bg(async move {
            let result = match machine {
                Ok(machine) => machine
                    .train_moderation_correction(content_id, is_spam)
                    .await
                    .map(|_model_half| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let msg = match result {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: "moderation_trained".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "moderation_trained".into(),
                    error: e,
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Bridge Management API — `fauna.bridges.*` WS-RPC kinds
    // -----------------------------------------------------------------------

    /// Fetch **every** bridge integration via `fauna.bridges.list`.
    ///
    /// Deliberately unfiltered: the reply feeds three consumers with different
    /// needs — the retained `bridges_snapshot` (all providers, so a dedicated
    /// page can render its own provider's link surface), the Bluesky
    /// notification-poll trigger (all providers), and the unified Bridges page
    /// list. Only the last one excludes the dedicated-page providers, so
    /// `is_unified_bridges_page_bridge` is applied *there*
    /// (`views::bridges::update_bridge_list`) rather than here — the same
    /// filter-at-the-page shape web uses (`routes/bridges/+page.svelte`).
    /// Filtering at fetch silently starved the other two consumers: it is what
    /// made the Bluesky poll trigger go dead the moment the predicate grew to
    /// exclude `"bluesky"` (bridges.md § Scope; ui/atproto.md § Migration).
    pub fn fetch_bridges(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges.list().await {
                Ok(reply) => UiMessage::Data(DataMessage::BridgesLoaded {
                    bridges: reply.bridges,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Link a bridge via `fauna.bridges.link`. `params` carries the per-mode
    /// form fields (handle / app_password / server_url / nsec / smtp_* / …)
    /// as a typed CBOR map; callers compose it from their dialog inputs.
    pub fn link_bridge(&self, bridge_id: &str, mode: &str, params: fauna_client::Value) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let bridge_id = bridge_id.to_string();
        let mode = mode.to_string();
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges.link(bridge_id.clone(), mode, params).await {
                // OAuth modes (e.g. Bluesky) return a `redirect_url` to the
                // provider's authorize page instead of linking synchronously —
                // open it in the browser (the bridge shows linked only after
                // the OAuth callback completes). Mirrors web's
                // `window.location.href = redirect_url`.
                Ok(reply) => match reply.redirect_url {
                    Some(url) => UiMessage::Data(DataMessage::OpenUrl { url }),
                    None => UiMessage::Action(ActionResult::Success {
                        context: "bridge_linked".into(),
                    }),
                },
                // A `link` ask carries an EMPTY target by construction —
                // approving a link approves connecting that bridge
                // (`FeedSourceOperation::takes_target`).
                Err(e) => feed_source_refusal(
                    &e,
                    &bridge_id,
                    fauna_core::data::FeedSourceOperation::Link,
                    "",
                )
                .unwrap_or_else(|| {
                    UiMessage::Action(ActionResult::Failed {
                        context: "bridge_linked".into(),
                        error: e.to_string(),
                    })
                }),
            };
            tx.send(msg);
        });
    }

    /// Unlink (disconnect) a bridge via `fauna.bridges.unlink`.
    pub fn unlink_bridge(&self, bridge_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let bridge_id = bridge_id.to_string();
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges.unlink(bridge_id).await {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: "bridge_unlinked".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "bridge_unlinked".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch follows for a bridge via `fauna.bridges.list_follows`. Sends
    /// `DataMessage::BridgeFollowsLoaded`.
    pub fn fetch_bridge_follows(&self, bridge_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let bid = bridge_id.to_string();
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges.list_follows(bid.clone()).await {
                Ok(reply) => UiMessage::Data(DataMessage::BridgeFollowsLoaded {
                    bridge_id: bid,
                    follows: reply.follows,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.bridges.list_follows".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Add a follow to a bridge via `fauna.bridges.add_follow`. No `extra`
    /// blob today — the dialog only exposes id + petname; per-provider
    /// extra metadata lands when a UI grows it.
    pub fn add_bridge_follow(&self, bridge_id: &str, id: &str, petname: Option<&str>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let bridge_id = bridge_id.to_string();
        let id = id.to_string();
        let petname = petname.map(str::to_string);
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges
                .add_follow(bridge_id.clone(), id.clone(), petname, None)
                .await
            {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: "bridge_follow_added".into(),
                }),
                Err(e) => feed_source_refusal(
                    &e,
                    &bridge_id,
                    fauna_core::data::FeedSourceOperation::Follow,
                    &id,
                )
                .unwrap_or_else(|| {
                    UiMessage::Action(ActionResult::Failed {
                        context: "bridge_follow_added".into(),
                        error: e.to_string(),
                    })
                }),
            };
            tx.send(msg);
        });
    }

    /// Remove a follow from a bridge via `fauna.bridges.remove_follow`.
    pub fn remove_bridge_follow(&self, bridge_id: &str, follow_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let bridge_id = bridge_id.to_string();
        let follow_id = follow_id.to_string();
        self.spawn_bg(async move {
            let bridges = fauna_client_bridges::BridgesClient::new(nest_rpc);
            let msg = match bridges.remove_follow(bridge_id, follow_id).await {
                Ok(()) => UiMessage::Action(ActionResult::Success {
                    context: "bridge_follow_removed".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "bridge_follow_removed".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Sync / Media API
    // -----------------------------------------------------------------------

    /// Fetch the file list for a folder via `fauna.sync.files` (WS-RPC; the
    /// typed twin of the deleted `GET /api/v1/sync/files`). Sends
    /// `DataMessage::SyncFilesLoaded`. The media file list renders each row's
    /// `path` + `state`; the WS reply carries no per-file `state`, so `state`
    /// is `"unknown"` (the HTTP path already produced that — its JSON only
    /// ever carried `path`/`manifest_hash`/`size_bytes`/`updated_at`).
    pub fn fetch_sync_files(&self, folder: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let fs = folder.to_string();
        self.spawn_bg(async move {
            let sync = fauna_client_sync::SyncClient::new(nest_rpc);
            let msg = match sync.files(fs.clone()).await {
                Ok(reply) => {
                    let files = reply
                        .files
                        .into_iter()
                        .map(|f| crate::rows::SyncFileRow {
                            id: 0,
                            folder: fs.clone(),
                            path: f.path,
                            local_hash: None,
                            remote_hash: Some(f.manifest_hash),
                            // Control-plane client: `fauna.sync.files` carries no
                            // per-file status (the wire `SyncFile` has none), so a
                            // listed file is available → `synced`. The full presence
                            // lifecycle is desktop-engine-only (file-sync.md
                            // § Per-file sync-status display; mirrors web's stance).
                            state: "synced".to_string(),
                        })
                        .collect();
                    UiMessage::Data(DataMessage::SyncFilesLoaded {
                        folder: fs.clone(),
                        files,
                    })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.sync.files".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Aggregate the Status page's Sync section ("Files Synced" / "Last Sync")
    /// across every device-sync folder binding (`sync_agent::current_locations()`)
    /// via `fauna.sync.files` per bound folder — the same wire call the Media
    /// page already uses (`fetch_sync_files`). Best-effort, per the
    /// `refresh_mail_epoch_schedule` posture: a set whose
    /// fetch fails is logged and excluded, not fatal to the aggregate (`status.md`
    /// footnote 4 — the section rendered a permanent "0"/"Never" placeholder,
    /// never wired to live data after initial paint). `current_locations()` reads a
    /// GTK-main-thread-local, so it must run on the calling thread before the
    /// background task is spawned — never inside it.
    pub fn fetch_sync_status_summary(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let folders = crate::sync_agent::current_locations();
        self.spawn_bg(async move {
            let sync = fauna_client_sync::SyncClient::new(nest_rpc);
            let mut files_synced: u64 = 0;
            let mut last_sync_at: Option<i64> = None;
            for folder in &folders {
                match sync.files(folder.folder.clone()).await {
                    Ok(reply) => {
                        files_synced += reply.files.len() as u64;
                        for f in &reply.files {
                            last_sync_at = Some(
                                last_sync_at.map_or(f.updated_at, |cur| cur.max(f.updated_at)),
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        "fetch_sync_status_summary: fauna.sync.files failed for {:?} \
                         (best-effort, excluded from the aggregate): {e}",
                        folder.folder
                    ),
                }
            }
            tx.send(UiMessage::Data(DataMessage::SyncStatusSummaryLoaded {
                files_synced,
                last_sync_at,
            }));
        });
    }

    // Sync-conflict resolution lives on the Peers/Devices page (the candidate
    // model: `views/peers/conflicts.rs` → `DevicesMachine::resolve_conflict`
    // over `fauna.sync.conflicts.resolve`). The old media-page keep-local/
    // remote/both dialog + its `POST /api/v1/sync/resolve-conflict` call were
    // removed in the WS-RPC rip-out — the route was deleted nest-side and the
    // dialog was unreachable (the WS `fauna.sync.files` reply carries no
    // per-file `state`, so no media row ever reached the `"conflict"` state).

    // -----------------------------------------------------------------------
    // Snapshots API
    // -----------------------------------------------------------------------

    /// Download one snapshot file's decrypted bytes via the shared
    /// client-side walk (`fauna_sync_engine::download_file_bytes_by_manifest`)
    /// and write them to `save_path` — the Rust-native twin of the FFI
    /// `download_snapshot_file_bytes` free fn
    /// (`libs/fauna-ffi/src/snapshot_download.rs`), minus the FFI hop: linux
    /// links `fauna-sync-engine` directly, so `manifest_hash` never needs to
    /// leave this process. `file` comes straight off the `BackupsMachine`'s
    /// already-fetched detail (no second server round trip to resolve path →
    /// manifest_hash); its `manifest_hash` is the machine record's **hex**,
    /// which is the shape that crosses every boundary uniformly.
    ///
    /// Only `"regular"` files carry a restorable manifest — the nest's value
    /// is `"regular"`, NOT `"file"` (`bins/fauna-nest/src/db/sync_storage.rs`)
    /// — a directory/symlink entry fails closed with a clear error instead of
    /// attempting a walk that cannot work.
    pub fn save_snapshot_file(
        &self,
        file: fauna_backups_machine::SnapshotFileRow,
        save_path: &str,
    ) {
        let context = "snapshot_file_downloaded".to_string();
        if file.file_type != "regular" {
            self.tx.send(UiMessage::Action(ActionResult::Failed {
                context,
                error: format!(
                    "{}: not a regular file — only regular files can be downloaded",
                    file.path
                ),
            }));
            return;
        }
        let raw: [u8; 32] = match hex32(&file.manifest_hash) {
            Some(r) => r,
            None => {
                self.tx.send(UiMessage::Action(ActionResult::Failed {
                    context,
                    error: format!("{}: manifest hash is not 32 hex-encoded bytes", file.path),
                }));
                return;
            }
        };
        let manifest_hash = fauna_core::data::ContentHash::from_digest_raw(raw);
        let relative_path = file.path.clone();
        let device_id = match crate::sync::device_id() {
            Ok(id) => id,
            Err(e) => {
                self.tx.send(UiMessage::Action(ActionResult::Failed {
                    context,
                    error: format!("device id: {e}"),
                }));
                return;
            }
        };

        let auth = self.build_sync_auth();
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let save_path = save_path.to_string();
        let tx = self.tx.clone();

        // The throwaway `SyncEngine` (`build_restore_engine`) owns an
        // in-memory rusqlite connection — `Send` but `!Sync`, so its future
        // is `!Send` and cannot run on `spawn_bg`'s multi-threaded runtime
        // (the standing `rusqlite` !Sync constraint). Run it on a
        // dedicated thread with its own
        // current-thread runtime; `tx` is safe to call from any thread —
        // every other `FaunaClient` async method already does so from a
        // tokio worker thread.
        if let Err(e) = std::thread::Builder::new()
            .name("fauna-snapshot-download".to_string())
            .spawn(move || {
                let msg = download_snapshot_file_and_save(
                    auth,
                    nest_rpc,
                    device_id,
                    &secret_hex,
                    manifest_hash,
                    &relative_path,
                    &save_path,
                    context,
                );
                tx.send(msg);
            })
        {
            self.tx.send(UiMessage::Action(ActionResult::Failed {
                context: "snapshot_file_downloaded".into(),
                error: format!("spawn snapshot-download worker: {e}"),
            }));
        }
    }

    // -----------------------------------------------------------------------
    // Message-kind snapshot restore — `fauna.filesync.snapshot.*` WS-RPC kinds
    //
    // These go through the shared `fauna-client-snapshots` crate (priority
    // #1/#2) rather than calling `nest_rpc.request(...)` directly — the
    // kind-composition is written once and lifted by the other 5 clients.
    // -----------------------------------------------------------------------

    /// Fetch the bearer's message-kind snapshots via
    /// `fauna.filesync.snapshot.list` (backs the `restore-snapshot-select`
    /// local picker). `message_kind = None` lists all kinds.
    pub fn fetch_message_kind_snapshots(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let snapshots = fauna_client_snapshots::SnapshotsClient::new(nest_rpc);
            let msg = match snapshots.list(None, None, 0).await {
                Ok(reply) => UiMessage::Data(DataMessage::MessageKindSnapshotsLoaded {
                    snapshots: reply.rows,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.filesync.snapshot.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the bearer's restore history via
    /// `fauna.filesync.snapshot.list_restore_history`.
    pub fn fetch_restore_history(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let snapshots = fauna_client_snapshots::SnapshotsClient::new(nest_rpc);
            let msg = match snapshots.list_restore_history(0).await {
                Ok(reply) => {
                    UiMessage::Data(DataMessage::RestoreHistoryLoaded { rows: reply.rows })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.filesync.snapshot.list_restore_history".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the divergence rows recorded against one snapshot's restore via
    /// `fauna.filesync.snapshot.list_restore_divergence` (per-row banner +
    /// modal). Fired once per restore-history row after the history lands.
    pub fn fetch_restore_divergence(&self, snapshot_id: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let snapshots = fauna_client_snapshots::SnapshotsClient::new(nest_rpc);
            let msg = match snapshots.list_restore_divergence(snapshot_id).await {
                Ok(reply) => UiMessage::Data(DataMessage::RestoreDivergenceLoaded {
                    snapshot_id,
                    rows: reply.rows,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.filesync.snapshot.list_restore_divergence".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Restore a message-kind snapshot via
    /// `fauna.filesync.snapshot.restore_message_kind`. `confirm_id` must
    /// equal `snapshot_id` stringified (the friction bar). On success the
    /// message loop re-fetches restore history and paints the reply's
    /// `config_present` advisory (`DataMessage::MessageKindRestored`).
    pub fn restore_message_kind(&self, snapshot_id: i64, confirm_id: String) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let snapshots = fauna_client_snapshots::SnapshotsClient::new(nest_rpc);
            let msg = match snapshots
                .restore_message_kind(snapshot_id, confirm_id)
                .await
            {
                // The reply is the only carrier of the `config_present`
                // advisory (`restore-warning`), so it rides the message typed.
                Ok(reply) => UiMessage::Data(DataMessage::MessageKindRestored {
                    config_present: reply.config_present,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "message_kind_restored".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Blob download API
    // -----------------------------------------------------------------------

    /// GET `path` from our nest and write the response body to `save_path`.
    /// On success sends `ActionResult::Success { context: "blob_downloaded" }`
    /// (both download paths share that context); `write_fail_context` is the
    /// `Failed` context used if the local write fails.
    fn download_to_file(
        &self,
        path: String,
        save_path: std::path::PathBuf,
        write_fail_context: &'static str,
    ) {
        let url = format!("{}{}", self.node_url, path);
        let nest = Arc::clone(&self.content_api);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match nest.get(&path).await {
                Ok(body) => match std::fs::write(&save_path, &body) {
                    Ok(()) => tx.send(UiMessage::Action(ActionResult::Success {
                        context: "blob_downloaded".into(),
                    })),
                    Err(e) => tx.send(UiMessage::Action(ActionResult::Failed {
                        context: write_fail_context.into(),
                        error: e.to_string(),
                    })),
                },
                Err(e) => tx.send(UiMessage::Action(ActionResult::Failed {
                    context: url,
                    error: e.to_string(),
                })),
            }
        });
    }

    /// Download a blob by hash and save it to `save_path`.
    /// Used for message attachment downloads.
    pub fn download_blob(&self, hash: &str, save_path: std::path::PathBuf) {
        self.download_to_file(paths::blob::by_hash(hash), save_path, "blob_write");
    }

    /// Fetch a blob's raw bytes from `/api/v1/blob/{hash}` on the tokio
    /// runtime, delivering the result over a one-shot `async-channel` the GTK
    /// thread consumes (via `glib::spawn_future_local`) to paint a feed
    /// `post-image`. Kept separate from `download_blob` (which writes a file +
    /// emits a generic `ActionResult`) because feed-image rendering needs the
    /// bytes in memory, tied to a specific widget.
    pub fn fetch_blob_bytes(
        &self,
        hash: &str,
    ) -> async_channel::Receiver<Result<Vec<u8>, crate::nest_content_api::ApiError>> {
        self.fetch_nest_bytes(paths::blob::by_hash(hash))
    }

    /// Fetch the raw bytes at a nest-relative `path` (bearer-carrying, bulk
    /// plane) the way [`Self::fetch_blob_bytes`] fetches a blob — a bridged
    /// post's `ProxiedImage` path is the same call with a different argument
    /// (render-model.md § D6c).
    pub fn fetch_nest_bytes(
        &self,
        path: String,
    ) -> async_channel::Receiver<Result<Vec<u8>, crate::nest_content_api::ApiError>> {
        let (tx, rx) = async_channel::bounded(1);
        let nest = Arc::clone(&self.content_api);
        self.spawn_bg(async move {
            let res = nest.get(&path).await.map(|b| b.to_vec());
            let _ = tx.send(res).await;
        });
        rx
    }

    /// Check a blob's `x-c2pa` provenance hint via `HEAD /api/v1/blob/{hash}`
    /// (`ui/media.md` § C2PA provenance) — the `c2pa-badge` gate, tui's
    /// `Op::FetchC2pa` twin. A HEAD request, not a `GET`: the badge check must
    /// not pull the whole blob just to read one response header. An error
    /// degrades to "no badge" at the call site, same as a resolved `false` —
    /// `has_c2pa` is a UI hint, not a security boundary.
    pub fn fetch_has_c2pa(
        &self,
        hash: &str,
    ) -> async_channel::Receiver<Result<bool, crate::nest_content_api::ApiError>> {
        let (tx, rx) = async_channel::bounded(1);
        let nest = Arc::clone(&self.content_api);
        let path = paths::blob::by_hash(hash);
        self.spawn_bg(async move {
            let res = nest.head_has_c2pa(&path).await;
            let _ = tx.send(res).await;
        });
        rx
    }

    // -----------------------------------------------------------------------
    // Quota / Node-info API
    // -----------------------------------------------------------------------

    /// Fetch quota info (tier, inbox usage, storage usage) via
    /// `fauna.quota.get` (WS-RPC). The typed `QuotaGetReply` is re-serialized to
    /// the same JSON shape the HTTP twin returned (the reply type mirrors it
    /// field-for-field), so the existing `QuotaLoaded` consumer is unchanged.
    pub fn fetch_quota(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let msg = match account.quota_get().await {
                Ok(reply) => UiMessage::Data(DataMessage::QuotaLoaded {
                    quota: serde_json::to_value(&reply).unwrap_or(serde_json::Value::Null),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.quota.get".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the gated-feature plane's transparency read (`feature-limits-
    /// section` — `dynamic-features.md` § Transparency & auditability) via the
    /// shared `FeaturesClient::rows()` — the one call an app makes, already
    /// joining `fauna.features.status` with `fauna.nest.info`'s capability set
    /// and folding both into ready-to-render rows (priority #2: no client
    /// recomposes the tier meet).
    pub fn fetch_features(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let features_client = fauna_client_features::FeaturesClient::new(nest_rpc);
            let msg = match features_client.rows().await {
                Ok(rows) => UiMessage::Data(DataMessage::FeaturesLoaded { rows }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.features.status".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Ask the nest's region relay (`fauna.region.artifact.get`) for every
    /// policy on the device's declared chain — at login (`force`) and whenever
    /// the shared cadence is due. Nothing declared, or nothing due, asks
    /// nothing. The answers fold on the GTK main thread
    /// (`DataMessage::RegionReplies`).
    pub fn fetch_region(&self, force: bool) {
        let Some(chain) = crate::region::chain_to_refresh(force) else {
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let replies = fauna_client_region::fetch_chain(nest_rpc.as_ref(), &chain).await;
            tx.send(UiMessage::Data(DataMessage::RegionReplies { replies }));
        });
    }

    /// Fetch the authenticated user's own account state via `fauna.account.get`
    /// (WS-RPC). Used to populate the handle in the status bar and
    /// Settings → Account, which the auth handshake doesn't return.
    pub fn fetch_account(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let msg = match account.get().await {
                Ok(reply) => UiMessage::Data(DataMessage::AccountLoaded {
                    handle: reply.handle,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.account.get".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch node info (domain, version) over the WS-RPC `fauna.nest.info` kind
    /// — the live, authenticated connection — instead of the (now deleted) HTTP
    /// `/api/v1/node-info` twin. Both resolved the same `nest_info_core` on the
    /// nest, but the HTTP twin was unreachable on a deployed HTTPS nest, so
    /// Settings > Status > Node > Domain sat at "—". `NestInfoReply` carries the
    /// `domain` + `version` the status view reads.
    pub fn fetch_nest_info(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};
            let reply: Result<NestInfoReply, _> = nest_rpc
                .request("fauna.nest.info", NestInfoRequest::default())
                .await;
            let info = match reply {
                Ok(r) => serde_json::json!({ "domain": r.domain, "version": r.version }),
                Err(e) => {
                    tracing::warn!(error = %e, "fauna.nest.info request failed");
                    serde_json::Value::Null
                }
            };
            tx.send(UiMessage::Data(DataMessage::NestInfoLoaded { info }));
        });
    }

    // -----------------------------------------------------------------------
    // Full-text search API
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Settings — Account API
    // -----------------------------------------------------------------------

    /// Change the authenticated actor's handle via `fauna.profile.handle.change`
    /// (WS-RPC). The change is queued as a pending action (delayed +
    /// cancellable); success here means "scheduled", not "applied".
    pub fn change_handle(&self, new_handle: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let new_handle = new_handle.to_string();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let msg = match account.change_handle(new_handle).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "handle_changed".into(),
                }),
                // The nest's refusal already reads in the user's language
                // (`RpcError::localized()` — a taken handle, a cooldown), so it
                // is the whole banner, never behind the RPC method name: the
                // user must read the nest's reason (`settings.md` § User
                // actions). tui's `Outcome::HandleFailed`.
                Err(e) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Permanently delete the authenticated account via `fauna.account.delete`
    /// (WS-RPC). Queued as a pending action with a cancellation window.
    pub fn delete_account(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let msg = match account.delete().await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "account_deleted".into(),
                }),
                // Localized by the shared `RpcError::localized()`, as above.
                Err(e) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Settings — Pending actions API (`settings.md` § Pending actions)
    // -----------------------------------------------------------------------

    /// List this actor's still-`pending` scheduled actions via
    /// `fauna.pending_actions.list` — the cancellation window the three
    /// delayed verbs (handle change, account delete, snapshot delete) open.
    /// Fired on Settings/Account page build, on every page-visible refresh,
    /// and after either delayed verb this page hosts completes.
    pub fn fetch_pending_actions(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let result = list_pending_actions(&account).await;
            tx.send(UiMessage::Data(DataMessage::PendingActionsLoaded {
                result,
            }));
        });
    }

    /// Cancel a scheduled action via `fauna.pending_actions.cancel` (one
    /// click, no confirm — cancelling is the safe direction). Ends on a
    /// fresh list read so the section reflects the removal immediately; a
    /// cancel failure leaves the list untouched and surfaces through the
    /// generic error path instead.
    pub fn cancel_pending_action(&self, id: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let msg = match account.pending_action_cancel(id).await {
                Ok(_) => UiMessage::Data(DataMessage::PendingActionsLoaded {
                    result: list_pending_actions(&account).await,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.pending_actions.cancel".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Settings — Recovery kit API (`settings.md` § Recovery kit)
    // -----------------------------------------------------------------------

    /// Read this identity's recovery-kit status via `fauna_client_recovery::kit_status`
    /// — off the registration chain, never a local flag, so a kit created on
    /// another device is reflected here. Fired on Settings/Account page build
    /// and on every page-visible refresh.
    pub fn fetch_recovery_status(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = match recovery_identity(&secret_hex) {
                Ok(identity) => {
                    let client = fauna_client_recovery::RecoveryClient::new(nest_rpc);
                    fauna_client_recovery::kit_status(&client, &identity.actor_id())
                        .await
                        .map_err(|e| e.to_string())
                }
                Err(e) => Err(e),
            };
            tx.send(UiMessage::Data(DataMessage::RecoveryStatusLoaded {
                result,
            }));
        });
    }

    /// Mint the first RecoveryKey registration (`recovery-kit-create-button`,
    /// live only in `NeverCreated`) — `create_kit` with `prior: None`. Also
    /// reachable for a successor retrofitting an owed kit that never landed,
    /// which is why predecessors are still resolved here even though this arm
    /// cannot overwrite a resting escrow section (nothing rests in
    /// `NeverCreated`) — sealing `&[]` there would mint the kit without the
    /// device-loss backstop (tui's `Action::RecoveryCreateKit`, same reasoning).
    pub fn create_recovery_kit(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = recovery_create_kit(nest_rpc, &secret_hex, None).await;
            tx.send(UiMessage::Data(DataMessage::RecoveryKitMinted { result }));
        });
    }

    /// Replace the registered kit using the one the user holds
    /// (`recovery-kit-replace-button`) — the second `create_kit` arm,
    /// `prior: Some(..)`. The shared core picks the arm off the chain head and
    /// re-puts the escrow blob in the same ceremony: the nest deletes the
    /// resting row the moment a registration changes the pubkey, so skipping
    /// the re-put would leave the account with no escrow at all.
    pub fn replace_recovery_kit(&self, phrase: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let phrase = phrase.to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = match fauna_client_recovery::parse_kit(&phrase) {
                Ok(prior) => {
                    recovery_create_kit(nest_rpc, &secret_hex, Some(&prior.recovery)).await
                }
                Err(e) => Err(e.to_string()),
            };
            tx.send(UiMessage::Data(DataMessage::RecoveryKitMinted { result }));
        });
    }

    /// Open a seed-alone replacement window (`recovery-kit-lost-button`) at the
    /// bound nest and every linked nest —
    /// `request_seed_alone_replacement_everywhere`. Unlike create/replace this does not
    /// move the chain head (the request only lands after the 30-day window),
    /// so no profile mirror runs here.
    pub fn request_recovery_kit_lost(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = match recovery_identity(&secret_hex) {
                Ok(identity) => {
                    let client = fauna_client_recovery::RecoveryClient::new(nest_rpc);
                    let dial = NativeLinkedNestDial::new(&identity);
                    let minted = fauna_client_recovery::request_seed_alone_replacement_everywhere(
                        &client, &identity, &dial,
                    )
                    .await
                    .map(|(pending, _linked)| pending.secret_hex().to_string())
                    .map_err(|e| e.to_string());
                    with_fresh_status(&client, &identity, minted).await
                }
                Err(e) => Err(e),
            };
            tx.send(UiMessage::Data(DataMessage::RecoveryKitMinted { result }));
        });
    }

    /// Contest the pending seed-alone replacement with the kit the user holds
    /// (`recovery-pending-veto-button`) at the bound nest and every linked
    /// nest — the shared `veto_with_status`, which folds the status re-read
    /// into the same task. The re-read status is the whole receipt: the
    /// countdown line and the button disappear. The failure is localized here
    /// so the section renders one message shape.
    pub fn veto_recovery_replacement(&self, phrase: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let phrase = phrase.to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = match recovery_identity(&secret_hex) {
                Ok(identity) => {
                    let client = fauna_client_recovery::RecoveryClient::new(nest_rpc);
                    fauna_client_recovery::veto_with_status(
                        &client,
                        identity.actor_id(),
                        &phrase,
                        &NativeLinkedNestDial::new(&identity),
                    )
                    .await
                    .map(|(_cancelled, status)| status)
                }
                Err(e) => Err(e),
            }
            .map_err(|e| crate::i18n::strings::settings::recovery_kit::veto_failed(&e));
            tx.send(UiMessage::Data(DataMessage::RecoveryKitRepaired { result }));
        });
    }

    /// The no-escrow repair (`recovery-kit-escrow-reseal-button`, rendered only
    /// in `RegisteredNoEscrow`): re-put the sealed seed under the kit already in
    /// hand, WITHOUT retiring it — the shared `reseal_escrow_with_status`.
    /// Predecessor seeds resolve exactly as for `create_kit`'s escrow write.
    pub fn reseal_recovery_escrow(&self, phrase: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let phrase = phrase.to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let result = match recovery_identity(&secret_hex) {
                Ok(identity) => {
                    let client = fauna_client_recovery::RecoveryClient::new(nest_rpc);
                    let predecessors = escrow_predecessor_seeds(&secret_hex);
                    fauna_client_recovery::reseal_escrow_with_status(
                        &client,
                        &identity,
                        &phrase,
                        &predecessors,
                    )
                    .await
                }
                Err(e) => Err(e),
            }
            .map_err(|e| crate::i18n::strings::settings::recovery_kit::action_failed(&e));
            tx.send(UiMessage::Data(DataMessage::RecoveryKitRepaired { result }));
        });
    }

    /// The succession ceremony (`identity-stolen-button`): re-point the account
    /// to a freshly minted successor, using the kit the user holds.
    ///
    /// **In-process, not over `libs/fauna-ffi`.** linux is Rust-native like tui,
    /// so it consumes `fauna_client_recovery` directly; the
    /// `succession_succeed_with_held_kit` UniFFI face is apple's and windows'
    /// route to the same shared code, not a layer this app goes through.
    ///
    /// ⚠ **No status re-read is folded in here, unlike every other ceremony
    /// above.** The succession transaction revokes this connection's bearer, so
    /// a re-read would race a dying session and report a failure that means
    /// nothing. The status the user sees next is the SUCCESSOR's, read after the
    /// switch by the section's own on-visible refresh.
    pub fn succeed_identity(&self, phrase: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let phrase = phrase.to_string();
        let node_url = self.node_url().to_string();
        let tx = self.tx.clone();
        // Taken HERE, while the app is still signed in as the identity being
        // succeeded — the one moment the old engine is both live and ours. The
        // ceremony's sweep must never open a second engine over the same
        // `mls_state.db`: two engines over one store is user-irrecoverable
        // corruption, which is why the live handle is reused rather than rebuilt.
        let old_engine =
            crate::conversations::conv_backend::active_session().map(|session| session.engine());
        self.spawn_bg(async move {
            let outcome =
                do_succeed_identity(nest_rpc, &secret_hex, &phrase, &node_url, old_engine).await;
            tx.send(UiMessage::Data(DataMessage::RecoverySucceeded {
                outcome: Box::new(outcome),
            }));
        });
    }

    /// Finish a group sweep the ceremony did not
    /// (`recovery-kit-sweep-retry-button`).
    ///
    /// Takes NO phrase: the retry re-acquires the landed statement from the
    /// chain rather than from a kit, so demanding one would gate a repair on a
    /// credential the ceremony already told the user to retire. Everything it
    /// needs survived the account switch — the retired identity's registry row
    /// keeps the seed and its scoped MLS store is still on disk — so **nothing
    /// new is stored at rest to make this possible**.
    pub fn retry_group_sweep(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        // The LIVE successor engine when conversations are up — never a second
        // over the same store. `None` when they never came up, in which case the
        // shared driver constructs one over the successor's own scope, exactly
        // as the ceremony's sweep does.
        let successor_engine =
            crate::conversations::conv_backend::active_session().map(|session| session.engine());
        self.spawn_bg(async move {
            let result =
                do_retry_group_sweep(nest_rpc, &secret_hex, successor_engine, tx.clone()).await;
            tx.send(UiMessage::Data(DataMessage::SweepRetried { result }));
        });
    }

    /// Discharge the group sweep a relaunch adoption owes — the unbidden press
    /// of `recovery-kit-sweep-retry-button` the post-auth hook makes for the
    /// successor (`recovery_kit::SUCCESSION_SWEEP_OWED`). Same engine choice as
    /// [`Self::retry_group_sweep`]; the fold parks whatever the answer says to.
    pub fn discharge_owed_sweep(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let successor_engine =
            crate::conversations::conv_backend::active_session().map(|session| session.engine());
        self.spawn_bg(async move {
            let result =
                do_discharge_owed_sweep(nest_rpc, &secret_hex, successor_engine, tx.clone()).await;
            tx.send(UiMessage::Data(DataMessage::OwedSweepDischarged {
                result,
                successor: actor_id_from_secret_hex(&secret_hex).unwrap_or_default(),
            }));
        });
    }

    // -----------------------------------------------------------------------
    // Settings — Privacy API
    // -----------------------------------------------------------------------

    /// Fetch the inbox mode for the authenticated actor via
    /// `fauna.inbox.mode.get` (WS-RPC).
    pub fn fetch_inbox_mode(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.inbox_mode_get().await {
                Ok(reply) => UiMessage::Data(DataMessage::InboxModeLoaded { mode: reply.mode }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.inbox.mode.get".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Set the inbox mode for the authenticated actor via
    /// `fauna.inbox.mode.set` (WS-RPC). `mode` is one of: "open", "knocks",
    /// "contacts", "closed".
    ///
    /// Discarding the reply is correct: `InboxModeSetReply` is ack-only (it
    /// carries nothing but `extra`), so there is no echoed state to feed back
    /// — audited 2026-08-04 against the stale-cache class. The live
    /// defect on this pair is on the GET side: `InboxModeLoaded` is dropped by
    /// `app.rs`, so the privacy page never pre-selects the account's real mode.
    pub fn set_inbox_mode(&self, mode: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let mode = mode.to_string();
        self.spawn_bg(async move {
            let contacts = fauna_client_contacts::ContactsClient::new(nest_rpc);
            let msg = match contacts.inbox_mode_set(mode).await {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "inbox_mode_set".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.inbox.mode.set".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // Email filter list/create/get/update/delete now live entirely in
    // settings/email_filters.rs, which calls fauna_client_email::EmailClient
    // directly off its own tokio runtime handle (spawn_with_snapshot) rather
    // than routing through this UiMessage/DataMessage dispatch — see that
    // module's doc comment for why.

    /// Fetch spam preferences via `fauna.spam.get_preferences` (WS-RPC). The
    /// subject is implicit — the connection knows its caller. Errors surface
    /// as `ActionResult::Failed` (mirrors `fetch_bridge_feeds`).
    pub fn fetch_spam_preferences(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let spam = fauna_client_spam::SpamClient::new(nest_rpc);
            let req = fauna_client_spam::spam::SpamGetPreferencesRequest {
                extra: Default::default(),
            };
            let msg = match spam.get_preferences(req).await {
                Ok(prefs) => UiMessage::Data(DataMessage::SpamPreferencesLoaded { prefs }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.spam.get_preferences".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Update spam preferences via `fauna.spam.set_preferences` (WS-RPC).
    /// Partial update — only the `Some(_)` fields of `req` change server-side;
    /// the nest clamps the thresholds to `[0.0, 1.0]`. The handler echoes
    /// the resulting full
    /// `SpamPreferences`, and we feed that echo back through
    /// `SpamPreferencesLoaded` **before** the success toast, so
    /// `content_policy::set_spam_preferences` re-binds the viewer's own
    /// thresholds to the render engine immediately: a user who moves their spam
    /// slider expects the next feed/thread paint to honour it, not the next
    /// launch (tui does the same on `Outcome::SpamPrefsSaved`).
    ///
    /// This echo used to be discarded, which made `app.rs`'s
    /// "re-set on every privacy-page save" comment false and left the
    /// own-threshold collapse stale for the rest of the session — caught by
    /// `test_moderation_client_model_write.py` once that test began arranging
    /// the threshold through the UI (2026-08-03).
    pub fn update_spam_preferences(&self, req: fauna_client_spam::spam::SpamSetPreferencesRequest) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let spam = fauna_client_spam::SpamClient::new(nest_rpc);
            let msg = match spam.set_preferences(req).await {
                Ok(prefs) => {
                    // Re-bind the just-saved thresholds to the render engine
                    // before reporting success (see the doc comment above).
                    tx.send(UiMessage::Data(DataMessage::SpamPreferencesLoaded {
                        prefs,
                    }));
                    UiMessage::Action(ActionResult::Success {
                        context: "spam_preferences_updated".into(),
                    })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.spam.set_preferences".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Settings — Encryption API
    // -----------------------------------------------------------------------

    /// Fetch the number of available MLS key packages for the authenticated
    /// actor via `fauna.conversations.keypackage.count`.
    pub fn fetch_key_package_count(&self) {
        let Some(actor_id) = self.actor_id() else {
            tracing::warn!("fetch_key_package_count: not authenticated");
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let convos = ConversationsClient::new(nest_rpc);
            let msg = match convos.keypackage_count(actor_id).await {
                Ok(reply) => UiMessage::Data(DataMessage::KeyPackageCountLoaded {
                    count: reply.count as u32,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.conversations.keypackage.count".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // `publish_key_packages_real` (a bare `keypackage_upload` of raw-minted
    // packages) was removed alongside `MlsManager::generate_key_packages`: that
    // pair minted + uploaded without ticking the replica autosave, so a later
    // provider swap wiped the fresh private init keys. Replenish now flows through
    // the durable `ConversationsManager::ensure_keypackages` surface
    // (`conversations::conv_backend::replenish_key_packages`), which both mints on
    // the session engine and notifies the autosave (`devices.md` § Cross-device
    // MLS group-state sync).

    // -----------------------------------------------------------------------
    // Admin API
    // -----------------------------------------------------------------------

    /// Check whether the authenticated user is a nest admin via
    /// `fauna.account.am_i_admin` (WS-RPC). Mirrors the HTTP twin's
    /// fail-soft behavior: any error hides admin UI (`is_admin = false`) rather
    /// than surfacing a failure.
    pub fn check_admin_status(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let account = fauna_client_account::AccountClient::new(nest_rpc);
            let is_admin = account.am_i_admin().await.map(|r| r.admin).unwrap_or(false);
            tx.send(UiMessage::Data(DataMessage::AdminStatusLoaded { is_admin }));
        });
    }

    // -----------------------------------------------------------------------
    // Family safety (`fauna.family.*`)
    // -----------------------------------------------------------------------

    /// Read `fauna.family.status` once per session (post-auth) to drive the two
    /// gated family surfaces: the `family-tab` sidebar row (shown when the
    /// caller guards someone **or** is supervised) and the global
    /// `supervised-indicator` (shown only when supervised) —
    /// `docs/goal/behavior/family-safety.md` § App surface.
    ///
    /// A failed read sends **no message** (`family_status_loaded` → `None`):
    /// the gated surfaces keep their fail-closed launch state (hidden), and the
    /// enforcement inputs (`content_policy` / `screen_lock`) keep whatever they
    /// hold — family-safety.md § Content policy's unfetched-policy ruling
    /// clause 1: "read failed" and "read says unsupervised" are different
    /// facts, and only the second may clear them. (The old error arm sent the
    /// same `FamilyStatusLoaded` with an all-`None` payload, collapsing the
    /// two — latent here while the read fires once per session, but it becomes
    /// a floor-evaporating bug the moment a reconnect re-read lands.)
    pub fn check_family_status(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let family = fauna_client_family::FamilyClient::new(nest_rpc);
            if let Some(msg) = family_status_loaded(family.status().await) {
                tx.send(UiMessage::Data(msg));
            }
        });
    }

    /// Guardian Notify (`family-safety.md` § Guardian Notify): drain the ward-side
    /// per-category enforcement counter and, if a batch is due (≤ hourly — the
    /// accumulator's own gate), report it coarsely via `fauna.family.notify_report`
    /// (category + count, **never content**). Called on the GTK main thread by the
    /// notify-flush tick, so the thread-local drain is valid; the RPC is spawned.
    pub fn flush_notify_report(&self) {
        let Some((entries, offset)) = crate::content_policy::take_notify_report() else {
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        self.spawn_bg(async move {
            let family = fauna_client_family::FamilyClient::new(nest_rpc);
            if let Err(e) = family.notify_report(entries, offset).await {
                tracing::debug!("fauna.family.notify_report failed (best-effort): {e}");
            }
        });
    }

    /// Screen time (`family-safety.md` § Screen time): drive the ward's usage
    /// heartbeat one step and, if the shared engine says a report is due, send
    /// `fauna.family.usage_report(minutes, utc_offset_minutes)`.
    ///
    /// `focused` is whether the app window has focus; the engine turns that
    /// (minus the lock) into the foreground delta. A `0`-minute report is a
    /// **read** — the goal doc's own term — and is what refreshes the day's
    /// total for a locked-out ward, which is how the lock lifts at local
    /// midnight or after a guardian raises the budget. Every reply repaints the
    /// lock, so crossing the budget is visible without waiting for the separate
    /// one-minute lock tick.
    ///
    /// Called on the GTK main thread by the heartbeat tick, so the thread-local
    /// drain is valid; the RPC is spawned. A failure re-credits the minutes
    /// rather than forgiving them — the delta is defined against the last
    /// *successful* report.
    pub fn flush_usage_report(&self, focused: bool) {
        let Some((minutes, offset)) = crate::screen_lock::take_due_report(focused) else {
            return;
        };
        // The engine has already moved those minutes to in-flight, so every
        // path from here MUST answer it. A torn-down runtime (sign-out raced
        // the tick) answers `report_failed` rather than returning — leaving the
        // report in flight would wedge the heartbeat for the rest of the
        // session, silently ending a ward's accounting.
        let Some(rt) = self.runtime.borrow().as_ref().map(|r| r.handle().clone()) else {
            crate::screen_lock::report_failed();
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        crate::async_helper::spawn_with_snapshot(
            &rt,
            move || async move {
                let family = fauna_client_family::FamilyClient::new(nest_rpc);
                match family.usage_report(minutes, offset).await {
                    Ok(reply) => Some((reply.day, reply.day_total_minutes)),
                    Err(e) => {
                        tracing::debug!(
                            "fauna.family.usage_report failed (retried next tick): {e}"
                        );
                        None
                    }
                }
            },
            move |result| match result {
                Some((day, total)) => {
                    crate::screen_lock::report_succeeded(day, total);
                    crate::screen_lock::repaint();
                }
                None => crate::screen_lock::report_failed(),
            },
        );
    }

    /// Fetch the deployment's local mail domains over WS-RPC via the shared
    /// `LocalDomainMachine` (`fauna.bridges.list_local_domains`), for the admin
    /// Settings page's email-domains section. Display-only: dispatches
    /// `Refresh` and ships the resulting snapshot (which also carries any
    /// error). No HTTP — the legacy admin pages still use the REST surface,
    /// but new admin UI is WS-RPC per the no-HTTP directive.
    pub fn fetch_local_domains(&self) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(Arc::clone(&nest_rpc));
            // `Refresh` records any error into the snapshot (unlike `hydrate`).
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let snapshot = machine.snapshot();
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot,
            }));
        });
    }

    /// Add a local mail domain (admin) from the `admin-dns` page. Mirrors the
    /// `approve_pending_bridge` shape: `Refresh` first so the list is populated
    /// even if the add errors, then `AddDomain` (which re-reads on success so the
    /// new row appears), then ship the snapshot. `cert_mode` uses the shared
    /// add-time default — the admin-dns add form is name-only; the per-domain
    /// advanced knobs are the deferred rich wizard, and the MTA-STS policy mode
    /// is the nest's own to set and advance. No HTTP.
    ///
    /// The nest mints the new domain's initial DKIM key as it adds the domain
    /// (`mail-bridge-lifecycle.md` § DKIM provisioning) — the client mints no key.
    pub fn add_local_domain(&self, domain: String) {
        use fauna_client_mail_settings::local_domains::{DEFAULT_CERT_MODE, LocalDomainAction};
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(Arc::clone(&nest_rpc));
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::AddDomain {
                    domain: domain.clone(),
                    mta_sts_cert_mode: DEFAULT_CERT_MODE.to_string(),
                })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Soft-delete (remove) a local mail domain (admin) from `admin-dns`. The
    /// nest refuses the primary (`cannot_remove_primary_domain`); that error
    /// surfaces in the shipped snapshot. Same `Refresh`-then-act shape.
    pub fn remove_local_domain(&self, domain: String) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::RemoveDomain { domain })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Restore a soft-deleted local mail domain (admin) from `admin-dns` (within
    /// the 30-day window). Same `Refresh`-then-act shape.
    pub fn restore_local_domain(&self, domain: String) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::RestoreDomain { domain })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Designate (`Some`) or clear (`None`) a domain's catch-all actor (admin)
    /// from the `admin-dns` per-domain row (`admin-dns-domain-catch-all-select`).
    /// Same `Refresh`-then-act shape as the other local-domain mutations; the
    /// nest writes `mail_domains.catch_all_actor_id` and any error surfaces in the
    /// shipped snapshot. Resolver consumption of the designation is a separate
    /// track (mail-multidomain.md § Implementation status today).
    pub fn set_catch_all_actor(&self, domain: String, actor_id: Option<Vec<u8>>) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::SetCatchAllActor { domain, actor_id })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Designate (`Some`) or clear (`None`) the per-domain override actor for one
    /// role address (postmaster/abuse/noc/security; admin) from the `admin-dns`
    /// per-domain row (`admin-dns-domain-role-address-<role>-select`). Same
    /// `Refresh`-then-act shape as `set_catch_all_actor`; the nest atomic-merges
    /// `mail_domains.role_address_overrides` (the other roles are preserved) and
    /// any error surfaces in the shipped snapshot (mail-multidomain.md § Per-domain
    /// role-address routing).
    pub fn set_role_address(
        &self,
        domain: String,
        role: fauna_client_mail_settings::local_domains::RoleAddressKind,
        actor_id: Option<Vec<u8>>,
    ) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::SetRoleAddress {
                    domain,
                    role,
                    actor_id,
                })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Start the primary-domain rename (admin) from the `admin-dns` rename sheet:
    /// promote `new_primary_domain_id` (an existing *additional* — the two-step
    /// rule) to primary, with an optional `grace_days` override (`None` → the nest
    /// default 7; range `[1, 30]`). Same `Refresh`-then-act shape as the other
    /// local-domain mutations; the nest validates every precondition
    /// (single-active-rename / new-primary-is-additional / cert-mode / TLS-posture
    /// / SAN-cap) and a 409-class refusal surfaces in the shipped snapshot (the
    /// page error-message) — the client duplicates no rules
    /// (mail-primary-domain-rename.md § UX surface). No HTTP.
    pub fn start_primary_rename(&self, new_primary_domain_id: Vec<u8>, grace_days: Option<i64>) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::StartPrimaryRename {
                    new_primary_domain_id,
                    grace_days,
                })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Finalize the in-flight primary-domain rename (admin) from the `admin-dns`
    /// banner. `rename_id` scopes it (from the snapshot's `active_rename`); `force`
    /// completes early from `grace` (accepting the cache-flush risk the confirm
    /// dialog names) — else the nest requires the grace window to have elapsed
    /// (`grace_period_not_expired`). Same `Refresh`-then-act shape; any refusal
    /// surfaces in the shipped snapshot. No HTTP.
    pub fn complete_primary_rename(&self, rename_id: Vec<u8>, force: bool) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::CompletePrimaryRename { rename_id, force })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Extend the in-flight rename's grace window (admin) by `additional_days`
    /// (`[1, 30]` per call). Valid from `grace` / `ready_to_complete`; the nest
    /// pushes `grace_ends_at` out and a `ready_to_complete` row reverts to `grace`.
    /// Same `Refresh`-then-act shape; any refusal surfaces in the shipped snapshot.
    /// No HTTP.
    pub fn extend_primary_rename_grace(&self, rename_id: Vec<u8>, additional_days: i64) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::ExtendPrimaryRenameGrace {
                    rename_id,
                    additional_days,
                })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Abort the in-flight primary-domain rename (admin) from the `admin-dns`
    /// banner. `rename_id` scopes it; `reason` is an optional audit string. Cheap
    /// pre-flip (nothing to unwind); the expensive inverse re-flip post-flip (the
    /// confirm dialog names the cost). Same `Refresh`-then-act shape; any refusal
    /// surfaces in the shipped snapshot. No HTTP.
    pub fn abort_primary_rename(&self, rename_id: Vec<u8>, reason: Option<String>) {
        use fauna_client_mail_settings::local_domains::LocalDomainAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_local_domains_machine(nest_rpc);
            let _ = machine.dispatch(LocalDomainAction::Refresh).await;
            let _ = machine
                .dispatch(LocalDomainAction::AbortPrimaryRename { rename_id, reason })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminLocalDomainsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Fetch the pending-bridge approval feed over WS-RPC via the shared
    /// `BridgeApprovalMachine` (`fauna.bridges.list_pending_bridges`), for the
    /// admin `admin-bridges-pending` page. Dispatches `Refresh` (records any
    /// error into the snapshot) and ships the resulting snapshot. No HTTP.
    pub fn fetch_pending_bridges(&self) {
        use fauna_client_mail_settings::BridgeApprovalAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_bridge_approval_machine(nest_rpc);
            let _ = machine.dispatch(BridgeApprovalAction::Refresh).await;
            tx.send(UiMessage::Data(DataMessage::AdminPendingBridgesLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Approve a pending bridge (admin). `pubkey_hex`/`role` come from the
    /// rendered `admin-bridges-pending-card`. We `Refresh` first so the feed is
    /// populated even if the approve errors (the machine starts empty); the
    /// `Approve` path re-reads on success, so the shipped snapshot already drops
    /// the approved card.
    pub fn approve_pending_bridge(&self, pubkey_hex: String, role: String) {
        use fauna_client_mail_settings::BridgeApprovalAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_bridge_approval_machine(nest_rpc);
            let _ = machine.dispatch(BridgeApprovalAction::Refresh).await;
            let _ = machine
                .dispatch(BridgeApprovalAction::Approve { pubkey_hex, role })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminPendingBridgesLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Reject (revoke) a pending bridge (admin). Same `Refresh`-then-act shape as
    /// [`approve_pending_bridge`]; the rejected card drops from the feed.
    pub fn reject_pending_bridge(&self, pubkey_hex: String) {
        use fauna_client_mail_settings::BridgeApprovalAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_bridge_approval_machine(nest_rpc);
            let _ = machine.dispatch(BridgeApprovalAction::Refresh).await;
            let _ = machine
                .dispatch(BridgeApprovalAction::Reject { pubkey_hex })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminPendingBridgesLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Rotate an approved bridge's service-user key (admin). Same
    /// `Refresh`-then-act shape as [`reject_pending_bridge`]; the rotated bridge
    /// is revoked (drops from the approved roster) and re-enrolls +
    /// auto-approves on a mail-enabled box (`mail-bridge-lifecycle.md`
    /// § Service-user re-keying).
    pub fn rotate_service_user(&self, pubkey_hex: String) {
        use fauna_client_mail_settings::BridgeApprovalAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_bridge_approval_machine(nest_rpc);
            let _ = machine.dispatch(BridgeApprovalAction::Refresh).await;
            let _ = machine
                .dispatch(BridgeApprovalAction::Rotate { pubkey_hex })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminPendingBridgesLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Fetch the nest-wide custody-hosting registry
    /// (`fauna.admin.custody_hosting.list`, via the shared `AdminHostingClient`)
    /// for the admin `admin-custody-hosting` page. Folded by the shared
    /// `admin_hosting_rows` projection so every lift app renders the same rows
    /// in the same order (priority #2) — see `apps/fauna-tui/src/admin/mod.rs`'s
    /// `load_custody_hosting_snapshot`, the reference this mirrors.
    pub fn fetch_custody_hosting(&self) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            tx.send(UiMessage::Data(DataMessage::AdminCustodyHostingLoaded {
                snapshot: load_custody_hosting_snapshot(&nest, None).await,
            }));
        });
    }

    /// Drop one custody-hosting row (`fauna.admin.custody_hosting.remove`),
    /// keyed by the `(host, grant)` pair the list serves — never the painted
    /// index, which a re-read can reorder. Re-reads either way: a failed
    /// remove still needs the registry's own answer on what is still there,
    /// or a stale surface after a failure would let an admin believe a row is
    /// gone when it is not.
    pub fn remove_custody_hosting(&self, host_actor_id: String, grant_id: Vec<u8>) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let status = match fauna_client_capabilities::custody_hosting::AdminHostingClient::new(
                nest.clone(),
            )
            .remove(&host_actor_id, &grant_id)
            .await
            {
                // `removed: false` is an honest no-op, not a failure: the row
                // was already gone (a sibling admin, or the host's own
                // reclaim). Saying so beats an error line that would claim
                // the opposite of what happened.
                Ok(reply) if !reply.removed => {
                    Some(crate::i18n::strings::admin::custody_hosting::REMOVE_MISSING.to_string())
                }
                Ok(reply) if reply.store_dropped => Some(
                    crate::i18n::strings::admin::custody_hosting::REMOVED_WITH_STORE.to_string(),
                ),
                Ok(_) => Some(crate::i18n::strings::admin::custody_hosting::REMOVED.to_string()),
                Err(e) => {
                    tx.send(UiMessage::Data(DataMessage::AdminCustodyHostingLoaded {
                        snapshot: load_custody_hosting_snapshot(
                            &nest,
                            Some(format!("remove held custody: {e}")),
                        )
                        .await,
                    }));
                    return;
                }
            };
            let mut snapshot = load_custody_hosting_snapshot(&nest, None).await;
            snapshot.status = status;
            tx.send(UiMessage::Data(DataMessage::AdminCustodyHostingLoaded {
                snapshot,
            }));
        });
    }

    /// Fetch the deployment's external mail forwarders + hosted-domain list over
    /// WS-RPC via the shared `ForwarderMachine`
    /// (`fauna.bridges.{list_forwarders,list_local_domains}`), for the admin
    /// `admin-aliases` page (admin.md § 4). `Refresh` records any error into the
    /// snapshot; the resulting snapshot ships once. No HTTP.
    pub fn fetch_forwarders(&self) {
        use fauna_client_mail_settings::ForwarderAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_forwarders_machine(nest_rpc);
            let _ = machine.dispatch(ForwarderAction::Refresh).await;
            tx.send(UiMessage::Data(DataMessage::AdminForwardersLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Create an external forwarder (admin). `Refresh`-then-act: the `Create`
    /// path re-reads on success, so the shipped snapshot already carries the new
    /// forwarder; a `conflicts_with_existing_alias` / `validate_forward_target` /
    /// `reserved_local_part` rejection surfaces in `snapshot.error`.
    pub fn create_forwarder(&self, local_domain: String, pattern: String, forward_target: String) {
        use fauna_client_mail_settings::ForwarderAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_forwarders_machine(nest_rpc);
            let _ = machine.dispatch(ForwarderAction::Refresh).await;
            let _ = machine
                .dispatch(ForwarderAction::Create {
                    local_domain,
                    pattern,
                    forward_target,
                })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminForwardersLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Delete an external forwarder (admin) by its hex alias id. Same
    /// `Refresh`-then-act shape; the deleted forwarder drops from the list.
    pub fn delete_forwarder(&self, alias_id_hex: String) {
        use fauna_client_mail_settings::ForwarderAction;
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let machine = crate::mail_glue::build_forwarders_machine(nest_rpc);
            let _ = machine.dispatch(ForwarderAction::Refresh).await;
            let _ = machine
                .dispatch(ForwarderAction::Delete { alias_id_hex })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminForwardersLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Get-or-build the **persistent** admin-dns `DnsManagementMachine`
    /// ([`Self::dns_machine`] field). Built once (lazily on the first DNS method),
    /// then the same `Arc` is cloned into every subsequent DNS background task so a
    /// suspended manual cert order survives `BeginManualIssueCert` →
    /// `CompleteManualIssueCert` (the live `instant-acme` order lives in the
    /// machine's `Inner::pending_order`; a fresh machine per action would drop it
    /// and wipe `pending_cert`). The credentialed builder wires every seam from
    /// `(nest_rpc, keypair)`; the keypair derivation is practically infallible
    /// (launch already validated `secret_hex`). Returns `None` only on that
    /// impossible derive failure (logged) — the caller then skips the dispatch.
    fn dns_machine(&self) -> Option<Arc<fauna_client_dns::DnsManagementMachine>> {
        if let Some(m) = self.dns_machine.borrow().as_ref() {
            return Some(Arc::clone(m));
        }
        let keypair = match secret_to_keypair(&self.secret_hex) {
            Ok(kp) => kp,
            Err(e) => {
                tracing::error!("dns_machine: derive keypair: {e:#}");
                return None;
            }
        };
        let machine = Arc::new(
            crate::mail_glue::build_dns_management_machine_with_credentials(
                Arc::clone(&self.nest_rpc),
                keypair,
                Arc::new(crate::account_runtime::handle),
            ),
        );
        *self.dns_machine.borrow_mut() = Some(Arc::clone(&machine));
        Some(machine)
    }

    /// Fetch the unified-DNS record matrix over WS-RPC via the **persistent**
    /// `DnsManagementMachine` (`fauna.dns.list_records` then `fauna.tls.cert_status`
    /// then `fauna.dns.verify_records`), for the admin `admin-dns` page. Three
    /// dispatches: `Refresh` renders the per-domain expected-record matrix first
    /// (records visible immediately), `RefreshCertStatus` overlays each domain's
    /// served-cert health badge (§ C.4), then `VerifyRecords { None }` overlays the
    /// live public-DNS red/green verdicts onto it. `Refresh` deliberately does NOT
    /// touch `pending_cert` / `pending_order`, so a manual issuance in flight
    /// survives a page reload. Each dispatch records any error into the snapshot;
    /// the resulting snapshot ships once (the page re-renders on the merged state).
    /// No HTTP.
    pub fn fetch_dns_records(&self) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            // Records + held credentials first (fast); then the served-cert
            // health per domain; then overlay verify verdicts. A verify failure
            // leaves the matrix rendered with `checking` rows.
            let _ = machine.dispatch(DnsAction::Refresh).await;
            // Per-domain served-cert health badge (tls-certificates.md § C.4) —
            // a pure Admin read (`fauna.tls.cert_status`) projected onto
            // `cert_statuses`, one row per domain the `Refresh` just loaded.
            // Independent of verify; runs on every app (web included).
            let _ = machine.dispatch(DnsAction::RefreshCertStatus).await;
            let _ = machine
                .dispatch(DnsAction::VerifyRecords { domain: None })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Remove a held DNS-provider credential by its `DnsSnapshot.credentials`
    /// index (the `admin-dns-credential-item[index]` clear-button). Same
    /// `Refresh`-then-act-then-verify shape as the local-domain CRUD: `Refresh`
    /// loads the record matrix + current credentials, `ClearCredentials` updates
    /// `fauna.state.dns` + re-projects effective modes, `VerifyRecords` restores
    /// the red/green verdicts the refresh reset. Domains that lose coverage
    /// re-render `"manual"`. No HTTP.
    pub fn dns_clear_credentials(&self, index: u32) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            let _ = machine
                .dispatch(DnsAction::ClearCredentials { index })
                .await;
            let _ = machine
                .dispatch(DnsAction::VerifyRecords { domain: None })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Store a DNS-provider credential typed into the write-only add-credential
    /// form (`admin-dns-add-credential-*`). Same `Refresh`-then-act-then-verify
    /// shape as `dns_clear_credentials`: `PutCredentials` verifies the credential
    /// against the provider API (`DnsProviderSeam::verify`) and, on success,
    /// seals it (with the zones `verify()` reported) into `fauna.state.dns`. On a
    /// failed verify (bad token, no covering zone, network error) the dispatch
    /// errors and `DnsSnapshot.error` carries the provider rejection, surfaced in
    /// the page `error-message`; nothing is stored. The provider secret never
    /// reaches the nest (client-held store; `client-holds-provider-keys-not-nest`).
    /// The credential label defaults to the provider id (the held-credential list
    /// already renders the provider prominently). No HTTP.
    /// `label` is what tells several held credentials apart in
    /// `admin-dns-credential-item` — the add-form has no label input and
    /// passes the bare provider id, while the onboarding hand-off passes the
    /// machine-computed `"{provider} ({domain})"`. It is a parameter rather
    /// than `provider_id.clone()` because this method used to hard-code the
    /// latter, silently degrading every credential the wizard captured.
    pub fn dns_put_credentials(
        &self,
        provider_id: String,
        fields: Vec<fauna_client_dns::DnsCredentialField>,
        label: String,
    ) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            // Only re-verify the record matrix when the credential actually
            // stored. Each `dispatch` clears `snapshot.error` at its start, so a
            // trailing `VerifyRecords` after a *failed* `PutCredentials` would
            // wipe the provider-rejection error before the page renders it. On
            // failure we ship the error snapshot as-is; on success we refresh the
            // red/green verdicts (the `Refresh` above reset them, and the new
            // credential may flip a domain's effective mode).
            let put_result = machine
                .dispatch(DnsAction::PutCredentials {
                    provider_id,
                    fields,
                    label,
                })
                .await;
            if put_result.is_ok() {
                let _ = machine
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Delegate a manual-mode domain's `_acme-challenge` renewals to a zone a
    /// held credential controls (tls-certificates.md § B tier 3, S6b;
    /// `admin-dns-cert-delegate-submit-button`). `DelegateRenewal` validates a held
    /// credential covers `target_zone`, computes the re-homing target name
    /// `_acme-challenge.<domain>.<target_zone>`, persists a `CnameDelegation` in the
    /// client-held `DnsConfig.delegations`, and surfaces the one-time CNAME on
    /// `snapshot.delegations` for the admin to paste **once**. Config-only (no CA
    /// round-trip), so the same `Refresh`-then-act-then-verify shape as the
    /// credential CRUD. On a failed delegate (no covering credential) the error
    /// surfaces in `error-message` and we skip the trailing verify so it isn't
    /// cleared. No HTTP.
    pub fn dns_delegate_renewal(&self, domain: String, target_zone: String) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            let delegated = machine
                .dispatch(DnsAction::DelegateRenewal {
                    domain,
                    target_zone,
                })
                .await;
            if delegated.is_ok() {
                let _ = machine
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Remove a domain's `_acme-challenge` CNAME delegation
    /// (`admin-dns-cert-remove-delegation-button` → `DnsAction::RemoveDelegation`,
    /// the inverse of `dns_delegate_renewal`): drop the `CnameDelegation` from
    /// `DnsConfig.delegations`; the domain reverts to per-renewal manual paste.
    /// Config-only; same shape as the credential CRUD. No HTTP.
    pub fn dns_remove_delegation(&self, domain: String) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            let removed = machine
                .dispatch(DnsAction::RemoveDelegation { domain })
                .await;
            if removed.is_ok() {
                let _ = machine
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Get/renew a trusted TLS certificate for a **managed or CNAME-delegated**
    /// domain (`admin-dns-cert-issue-button`, tls-certificates.md § B tier 2 / § C.3
    /// S5): a single `DnsAction::IssueCert { domain, target_nest_id }` runs the full
    /// client-driven DNS-01 order — resolve the covering credential + zone (or the
    /// delegated re-home target), publish the `_acme-challenge` TXT, finalize, then
    /// seal + deliver the cert to the serving private nest and persist the ACME
    /// account (D6). `target_nest_id` is the id the connection is bound to
    /// ([`resolve_this_nest_id`]).
    /// After issuance we re-read the served-cert badge (`RefreshCertStatus`) so the
    /// page reflects the new cert; on any failure (e.g. no CA reachable) the machine
    /// records it into `snapshot.error` (surfaced in `error-message`) and the nest
    /// stays gracefully on the floor. The **render layer** routes a manual
    /// (no-covering-credential, undelegated) domain to [`Self::dns_begin_manual_issue`]
    /// instead. Native-only (the order core is not WASM-safe). No HTTP.
    pub fn dns_issue_cert(&self, domain: String) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match resolve_this_nest_id(&nest_rpc).await {
                Ok(target_nest_id) => {
                    let _ = machine
                        .dispatch(DnsAction::IssueCert {
                            domain,
                            target_nest_id,
                        })
                        .await;
                    // Reflect the (possibly) freshly-installed cert in the badge.
                    let _ = machine.dispatch(DnsAction::RefreshCertStatus).await;
                }
                Err(e) => tracing::error!("dns_issue_cert: resolve target nest id: {e}"),
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Phase 1 of **manual-paste** issuance for a domain with no covering DNS
    /// credential (`admin-dns-cert-issue-button` on a manual, undelegated domain;
    /// tls-certificates.md § B tier 3 S6a): `DnsAction::BeginManualIssueCert` opens
    /// the DNS-01 order, stashes the live order in the **persistent** machine's
    /// `Inner::pending_order`, and surfaces the transient `_acme-challenge` TXT(s) on
    /// `snapshot.pending_cert` for the admin to paste at their registrar. The page
    /// then renders those rows (`admin-dns-record`) + the complete/cancel
    /// affordances. `target_nest_id` is the id the connection is bound to
    /// ([`resolve_this_nest_id`]).
    /// The order is held across the admin's out-of-band paste — which is exactly why
    /// the machine must persist ([`Self::dns_machine`]). Native-only. No HTTP.
    pub fn dns_begin_manual_issue(&self, domain: String) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match resolve_this_nest_id(&nest_rpc).await {
                Ok(target_nest_id) => {
                    let _ = machine
                        .dispatch(DnsAction::BeginManualIssueCert {
                            domain,
                            target_nest_id,
                        })
                        .await;
                }
                Err(e) => tracing::error!("dns_begin_manual_issue: resolve target nest id: {e}"),
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Phase 2 of manual-paste issuance (`admin-dns-cert-complete-button`,
    /// tls-certificates.md § B tier 3 S6a): the admin has pasted the surfaced
    /// `_acme-challenge` TXT and verified it live, so `DnsAction::CompleteManualIssueCert`
    /// resumes the suspended order on the persistent machine, finalizes it with the
    /// CA, seals + delivers the cert to the nest, and persists the ACME account; on
    /// success the badge is re-read (`RefreshCertStatus`). On failure the consumed
    /// order is gone and the paste surface clears (the admin re-begins). No HTTP.
    pub fn dns_complete_manual_issue(&self) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let completed = machine.dispatch(DnsAction::CompleteManualIssueCert).await;
            if completed.is_ok() {
                let _ = machine.dispatch(DnsAction::RefreshCertStatus).await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Abandon a pending manual-paste issuance (`admin-dns-cert-cancel-button`,
    /// tls-certificates.md § B tier 3 S6a): `DnsAction::CancelManualIssueCert` drops
    /// the suspended order on the persistent machine and clears `pending_cert` — the
    /// nest stays on the self-signed floor (graceful). No CA work; no HTTP.
    pub fn dns_cancel_manual_issue(&self) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::CancelManualIssueCert).await;
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Turn automatic certificate renewal on/off for `domain` (the per-domain
    /// `admin-dns-domain-auto-renew` checkbox; tls-certificates.md § C.3 C3):
    /// `DnsAction::SetAutoRenew { domain, enabled }` persists the opt-OUT in the
    /// client-held `DnsConfig.auto_renew_off` and re-projects effective state.
    /// Config-only (no CA round-trip), so no `Refresh` — `SetAutoRenew` re-applies
    /// config and re-projects without touching the matrix/verdicts the page already
    /// shows, keeping the toggle snappy and the red/green checks intact. Default is
    /// on, so a fresh managed/delegated domain is checked with no admin action. The
    /// background cadence that acts on it (`run_auto_renew_cadence_tick`, spawned by
    /// `start_ws_rpc`) is the native auto-issue half. No HTTP.
    pub fn dns_set_auto_renew(&self, domain: String, enabled: bool) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine
                .dispatch(DnsAction::SetAutoRenew { domain, enabled })
                .await;
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// The post-auth edge of the deployment-seed custody leg (`box-recovery.md`
    /// § The plane-era recovery floor → *(c) The writes*): runs the leg now when
    /// the account store is already up. When it is not, this does nothing —
    /// `account_runtime::install`'s store-ready edge, landing second, runs it.
    /// The steady state (this box already custodied) makes no round trip.
    pub fn run_deployment_seed_custody_leg(&self) {
        let Some(store) = crate::account_runtime::handle() else {
            return;
        };
        self.spawn_bg(run_deployment_seed_custody_leg(
            Arc::clone(&self.nest_rpc),
            store,
            self.tx.clone(),
        ));
    }

    /// Run the post-succession **aftermath** for this session's identity — the
    /// shared ordered pass (legs 1, 2, 4, 7, 6) that repairs what a succession
    /// moves ownership of but not the seal on (`succession-aftermath.md`
    /// § Re-key scope). Fired first at the universal post-auth hook (`app.rs`'s
    /// `AuthSuccess` arm), at **every** sign-in on every device: each leg is
    /// idempotent and the corpus is its own progress record, so a pass that
    /// finds nothing owed reports nothing.
    ///
    /// The legs' order is `fauna_client_recovery::aftermath`'s; what linux alone
    /// answers — the gate, the predecessor material, the parked ceremony, the
    /// sink — is [`crate::succession_aftermath`]'s.
    pub fn run_succession_aftermath(&self) {
        let Some(actor_id) = self.actor_id() else {
            return;
        };
        let registry = crate::account_registry();
        // Learn a succession link this device's registry does not hold (it
        // never held the predecessor's row, or the user removed the retired
        // account) — the shared hop, best-effort, one profile read for an
        // ordinary identity. Spawned beside the pass rather than ahead of it:
        // a device with no predecessor material has no pass to run today, and
        // the link this records is what the profile writers read at the next
        // save and what the next sign-in's gate sees (tui's shape).
        {
            let nest_rpc = Arc::clone(&self.nest_rpc);
            let keypair = fauna_core::identity::ActorKeypair::from_secret(self.secret_bytes());
            self.spawn_bg(async move {
                fauna_client_recovery::ceremony::learn_succession_link(
                    nest_rpc,
                    &crate::account_registry(),
                    &keypair,
                )
                .await;
            });
        }
        let Some(inputs) = crate::succession_aftermath::inputs(
            &registry.predecessors_of(&actor_id),
            || registry.predecessor_backup_keys_by_actor(&actor_id),
            self.secret_bytes(),
        ) else {
            return;
        };
        // No review re-read here: the review surfaces live on the succession
        // ledger, which the post-store-ready pass re-reads.
        let sink = crate::succession_aftermath::AftermathUi::new(self.tx.clone(), None);
        self.spawn_bg(crate::succession_aftermath::run(
            Arc::clone(&self.nest_rpc),
            inputs,
            sink,
        ));
    }

    /// Report the nest's **public** IP so it gates ACME HTTP-01 on the *strong*
    /// resolve-check and assembles the apex/`mail.` records
    /// (`domains-and-tls-bootstrap.md` § Host-address acquisition). The linux twin
    /// of the native `report_host_address` FFI and the web `reportHostAddress`
    /// binding. Fired once per session at admin confirmation (`AdminStatusLoaded`
    /// → `is_admin`), so it is **admin-gated by the caller** — `set_host_address`
    /// is Admin-only, and gating avoids a pointless failing RPC for a non-admin.
    /// Fire-and-forget / log-only (the shared decision fn logs the outcome itself);
    /// idempotent last-writer-wins nest-side; never publishes a private/LAN address
    /// (the safety invariant lives in the shared fn).
    pub fn report_host_address(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        self.spawn_bg(async move {
            // The dial-address the client used to reach the nest, read before the
            // Arc is moved into the typed DNS caller.
            let dial_url = nest_rpc.nest_url();
            let dns = fauna_client_dns::DnsAdminClient::new(nest_rpc);
            let probe = fauna_client_dns::host_address::NativeHostAddressProbe;
            // The shared decision fn classifies the dial-address (never publishing a
            // private/LAN one), reports the public IP when determinable, and logs
            // the outcome (info / debug / warn) itself.
            let _ =
                fauna_client_dns::host_address::report_host_address(&dns, &dial_url, &probe).await;
        });
    }

    /// Populate the conversation muted-keyword cache from the sealed
    /// `fauna.state.moderation` muted-keyword list at the universal post-auth hook
    /// (fired next to [`Self::refresh_mail_epoch_schedule`] in `app.rs`), so the conversation
    /// bubble collapse (`views/conversations/message_bubble.rs`) works on a
    /// fresh launch without first opening the "Muted words" Settings page — the
    /// GTK render is synchronous and can't await a load, so the list is
    /// preloaded here (the web app instead loads it on Conversations mount +
    /// re-renders reactively; same concept, platform-divergent mechanism,
    /// priority #2). The list is also refreshed on every load/save of that
    /// Settings page (`settings/muted_words.rs`); both writers run on the GTK
    /// thread, matching the `crate::conversations` thread-local cache.
    ///
    /// GTK-thread only: spawns the account-store read on the runtime and
    /// marshals the list back to the GTK thread via [`spawn_with_snapshot`].
    /// Best-effort (leaves the list empty on a hard failure — nothing
    /// collapses, and the next page visit repopulates it); no re-render
    /// trigger is needed since auth precedes any conversation view this
    /// session. Kept: the read waits for the account runtime's assembly, not a
    /// single NestClient RPC (transport.md § Request lifecycle step 3's note).
    pub fn load_muted_keywords(&self) {
        crate::async_helper::spawn_with_snapshot(
            &self.runtime_handle,
            move || async move {
                let store = crate::account_runtime::handle_source();
                crate::async_helper::hydrate_with_retry(|| async {
                    fauna_sync_engine::preference_surfaces::load_muted_words(&store)
                        .await
                        .map(|snapshot| snapshot.keywords)
                        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
                })
                .await
                .unwrap_or_default()
            },
            crate::conversations::set_muted_keywords_cache,
        );
    }

    /// Opportunistically refresh the published mail content-sealing epoch
    /// schedule on every successful (re)connect
    /// (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
    /// *Content-sealing epochs*), mirroring [`Self::run_critical_alert_sweep`]'s
    /// posture exactly: fired at the same universal post-auth hook,
    /// best-effort / log-only. Delegates to the shared, idempotent
    /// [`fauna_client_mail_settings::MailSettingsMachine::refresh_epoch_schedule`],
    /// which no-ops when mail isn't enabled (no MSEK) — so this is always
    /// safe to call unconditionally, admin or not. Without it, a schedule published
    /// at enable-mail time slides stale past the 26-week
    /// `MAIL_EPOCH_PUBLISH_HORIZON` for an actor who never re-runs
    /// enable/rotate; the design's § 3 step 2 degradation (seal under the
    /// newest published epoch — still correct, just coarser) covers that gap
    /// safely in the meantime, so a failure here is never surfaced to the
    /// user.
    pub fn refresh_mail_epoch_schedule(&self) {
        let machine = match crate::mail_glue::build_mail_settings_machine(self) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("refresh_mail_epoch_schedule: build machine: {e}");
                return;
            }
        };
        self.spawn_bg(async move {
            match machine.refresh_epoch_schedule().await {
                Ok(()) => tracing::debug!(
                    "mail epoch schedule refresh: no-op or republished (mail disabled \
                     no-ops silently)"
                ),
                Err(e) => tracing::warn!(
                    "mail epoch schedule refresh failed (best-effort; re-converges next \
                     connect): {e}"
                ),
            }
        });
    }

    /// Run the feeders that have no page of their own, at the same universal
    /// post-auth hook as [`Self::refresh_mail_epoch_schedule`]
    /// (`critical-alerts.md` § Goal — a set-and-forget deployment must still
    /// raise its banner). tui's `critical_alerts::spawn_session_start_sweep`
    /// (`session::establish`) is the reference this mirrors exactly
    /// (`critical-alerts.md` § Implementation status today: "each is one call
    /// at the app's own post-auth hook, not a per-app design") — this app's
    /// `crate::critical_alerts::registry()` substituted in for tui's
    /// per-`App` one. Best-effort/log-only: sign-in must not fail, or even
    /// wait, on a nest that cannot answer the recovery/directory planes.
    ///
    /// Runs [`fauna_client_alert_sweep::run_alert_sweep_loop`] — sweeps
    /// immediately, then every `RE_SWEEP_INTERVAL_SECS` for as long as the
    /// identity lives (§ Mechanism → *Who runs the detector*), logging each
    /// sweep itself. It stops on the first wake after `CriticalAlerts::clear_all`
    /// runs, so no cancellation is needed here.
    ///
    /// Once per session ESTABLISHMENT (the AuthSuccess hook) — never from the
    /// same-actor converge arm, which runs [`Self::run_critical_alert_sweep_once`]
    /// instead: re-firing the loop there stacked one concurrent sweeper per
    /// re-establish, forever (tui's `spawn_one_shot_sweep` and windows' leg
    /// ratified the same split, `critical-alerts.md` § Implementation status
    /// today).
    pub fn run_critical_alert_sweep(&self) {
        let Some((nest_rpc, custody, alerts, actor_id)) = self.alert_sweep_parts() else {
            return;
        };
        // The e2e build runs the SAME loop with its wait raceable by the agent's
        // `alert_sweep_wake` (convention 14), minting a fresh wake per identity;
        // a production build has no wake to race and runs the clock alone.
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        {
            let wake = crate::critical_alerts::mint_sweep_wake();
            self.spawn_bg(async move {
                fauna_client_alert_sweep::run_alert_sweep_loop_wakeable(
                    nest_rpc,
                    &custody,
                    &alerts,
                    &actor_id,
                    move || {
                        let notify = Arc::clone(&wake);
                        async move { notify.notified().await }
                    },
                )
                .await;
            });
        }
        #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
        self.spawn_bg(async move {
            fauna_client_alert_sweep::run_alert_sweep_loop(nest_rpc, &custody, &alerts, &actor_id)
                .await;
        });
    }

    /// Run ONE sweep pass — the same-actor converge arm's re-run of the
    /// post-auth feeders (`main.rs`'s session patch). The loop
    /// [`Self::run_critical_alert_sweep`] started at AuthSuccess is still
    /// running for this identity, so a second loop here would only stack.
    pub fn run_critical_alert_sweep_once(&self) {
        let Some((nest_rpc, custody, alerts, actor_id)) = self.alert_sweep_parts() else {
            return;
        };
        self.spawn_bg(async move {
            let report = fauna_client_alert_sweep::run_session_start_sweep(
                nest_rpc, &custody, &alerts, &actor_id,
            )
            .await;
            tracing::debug!(
                failures = report.failures.len(),
                skipped = report.skipped.len(),
                "converge-arm critical-alert sweep pass done"
            );
        });
    }

    /// What every sweep needs, from the session secret. An undecodable secret is
    /// not a reason to fail sign-in, so it skips the sweep and logs.
    #[allow(clippy::type_complexity)]
    fn alert_sweep_parts(
        &self,
    ) -> Option<(
        Arc<NestClient>,
        Arc<dyn fauna_client_account_runtime::atproto_identity::AtprotoIdentityStore>,
        Arc<fauna_client_alerts::CriticalAlerts>,
        fauna_core::identity::ActorId,
    )> {
        let keypair = match secret_to_keypair(&self.secret_hex) {
            Ok(kp) => kp,
            Err(e) => {
                tracing::warn!("critical-alert sweep skipped: bad secret: {e}");
                return None;
            }
        };
        let actor_id = keypair.actor_id();
        let nest_rpc = Arc::clone(&self.nest_rpc);
        // Feeder #1's rotation keyring: the account plane's custody over this
        // process's runtime handle, read fresh per call.
        let custody: Arc<dyn fauna_client_account_runtime::atproto_identity::AtprotoIdentityStore> =
            Arc::new(
                fauna_client_account_runtime::atproto_identity::RuntimeAtprotoIdentity::new(
                    crate::account_runtime::handle,
                ),
            );
        Some((
            nest_rpc,
            custody,
            crate::critical_alerts::registry(),
            actor_id,
        ))
    }

    /// Mint the default capability-grant set for this box — the one-tap
    /// `trust_prompt` answer, consumed at the signed-in handoff
    /// (`onboarding.md` § 3b-ter).
    ///
    /// Best-effort and log-only **by design**: the user has completed
    /// onboarding, and a mint failure must not paint an error over that. The
    /// same trust is grantable any time from Settings → Nests, which is also
    /// where the minted grants and their log entries surface.
    ///
    /// **Which grants** is not decided here: `MintDefaultSet` mints exactly
    /// what the shared mint catalog derives, so this glue holds no policy that
    /// could drift from the Nests page's own picker — and it is the *same*
    /// machine that page drives. An empty set (mail not yet enabled, no content
    /// processor enrolled) is an honest no-op, not an error.
    pub fn mint_default_trust_set(&self) {
        let machine =
            match crate::mail_glue::build_linked_nests_machine_with_mail_relay_and_trust(self) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("one-tap trust: {e}; nothing minted");
                    return;
                }
            };
        self.spawn_bg(fauna_client_pair::dispatch_mint_default_trust_set(machine));
    }

    /// Register the kit the onboarding `recovery_kit` screen minted and the
    /// user confirmed — the deferred half of that screen's ceremony, run here
    /// because no nest existed at its position and the root is never persisted
    /// (`identity-succession.md` § The RecoveryKey → *Creation UX*). The shared
    /// `fauna_client_recovery::ceremony::register_deferred_kit` owns the
    /// registration, the profile-head mirror and every logged arm (tui's
    /// handoff calls the same). Fire-and-forget: a failure leaves Settings'
    /// `recovery-kit-status` telling the truth.
    pub fn register_deferred_recovery_kit(&self, kit_hex: fauna_core::secret::SecretString) {
        let identity = match ActorKeypair::from_secret_hex(&self.secret_hex) {
            Ok(kp) => kp,
            Err(e) => {
                tracing::error!("deferred recovery kit: identity secret unreadable: {e}");
                return;
            }
        };
        let nest = Arc::clone(&self.nest_rpc);
        self.spawn_bg(async move {
            fauna_client_recovery::ceremony::register_deferred_kit(nest, &identity, &kit_hex).await;
        });
    }

    /// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*):
    /// hand the four intents read off the onboarding machine to the shared
    /// [`fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement`],
    /// which owns the whole firing — the first-setup mail provision (the
    /// admin's mailbox + `set_mail_enabled(true)`, or the policy-gated new-user
    /// auto-mint), the three DAV deployment toggles, and the one-MSEK-mint-path
    /// companion mints — and publishes the
    /// `fauna_e2e_agent::SERVING_ENABLEMENT_KEY` completion anchor. tui's
    /// `LoggedIn` handoff calls the same entry. Fire-and-forget over the
    /// now-authenticated WS connection.
    pub fn apply_post_claim_serving_enablement(
        &self,
        intents: fauna_client_mail_settings::serving_enablement::ServingEnablementIntents,
    ) {
        let keypair = match ActorKeypair::from_secret_hex(self.secret_hex()) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!("apply_post_claim_serving_enablement: decode secret_hex: {e}");
                return;
            }
        };
        self.spawn_bg(
            fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement(
                Arc::clone(&self.nest_rpc),
                keypair,
                crate::account_runtime::mail_store(),
                self.node_url().to_string(),
                crate::account_runtime::ledger_seam(),
                intents,
            ),
        );
    }

    /// Factory-reset this nest (admin "Danger zone" affordance, `admin-settings`).
    /// Calls the shared `AdminClient::factory_reset` (`fauna.admin.factory_reset`)
    /// over the authenticated Admin WS connection; the reply carries the
    /// post-reset claim code (the human never sees it — the nest exits + restarts
    /// into the wipe right after replying). On success we ship a
    /// `FactoryResetComplete` carrying the re-onboard seed the client already
    /// holds (nest_url + secret + handle + the returned code), so the GTK-side
    /// handler can tear the session down and re-seed onboarding at the claim-code
    /// step with the code pre-filled. On error the box is untouched
    /// (`FactoryResetFailed`). Per `docs/goal/behavior/mail-bridge-lifecycle.md`
    /// § Factory reset.
    pub fn factory_reset(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let nest_url = self.node_url.clone();
        let secret_hex = self.secret_hex.clone();
        // The re-claim after the wipe re-registers the primary mail domain from
        // the handle's `@domain` (claim auto-registers mail_domain — see
        // onboarding-auto-adds-handle-domain). A handle is now REQUIRED to claim
        // (a handle-less admin is rejected ),
        // so the re-onboard MUST carry a non-empty handle or the re-claim fails.
        // The account cache can be empty (a locked/absent keyring, or never
        // populated), so source the handle AUTHORITATIVELY from the live admin
        // session (`fauna.account.get`) while it still exists — only falling back
        // to the cache. The nest stores/returns BARE localparts, so re-qualify
        // with the domain (cached, else the nest URL host); without this the
        // re-onboarded nest comes back with NO primary mail domain — the bridge
        // idles and never binds IMAPS, so mail silently breaks after a reset.
        let (cached_handle, cached_domain, _) = load_account_cache();
        self.spawn_bg(async move {
            // Authoritative handle from the still-live session, before the wipe.
            let authoritative = fauna_client_account::AccountClient::new(Arc::clone(&nest_rpc))
                .get()
                .await
                .ok()
                .and_then(|r| r.handle)
                .filter(|h| !h.is_empty());
            let local = authoritative.or(cached_handle).unwrap_or_default();
            // The qualification rule + nest-URL-host parse live once in shared Rust
            // (windows/android already adopt it via FFI) — never re-derive inline.
            let handle = fauna_onboarding_machine::qualify_reclaim_handle(
                local,
                cached_domain,
                nest_url.clone(),
            );
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);

            // CR-1 (`common.md` § Client-state recoverability): mint the
            // post-reset claim code and persist it to the long-term store
            // BEFORE dispatching, then pin it onto the request. Until this
            // landed, the code existed only in the synchronous reply — a SIGKILL
            // between dispatch and reply-render left the box at the fresh floor
            // but un-claimable by anyone. The mint-and-persist helper returns the
            // code only once the row is durable, so the crash-unsafe ordering is
            // unrepresentable here.
            let persistence = crate::account_registry().launch_persistence();
            let Some(pinned_code) = fauna_launch_machine::mint_and_persist_pending_factory_reset(
                &persistence,
                nest_url.clone(),
                handle.clone(),
            ) else {
                // The store silently dropped the row (a locked libsecret
                // collection is the realistic case). Do NOT dispatch: a reset
                // whose code we failed to persist is exactly the un-claimable box
                // CR-1 describes. Refusing to start is always recoverable; a wipe
                // against a code nobody holds is not.
                tracing::error!("factory_reset: refusing to dispatch — claim code not persisted");
                tx.send(UiMessage::Data(DataMessage::FactoryResetFailed {
                    error: crate::i18n::strings::admin::settings_page::FACTORY_RESET_PERSIST_FAILED
                        .to_string(),
                }));
                return;
            };

            match admin.factory_reset(Some(pinned_code)).await {
                Ok(reply) => {
                    // The nest honors a pinned code verbatim, so this is the code
                    // already in the slot; take it from the reply anyway so the
                    // nest stays the authority on what the box booted with.
                    tx.send(UiMessage::Data(DataMessage::FactoryResetComplete {
                        claim_code: reply.claim_code,
                        nest_url,
                        // The onboarding re-seed handler this message feeds
                        // ultimately re-exposes the secret as a plaintext
                        // prefill string anyway (`settings::trigger_factory_reset`),
                        // so this hop stays a plain `String` rather than
                        // carrying `SecretString` one message further for no
                        // containment gain.
                        secret_hex: secret_hex.to_string(),
                        handle,
                    }));
                }
                Err(e) => {
                    tracing::error!("factory_reset: {e:?}");
                    tx.send(UiMessage::Data(DataMessage::FactoryResetFailed {
                        error: format!("{e:?}"),
                    }));
                }
            }
        });
    }

    /// Reads the deployment-seed rotation confirm's roster via the shared
    /// `AdminClient::seed_rotate_roster_view` (`box-recovery.md` § Deployment-
    /// seed rotation → Ordering rule). Fired on the arm click
    /// (`admin-nest-seed-rotate-button`); mirrors tui's `load_seed_rotate_roster`
    /// (`apps/fauna-tui/src/admin/mod.rs`) — same shared fold, same "never drop
    /// a row it cannot name" degrade.
    pub fn load_seed_rotate_roster(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let client = fauna_client_admin::AdminClient::new(nest_rpc);
            let state = match client.seed_rotate_roster_view().await {
                Ok(view) => crate::views::admin::SeedRotateConfirmState::Ready(Box::new(view)),
                Err(e) => crate::views::admin::SeedRotateConfirmState::Failed(
                    crate::i18n::strings::admin::nest_page::rotate_seed_roster_error(
                        &e.to_string(),
                    ),
                ),
            };
            tx.send(UiMessage::Data(DataMessage::SeedRotateRosterLoaded {
                state,
            }));
        });
    }

    /// Drives the deployment-seed rotation ceremony and reports it in one
    /// sentence (`box-recovery.md` § Deployment-seed rotation → § The
    /// ceremony), fired on the confirm click (`admin-nest-seed-rotate-confirm-button`)
    /// once the caller has disarmed the roster (disarm-before-dispatch — a
    /// double click must not chain a second rotation onto the first).
    ///
    /// The whole ceremony is the shared plane drive
    /// (`fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed`:
    /// the successor's custody row merged and published before dispatch, over
    /// the account-store handle — refused plainly without one). There is no
    /// fan-out: the plane carries the successor's row (`box-recovery.md` § The
    /// plane-era recovery floor → *(c) The writes*). The verdict is the shared
    /// decision, not this app's.
    pub fn rotate_deployment_seed(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let store = crate::account_runtime::handle();
        self.spawn_bg(async move {
            let status = fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed(
                &nest_rpc,
                store.as_ref(),
            )
            .await
            .resolve(crate::i18n::strings::lookup);
            tx.send(UiMessage::Data(DataMessage::SeedRotated { status }));
        });
    }

    /// Reads the outside-app sign-in key set (`fauna.oauth.issuer_key_status`)
    /// for the `admin-nest-oauth-*` section, read and folded by the shared
    /// `AdminClient::issuer_key_status_view` door (`authorization-server.md`
    /// § The issuer). Fired at the admin gate and on every admin-nest show;
    /// a failure is the section's own reason line, never a page error — see
    /// [`read_oauth_issuer_keys`].
    pub fn fetch_oauth_issuer_keys(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let keys = read_oauth_issuer_keys(nest_rpc).await;
            tx.send(UiMessage::Data(DataMessage::OauthKeysLoaded { keys }));
        });
    }

    /// The ordinary issuer-key rotation (`fauna.oauth.rotate_issuer_key`),
    /// fired by `admin-nest-oauth-rotate-button` — dispatched AND worded by the
    /// shared `AdminClient::rotate_issuer_key_verdict` door (a failure is a
    /// verdict too: the kind mints, so a lost reply can follow a committed
    /// rotation, and only the shared fold may say so), then the key set re-read
    /// and both delivered in one message. Mirrors tui's `Op::RotateIssuerKey`
    /// (`apps/fauna-tui/src/admin/mod.rs`).
    pub fn rotate_oauth_issuer_key(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let status = fauna_client_admin::AdminClient::new(Arc::clone(&nest_rpc))
                .rotate_issuer_key_verdict()
                .await
                .resolve(crate::i18n::strings::lookup);
            let keys = read_oauth_issuer_keys(nest_rpc).await;
            tx.send(UiMessage::Data(DataMessage::OauthDone { status, keys }));
        });
    }

    /// One forced arm — exactly `arm`'s kind (`fauna.oauth.force_rotate_issuer_key`
    /// / `fauna.oauth.force_rotate_session_secret`), fired by
    /// `admin-nest-oauth-confirm-button` once the caller has disarmed
    /// (disarm-before-dispatch). Dispatched and worded by the shared
    /// `AdminClient::force_rotate_verdict` door on this OS's clock face, then
    /// the key set re-read, as [`Self::rotate_oauth_issuer_key`]. Mirrors tui's
    /// `Op::OauthForced`.
    pub fn force_rotate_oauth_issuer(&self, arm: fauna_client_admin::IssuerForcedArm) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let status = fauna_client_admin::AdminClient::new(Arc::clone(&nest_rpc))
                .force_rotate_verdict(arm, fauna_core::format::format_unix_local)
                .await
                .resolve(crate::i18n::strings::lookup);
            let keys = read_oauth_issuer_keys(nest_rpc).await;
            tx.send(UiMessage::Data(DataMessage::OauthDone { status, keys }));
        });
    }

    /// Dispatch the armed legal-takedown/restore form
    /// (`fauna.moderation.legal_takedown`; `moderation.md` § Legal takedown →
    /// Invocation surface), fired on the confirm click
    /// (`admin-nest-takedown-confirm-button`) once the caller has disarmed
    /// (disarm-before-dispatch — a double click must not dispatch a second
    /// compulsory act). The id/reference are trimmed here, exactly as the
    /// shared form fold judged them (`takedown_form_view` gates on the
    /// trimmed values), so what was confirmed is what is sent. Mirrors tui's
    /// `submit_takedown` (`apps/fauna-tui/src/admin/mod.rs`) — the verdict
    /// wording is the shared decision, not this app's.
    pub fn submit_takedown(&self, form: fauna_client_moderation::TakedownForm) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let error = fauna_client_moderation::ModerationClient::new(nest_rpc)
                .legal_takedown(
                    form.content_id.trim(),
                    form.content_type.wire(),
                    form.legal_reference.trim(),
                    form.restore,
                )
                .await
                .err()
                .map(|e| e.to_string());
            let status = fauna_client_moderation::takedown_verdict(form.restore, error)
                .resolve(crate::i18n::strings::lookup);
            tx.send(UiMessage::Data(DataMessage::TakedownSubmitted { status }));
        });
    }

    /// Opt a domain in/out of Fauna-managed DNS via the per-domain
    /// `admin-dns-domain-mode` toggle. Drives the shared `DnsManagementMachine`'s
    /// `SetMode`: `Refresh` (loads the matrix + held credential store + projects
    /// effective modes), then `SetMode { domain, managed }`. Opting **in**
    /// requires a held credential whose zones cover `domain`; otherwise `SetMode`
    /// rejects with `InvalidState` and `DnsSnapshot.error` carries the rejection
    /// (surfaced in the page `error-message`), the domain staying manual.
    ///
    /// A successful opt-**in** publishes the domain's record matrix through the
    /// held credential inside the same `SetMode` (the shared machine owns that
    /// sequencing — `dns-management.md` § Fauna-managed: managed mode publishes
    /// automatically, there is no manual Publish button). A publish failure
    /// surfaces in `error-message` and does **not** fall back to manual (the
    /// opt-in is already committed). After a clean `SetMode` we re-verify to
    /// refresh the red/green verdicts the `Refresh` reset. Each `dispatch` clears `snapshot.error` at its start,
    /// so we run a trailing dispatch only when the prior step succeeded — else a
    /// `VerifyRecords` would wipe the rejection before the page renders it (the
    /// same guard the credential write-path uses). No HTTP.
    ///
    /// (Background reconcile — re-publishing every managed domain whenever the
    /// page is open or drift is detected, rate-limited per `mail-multidomain.md`
    /// — is deferred: it needs the per-entry per-minute rate limiter that does
    /// not yet exist client-side.)
    pub fn dns_set_mode(&self, domain: String, managed: bool) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            let set_result = machine
                .dispatch(DnsAction::SetMode {
                    domain: domain.clone(),
                    managed,
                })
                .await;
            // A SetMode error (incl. a failed publish after the opt-in) is
            // shipped as-is — no trailing verify to wipe it.
            if set_result.is_ok() {
                let _ = machine
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Deployment "Fauna controls all domains" master switch
    /// (`admin-dns-manage-all-toggle`): set every active domain's mode at once
    /// (`dns-management.md` § The two modes — a deployment-level convenience over
    /// the per-domain state). `Refresh` loads the domain list + credential store,
    /// then `SetMode { managed }` runs for each domain (which publishes each on
    /// opt-in, like `dns_set_mode`). The first failure — typically a domain with
    /// no covering credential (`InvalidState`) — stops the loop and ships that
    /// error snapshot (so a partial deployment surfaces *which* domain still
    /// needs a credential); domains already switched keep their new mode. Only on
    /// a clean sweep do we re-verify. No HTTP.
    pub fn dns_set_all_managed(&self, managed: bool) {
        use fauna_client_dns::DnsAction;
        let Some(machine) = self.dns_machine() else {
            return;
        };
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let _ = machine.dispatch(DnsAction::Refresh).await;
            let domains: Vec<String> = machine
                .snapshot()
                .domains
                .iter()
                .map(|d| d.domain.clone())
                .collect();
            let mut errored = false;
            for domain in domains {
                if machine
                    .dispatch(DnsAction::SetMode { domain, managed })
                    .await
                    .is_err()
                {
                    errored = true;
                    break;
                }
            }
            if !errored {
                let _ = machine
                    .dispatch(DnsAction::VerifyRecords { domain: None })
                    .await;
            }
            tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
                snapshot: machine.snapshot(),
            }));
        });
    }

    /// Fetch nest-wide statistics (admin only) via `fauna.admin.stats` (WS-RPC;
    /// the typed twin of the deleted `GET /admin/api/stats`). The dashboard
    /// reads its counters from the JSON `stats` value, so `AdminStatsReply` is
    /// re-serialized with `serde_json::to_value` — shape-identical to the old
    /// HTTP body (`total_users`/`users_by_tier`/`suspended_users`/
    /// `total_inbox_bytes`/`total_storage_bytes`/`ws_connections`). Typing the
    /// `AdminStatsLoaded` payload is a later dashboard cleanup, not this slice.
    pub fn fetch_admin_stats(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.stats().await {
                Ok(reply) => {
                    let stats = serde_json::to_value(&reply).unwrap_or(serde_json::Value::Null);
                    UiMessage::Data(DataMessage::AdminStatsLoaded { stats })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.stats".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Refresh the admin shell's dynamic feeds: the admin-users hub's three
    /// sections (users + invite codes + pending invite requests — admin.md
    /// § Users), local mail domains, the pending-bridge approval feed, the
    /// DNS matrix, admin services, the `admin-aliases` forwarder list and its
    /// hosted-domain picker (the `ForwarderMachine` snapshot, which reads the
    /// domain list separately from the DNS page's — tui refreshes the same
    /// machine on every entry to that page), and the settings page's membership
    /// designation picker (the admin's own subscription tier names —
    /// monetization.md § Pillar 4). One shared call from both the real
    /// sidebar nav edge (`app.rs`'s content-stack notify) and the
    /// test-agent nav patch (`main.rs`), so an admin returning to the shell
    /// by either door sees resources another actor (or a WS-RPC seed)
    /// created after the initial admin-status fetch — which otherwise fires
    /// only once per session.
    pub fn refresh_admin_shell(&self) {
        self.fetch_admin_users();
        self.fetch_admin_invite_codes();
        self.fetch_admin_invite_requests();
        self.fetch_local_domains();
        self.fetch_pending_bridges();
        self.fetch_dns_records();
        self.fetch_admin_services();
        self.fetch_forwarders();
        self.fetch_own_membership_tier_names();
    }

    /// Every account on the nest for the admin actor pickers
    /// (`fauna_client_admin::users_list_all`; `admin.md` § 2 → *Which accounts a
    /// picker offers*), read beside each users page so the page and the pickers
    /// arrive in one `AdminUsersLoaded`. `None` on failure — the pickers then keep
    /// the list they had rather than an emptied one.
    async fn picker_users_read<R: fauna_protocol::RpcRequester>(
        admin: &fauna_client_admin::AdminClient<R>,
    ) -> Option<Vec<fauna_client_admin::AdminUser>> {
        match fauna_client_admin::users_list_all(admin).await {
            Ok(users) => Some(users),
            Err(e) => {
                tracing::warn!("fauna.admin.users.list (every account, for the pickers): {e}");
                None
            }
        }
    }

    /// Fetch registered users (admin only) over `fauna.admin.users.list`.
    /// `limit` `None` ⇒ the nest default page (50); backs the consolidated
    /// `admin-users` Users section + the dashboard recent-users group, and
    /// carries the pickers' every-account read ([`Self::picker_users_read`]).
    pub fn fetch_admin_users(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.users_list(None, 0).await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminUsersLoaded {
                    reply,
                    picker_users: Self::picker_users_read(&admin).await,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch one page of users at an explicit `offset` (admin.md § Users —
    /// pagination). The page size is `USERS_PAGE_SIZE` (= the nest default);
    /// backs the `admin-users-prev-page` / `-next-page` controls and the
    /// post-action refetch (which preserves the current offset).
    pub fn fetch_admin_users_page(&self, offset: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let limit = Some(crate::views::admin::USERS_PAGE_SIZE);
            let msg = match admin.users_list(limit, offset).await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminUsersLoaded {
                    reply,
                    picker_users: Self::picker_users_read(&admin).await,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch server status (admin only) via `fauna.admin.status` (WS-RPC; the
    /// typed twin of the deleted `GET /admin/api/status`). The dashboard
    /// reads `version` + the `update_available` advisory from the JSON `status`
    /// value; `AdminStatusReply` re-serialized with `serde_json::to_value` is
    /// shape-identical to the old HTTP body. (The view's `uptime`/`workers`
    /// reads are defensive `if let Some` arms the HTTP twin never populated —
    /// live worker/cluster detail lives on the sibling `fauna.admin.cluster.
    /// status`/`fauna.admin.worker.status` kinds, not here — so nothing is lost.)
    pub fn fetch_admin_server_status(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.status().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminServerStatusLoaded {
                    status: serde_json::to_value(&reply).unwrap_or(serde_json::Value::Null),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.status".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the defined tiers (admin only) over `fauna.admin.tiers.list`. The
    /// `admin-users` tier-pickers cycle through these names (the tier is the
    /// quota — admin.md § Users); `admin-settings` renders the definitions.
    pub fn fetch_admin_tiers(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.tiers_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminTiersLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.tiers.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Overwrite a tier definition's caps over `fauna.admin.tiers.update` — the
    /// in-place edit on an `admin-settings-tier-item` row (admin.md § 3). `req`
    /// carries the tier `name` (identifies the row, unchanged) + the edited
    /// raw-i64 caps. On success the caller refetches `tiers.list` so the rows
    /// re-render from persisted state; a missing tier is `fauna.admin.not_found`.
    pub fn update_admin_tier(&self, req: fauna_client_admin::admin::AdminTierUpdateRequest) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.tiers_update(req).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminTierUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.tiers.update".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the calling admin's membership designations (admin only) over
    /// `fauna.admin.membership_tiers.list` — which of the admin's own
    /// subscription tiers already link to paid nest access, and what quota
    /// tiers they run at (monetization.md § Pillar 4). An empty list is the
    /// normal out-of-the-box state (nothing designated yet), not an error.
    pub fn fetch_admin_membership_tiers(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.membership_tiers_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminMembershipTiersLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.membership_tiers.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the calling admin's OWN subscription tier names over
    /// `fauna.subscriptions.tiers.list` — the row set the membership
    /// designation section renders (one row per owned tier, monetization.md
    /// § Pillar 4). A failed read degrades to an empty list, same as
    /// [`fetch_own_tier_names`](Self::fetch_own_tier_names) — the section
    /// renders its "no subscription tiers yet" empty state rather than an
    /// error, since a payee with no tiers has nothing to designate.
    pub fn fetch_own_membership_tier_names(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let subs = fauna_client_subscriptions::SubscriptionsClient::new(nest_rpc);
            let names = match subs.tiers_list().await {
                Ok(tiers) => tiers.into_iter().map(|t| t.name).collect(),
                Err(_) => Vec::new(),
            };
            tx.send(UiMessage::Data(DataMessage::OwnMembershipTierNamesLoaded {
                names,
            }));
        });
    }

    /// Designate/re-point a membership tier over
    /// `fauna.admin.membership_tiers.set` — an upsert, so re-saving the same
    /// row re-points the link rather than conflicting (monetization.md
    /// § Pillar 4). A `tier_name` the caller does not own is
    /// `fauna.admin.not_found`; an unknown quota tier is
    /// `fauna.admin.invalid_params`. On success the caller refetches
    /// `membership_tiers.list` so the row re-renders from persisted state.
    pub fn save_membership_tier(
        &self,
        req: fauna_client_admin::admin::AdminMembershipTierSetRequest,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.membership_tiers_set(req).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminMembershipTierUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.membership_tiers.set".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Drop a membership designation over `fauna.admin.membership_tiers.clear`
    /// — the subscription tier itself is untouched, it just reverts to an
    /// ordinary content tier (monetization.md § Pillar 4). A tier carrying no
    /// designation is `fauna.admin.not_found`.
    pub fn clear_membership_tier(&self, tier_name: String) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.membership_tiers_clear(tier_name).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminMembershipTierUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.membership_tiers.clear".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the sidecar-service enable flags (admin only) over
    /// `fauna.admin.services.list` — the `{ bridge, pairing }` flags; the
    /// `admin-service-pairing-toggle` on `admin-nest` reads them (admin.md § N Nest). The DNS
    /// toggle is driven separately (the "Fauna controls DNS" master switch,
    /// `dns_set_all_managed`); the nest carries no `dns` service flag.
    pub fn fetch_admin_services(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.services_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminServicesLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.services.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the nest's `fauna-log` ring (admin only) over `fauna.admin.logs` —
    /// the `admin-logs` page (observability.md § Surfaces). Rendered with the
    /// same widget as the client's own Settings → Logs page; the client filters
    /// by severity in the widget, so the request is parameterless.
    pub fn fetch_admin_logs(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.logs().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminLogsLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.logs".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Flip one sidecar-service flag (admin only) over
    /// `fauna.admin.services.update` — the `admin-service-pairing-toggle`
    /// click (`name` ∈ {bridge, pairing}; the nest rejects
    /// others). On success the caller refetches so the toggle + status reflect
    /// the applied state.
    pub fn set_service(&self, name: String, enabled: bool) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.services_update(name, enabled).await {
                Ok(_) => UiMessage::Data(DataMessage::AdminServiceUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.services.update".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the deployment's client-facing API serving port over
    /// `fauna.setup.status` (`serving_port`, read straight from the DB singleton,
    /// default 443) for the `admin-nest-serving-port-input`. Anonymous read over
    /// the authed `nest_rpc`; any error leaves the entry at its default rather than
    /// surfacing a page error.
    pub fn fetch_nest_serving_port(&self) {
        use fauna_client::{SetupStatusReply, SetupStatusRequest};

        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let reply: Result<SetupStatusReply, _> = nest_rpc
                .request("fauna.setup.status", SetupStatusRequest::default())
                .await;
            if let Ok(reply) = reply {
                tx.send(UiMessage::Data(DataMessage::NestServingPortLoaded {
                    port: reply.serving_port,
                    fronted: reply.fronted_by_router,
                }));
            }
        });
    }

    /// Fetch the host-OS-maintenance state over `fauna.setup.status` (the `os_*`
    /// fields, surfaced from the host coordinator's `host-status` mount) for the
    /// `nest-os-maintenance-status` indicator on `admin-nest` (installers/vps.md
    /// § Host OS Maintenance § 4). Anonymous read over the authed `nest_rpc`; any
    /// error leaves the indicator at "OS up to date" rather than surfacing a page
    /// error — a sibling of `fetch_nest_serving_port`. A nest with no host channel
    /// reports the serde-default zeros (never a false alarm).
    pub fn fetch_nest_os_maintenance(&self) {
        use fauna_client::{SetupStatusReply, SetupStatusRequest};

        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let reply: Result<SetupStatusReply, _> = nest_rpc
                .request("fauna.setup.status", SetupStatusRequest::default())
                .await;
            if let Ok(reply) = reply {
                tx.send(UiMessage::Data(DataMessage::NestOsMaintenanceLoaded {
                    security_updates_pending: reply.os_security_updates_pending,
                    reboot_pending: reply.os_reboot_pending,
                }));
            }
        });
    }

    /// Expedite the host's idle-gated reboot over `fauna.admin.request_host_restart`
    /// (the `nest-os-restart-now-button` on `admin-nest`), via the shared
    /// `AdminClient` (no per-feature wrapper — a raw `fauna.admin.*` call, the
    /// serving-port twin). The nest writes a `restart-requested` flag the host
    /// reboot-coordinator consumes on its next run; a nest with no maintenance
    /// mount rejects `no_host`, surfaced on the page error. On success the caller
    /// refetches `setup.status` so the indicator reflects the request.
    pub fn request_host_restart(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.request_host_restart().await {
                Ok(()) => UiMessage::Data(DataMessage::NestHostRestartRequested),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.request_host_restart".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Set the deployment-wide client-facing API serving port over
    /// `fauna.admin.set_serving_port` (the `admin-nest-serving-port-save-button`),
    /// via the shared `AdminClient` (no per-feature wrapper — the symmetric twin of
    /// the CalDAV port, a `fauna.admin.*` call). `port` is a validated u16 in
    /// `[1, 65535]` (the view validates before calling). On success the caller
    /// refetches `setup.status` so the entry re-seeds from the persisted port; the
    /// new port binds on the next nest restart (the nest cannot hot-rebind).
    pub fn set_serving_port(&self, port: u16) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.set_serving_port(port).await {
                Ok(()) => UiMessage::Data(DataMessage::NestServingPortSaved),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.set_serving_port".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the declared region over `fauna.admin.region.get` (the
    /// `admin-nest-region-*` section) via the shared `AdminClient`, folded
    /// through `fauna_client_admin::admin_region_view` — every rendering
    /// decision lives there (tui's `admin/nest.rs`, the reference leg), so
    /// this call decides nothing about the plane. A sibling of
    /// `fetch_nest_serving_port`; any error leaves the section as
    /// last-rendered rather than surfacing a page error.
    pub fn fetch_nest_region(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            if let Ok(reply) = admin.region_status().await {
                tx.send(UiMessage::Data(DataMessage::NestRegionLoaded {
                    view: fauna_client_admin::admin_region_view(&reply),
                }));
            }
        });
    }

    /// Declare or withdraw the region over `fauna.admin.region.set` (the
    /// `admin-nest-region-save-button` / `-withdraw-button`), via the shared
    /// `AdminClient`. `region` is `None` for withdraw — same call, no
    /// separate wire kind. `region_entry`'s text is already validated by the
    /// caller (`fauna_client_admin::parse_region_code`); withdraw has nothing
    /// to validate. On success the caller refetches so the section re-seeds
    /// from the persisted declaration (a re-declaration also retires the
    /// previous region's feature-policy document nest-side).
    pub fn set_region(&self, region: Option<fauna_client_admin::RegionCode>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.set_region(region).await {
                Ok(()) => UiMessage::Data(DataMessage::NestRegionSaved),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.region.set".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the deployment's registration posture + free-tier ceiling over
    /// `fauna.setup.status` (`registration_mode` raw wire string +
    /// `max_free_users`) for the `admin-users-registration-section` (admin.md § 2
    /// Users → Section 2 — Registration). Anonymous read over the authed
    /// `nest_rpc`, the same shape as `fetch_nest_serving_port`; any error leaves
    /// the section as last-rendered rather than surfacing a page error. `mode` is
    /// deliberately the RAW wire string (not pre-parsed): the view
    /// (`views::admin::set_registration_mode`) is what decides `None`/unparseable
    /// ⇒ read-only, per public-mode.md's never-coerce rule.
    pub fn fetch_registration_mode(&self) {
        use fauna_client::{SetupStatusReply, SetupStatusRequest};

        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let reply: Result<SetupStatusReply, _> = nest_rpc
                .request("fauna.setup.status", SetupStatusRequest::default())
                .await;
            if let Ok(reply) = reply {
                tx.send(UiMessage::Data(DataMessage::RegistrationModeLoaded {
                    mode: reply.registration_mode,
                    max_free_users: reply.max_free_users,
                    age_verification_required: reply.age_verification_required,
                }));
            }
        });
    }

    /// Save the registration posture + free-tier ceiling over
    /// `fauna.admin.set_registration_mode` (the
    /// `admin-users-registration-save-button`), via the shared `AdminClient` —
    /// ONE call carries both, mode + cap being one admin decision (admin.md § 2
    /// Users → Section 2 — Registration). `mode` is the wire string the
    /// registration DropDown's model shows
    /// (`views::admin::build_registration_mode_dropdown` builds that model from
    /// `RegistrationMode::as_wire_str` values only), so this always parses in
    /// practice; an unparseable `mode` surfaces as a failed action rather than
    /// silently no-op'ing. The read-only guard in `views::admin::set_registration_mode`
    /// is what actually prevents a guessed mode from ever reaching this call.
    ///
    /// `age_verification` is the age require-knob's new value when its toggle
    /// changed (`None` = unchanged, nothing sent): it is dispatched as
    /// `fauna.admin.set_age_verification_required` beside the mode, in the
    /// same gesture (`family-safety.md` § App surface → *Age-band surfaces*).
    pub fn set_registration_mode(
        &self,
        mode: String,
        max_free_users: Option<u64>,
        age_verification: Option<bool>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let Some(parsed) = fauna_client_admin::RegistrationMode::from_wire_str(&mode) else {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.set_registration_mode".into(),
                    error: format!("unrecognized registration mode: {mode}"),
                }));
                return;
            };
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            if let Err(e) = admin.set_registration_mode(parsed, max_free_users).await {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.set_registration_mode".into(),
                    error: e.to_string(),
                }));
                return;
            }
            if let Some(required) = age_verification
                && let Err(e) = admin.set_age_verification_required(required).await
            {
                tx.send(UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.set_age_verification_required".into(),
                    error: e.to_string(),
                }));
                return;
            }
            tx.send(UiMessage::Data(DataMessage::RegistrationModeSaved));
        });
    }

    /// Build the shared admin NAT-mode machine
    /// (`fauna_onboarding_machine::AdminNatModeMachine`) for the `admin-nest`
    /// page's NAT-mode control (admin.md § Nest → NAT-mode control). Fresh per
    /// page build. The machine rides the pre-identity WS-RPC transport — the
    /// admin payload signature (this identity's secret) is the authorization,
    /// not this client's bearer connection — so it takes the raw session pair
    /// rather than `nest_rpc`.
    pub fn admin_nat_mode_machine(
        &self,
    ) -> std::sync::Arc<fauna_onboarding_machine::AdminNatModeMachine> {
        fauna_onboarding_machine::AdminNatModeMachine::new(
            self.node_url.clone(),
            self.secret_hex.clone(),
        )
    }

    /// Change a user's tier (= the quota) over `fauna.admin.users.update`. The
    /// `admin-users-tier-select` control on a user row; on success the caller
    /// refetches the list so the row reflects the new tier.
    pub fn update_admin_user_tier(&self, actor_id: Vec<u8>, tier: String, label: String) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.users_update(actor_id, tier, label).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.update".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Admit a known actor id directly over `fauna.admin.users.create` — the
    /// `admin-users-admit-button` (`public-mode.md` § Registration & Identity,
    /// the third account-creation path). `label` is always empty (tui's own
    /// idiom — the shared writer's `label` field is unused by any app UI
    /// today). On success the caller refetches the users list at the current
    /// page, same as [`Self::update_admin_user_tier`]/[`Self::evict_user`],
    /// so the newly admitted account is the visible feedback.
    pub fn admit_user(&self, actor_id: Vec<u8>, tier: String, handle: Option<String>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin
                .users_create(actor_id, tier, String::new(), handle)
                .await
            {
                Ok(()) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.create".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Start a user's eviction timeline (warn→suspend→delete) over
    /// `fauna.admin.users.evict` — the `admin-users-evict-button` on a user row.
    /// One-click with a default reason + the `other` category (the only inputs
    /// the row exposes); the user is not deleted, and the row's refetch shows the
    /// cancel control. On success the caller refetches at the current page.
    pub fn evict_user(&self, actor_id: Vec<u8>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let reason = crate::i18n::strings::admin::users_page::EVICT_DEFAULT_REASON;
            let msg = match admin.users_evict(actor_id, reason, "other").await {
                Ok(()) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.evict".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Suspend a user **immediately** over `fauna.admin.users.suspend` — the
    /// `admin-users-suspend-button` on a user row. Unlike [`Self::evict_user`]
    /// this schedules no deletion: it enters the eviction machine's `suspended`
    /// state at once and leaves `eviction_delete_at` null (`admin.md` § 2 Users →
    /// *Cutting a user off*). It is the "stop this account now" path the timed
    /// ladder cannot serve. Reversed by [`Self::cancel_user_eviction`] — suspend
    /// and evict share one exit — so the row's refetch shows the cancel control.
    pub fn suspend_user(&self, actor_id: Vec<u8>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let reason = crate::i18n::strings::admin::users_page::SUSPEND_DEFAULT_REASON;
            let msg = match admin
                .users_suspend(actor_id, reason.to_string(), "other".to_string())
                .await
            {
                Ok(_) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.suspend".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Cancel a user's in-flight eviction over `fauna.admin.users.cancel_eviction`
    /// — the `admin-users-cancel-eviction-button` on a user row. On success the
    /// caller refetches at the current page so the row shows the evict control.
    pub fn cancel_user_eviction(&self, actor_id: Vec<u8>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.users_cancel_eviction(actor_id).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.users.cancel_eviction".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Grant the admin role over `fauna.admin.admins.add` — the
    /// `admin-users-make-admin-button` on a plain (non-admin) user row. Schedules
    /// an `AdminAdd` pending action (24h delay, `admin.md` § Admin continuity and
    /// succession) — the row does not flip to an admin row right away; a
    /// scheduled reply (no error) is success. On success the caller refetches at
    /// the current page.
    pub fn make_admin(&self, actor_id: Vec<u8>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.admins_add(actor_id).await {
                Ok(_) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.admins.add".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Revoke the admin role over `fauna.admin.admins.remove` — the
    /// `admin-users-remove-admin-button` on an `is_admin` row. Schedules an
    /// `AdminRemove` pending action; refuses (`fauna.admin.conflict`) when it
    /// would leave zero superadmins — the nest, not the client, makes that call.
    /// On success the caller refetches at the current page.
    pub fn remove_admin(&self, actor_id: Vec<u8>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.admins_remove(actor_id).await {
                Ok(_) => UiMessage::Data(DataMessage::AdminUserUpdated),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.admins.remove".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch invite codes (admin only) over `fauna.admin.invite_codes.list`.
    pub fn fetch_admin_invite_codes(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.invite_codes_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminInviteCodesLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_codes.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Mint a closed-registration invite code (admin only) over
    /// `fauna.admin.invite_codes.create`. An empty `code` ⇒ the nest mints a
    /// random token (admin.md § 3, mint-on-empty); the reply carries the token
    /// the `admin-users` Invite section surfaces copyable.
    ///
    /// `guardian` (from `admin-users-invite-guardian-select`, `None` = the
    /// default "None" option) links the redeemed account to a guardian —
    /// supervised admission, `family-safety.md` § Wire & data shape. The nest
    /// re-validates the guardian (exists / not suspended / not itself
    /// supervised / ≠ the admitted actor).
    ///
    /// `age_band` (the form's `admin-users-invite-age-band-select`) is the band
    /// the redeemed account is admitted under — it rides only beside a guardian
    /// (the nest refuses it otherwise; the view sends `None` without one).
    pub fn create_admin_invite_code(
        &self,
        tier: String,
        uses: i64,
        guardian: Option<Vec<u8>>,
        age_band: Option<fauna_protocol::age::AgeBand>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin
                .invite_codes_create("", tier, uses, guardian, age_band)
                .await
            {
                Ok(reply) => UiMessage::Data(DataMessage::AdminInviteCodeCreated { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_codes.create".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Delete an invite code (admin only) over `fauna.admin.invite_codes.delete`.
    pub fn delete_admin_invite_code(&self, code: String) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.invite_codes_delete(code).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminInviteCodeDeleted),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_codes.delete".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch pending+decided invite requests (admin only) over
    /// `fauna.admin.invite_requests.list` — the `admin-users` Pending requests
    /// section.
    pub fn fetch_admin_invite_requests(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.invite_requests_list().await {
                Ok(reply) => UiMessage::Data(DataMessage::AdminInviteRequestsLoaded { reply }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_requests.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Approve a pending invite request by id (admin only) over
    /// `fauna.admin.invite_requests.approve`, admitting at the chosen `tier`.
    /// Creates the user and deletes the row; the caller refetches on success.
    ///
    /// `guardian` (from the row's `invite-request-row-guardian-select`, `None` =
    /// the default "None" option) admits the account **supervised** by that
    /// guardian — the link, policy, and user row land in one admission
    /// transaction (`family-safety.md` § Wire & data shape). `age_band` (the
    /// row's `invite-request-row-age-band-select`) rides beside that guardian
    /// only — `None` without one (`family-safety.md` § App surface → *Age-band
    /// surfaces*).
    pub fn approve_invite_request(
        &self,
        id: i64,
        tier: Option<String>,
        guardian: Option<Vec<u8>>,
        age_band: Option<fauna_protocol::age::AgeBand>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin
                .invite_requests_approve(id, tier, None, guardian, age_band)
                .await
            {
                Ok(_reply) => UiMessage::Data(DataMessage::AdminInviteRequestDecided),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_requests.approve".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Deny a pending invite request by id (admin only) over
    /// `fauna.admin.invite_requests.deny`. `reason`, if present, is surfaced to
    /// the requester on their invite_request_pending screen.
    pub fn deny_invite_request(&self, id: i64, reason: Option<String>) {
        let reason = reason.filter(|s| !s.is_empty());
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let admin = fauna_client_admin::AdminClient::new(nest_rpc);
            let msg = match admin.invite_requests_deny(id, reason).await {
                Ok(()) => UiMessage::Data(DataMessage::AdminInviteRequestDecided),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.admin.invite_requests.deny".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Generic API helpers
    //
    // All bearer-authed calls to our nest funnel through `self.content_api`
    // (see `crate::nest_content_api`), which owns the bearer attach and the
    // 401-reactive token refresh. These helpers only translate the result
    // into a `UiMessage`.
    // -----------------------------------------------------------------------

    /// Map a [`crate::nest_content_api::NestContentApi`] result to a
    /// `UiMessage`: `on_success(body)` for a `2xx`, `ActionResult::ApiFailed`
    /// (the nest's structured error) for a non-2xx, `ActionResult::Failed`
    /// (keyed on the full request URL, matching the legacy behavior) for a
    /// transport / no-bearer failure.
    fn dispatch_api_result(
        result: Result<bytes::Bytes, crate::nest_content_api::ApiError>,
        endpoint: String,
        url: String,
        on_success: impl FnOnce(bytes::Bytes) -> UiMessage,
    ) -> UiMessage {
        use crate::nest_content_api::ApiError;
        match result {
            Ok(body) => on_success(body),
            Err(ApiError::Status { code, message }) => UiMessage::Action(ActionResult::ApiFailed {
                endpoint,
                status: code,
                message,
            }),
            Err(ApiError::Transport(error)) => UiMessage::Action(ActionResult::Failed {
                context: url,
                error,
            }),
            // The nest's pinned identity changed. This reports it on the page
            // that asked; the *blocking* re-trust surface is reached the way it
            // already is on linux — `DataMessage::NestIdentityChanged` →
            // `settings::trigger_nest_identity_changed` — never a second,
            // softer per-page shape (`security.md` § Post-auth surfacing).
            // The mid-session sign-in and succession refusals likewise: the
            // page reports them, and the launch surface is reached through the
            // supervisor stop / silent sign-in → `DataMessage::SignInRefused` /
            // `DataMessage::IdentitySuperseded`.
            Err(
                e @ (ApiError::NestIdentityChanged { .. }
                | ApiError::SignInRefused
                | ApiError::Superseded { .. }),
            ) => UiMessage::Action(ActionResult::Failed {
                context: url,
                error: e.to_string(),
            }),
        }
    }

    /// GET our nest's `path` with the session bearer. Calls `on_success`
    /// with the response body bytes on `2xx`; on a non-2xx sends
    /// `ActionResult::ApiFailed` with the structured error; on a transport
    /// / no-bearer failure sends `ActionResult::Failed`.
    pub fn api_get<F>(&self, path: &str, on_success: F)
    where
        F: FnOnce(bytes::Bytes) -> UiMessage + Send + 'static,
    {
        let url = format!("{}{}", self.node_url, path);
        let endpoint = path.to_string();
        let nest = Arc::clone(&self.content_api);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let r = nest.get(&endpoint).await;
            tx.send(Self::dispatch_api_result(r, endpoint, url, on_success));
        });
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Returns the actor_id derived from the cached secret. Always Some
    /// when the secret is well-formed (32 bytes hex). Previously read
    /// from the AuthState cache; that's gone now since LaunchMachine
    /// owns the bearer lifecycle.
    ///
    /// **Derived once.** `secret_hex` never changes for a given client (an
    /// actor switch builds a new one), and deriving is an Ed25519 scalar
    /// multiplication — cheap alone, and not when a caller treats this like a
    /// field read. The feed's per-post render did exactly that
    /// (`post_list::build_web_publish_verbs`), and a captured UI-thread stall
    /// sat inside `curve25519_dalek` under this accessor while the GTK main
    /// loop was held. An accessor that *looks* free
    /// should be free.
    pub fn actor_id(&self) -> Option<String> {
        self.actor_id
            .get(|| actor_id_from_secret_hex(&self.secret_hex))
    }

    /// Returns a clone of the UiSender.
    pub fn tx(&self) -> UiSender {
        self.tx.clone()
    }

    /// Returns the node URL.
    pub fn node_url(&self) -> &str {
        &self.node_url
    }

    /// Returns the secret hex.
    pub fn secret_hex(&self) -> &str {
        &self.secret_hex
    }

    /// Returns a handle to the underlying tokio runtime.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime_handle.clone()
    }

    /// A clone of this client's `UiMessage` sender — for a long-lived
    /// background driver that posts into the GTK loop on its own schedule
    /// rather than through one of this type's per-gesture methods
    /// (`crate::share_glue`, the share plane's pump).
    pub fn ui_sender(&self) -> UiSender {
        self.tx.clone()
    }

    /// Spawn a background task on the client's runtime. No-ops if the runtime
    /// has already been torn down by `shutdown()` — so a leaked GTK signal
    /// closure that fires after sign-out (the window widget-tree cycle keeps a
    /// few alive) silently does nothing instead of panicking on a dead handle.
    fn spawn_bg<F>(&self, fut: F)
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if let Some(rt) = self.runtime.borrow().as_ref() {
            rt.spawn(fut);
        }
    }

    /// Tear down the background runtime, aborting all spawned tasks and
    /// releasing their fds (WebSocket, sync-engine inotify watcher, P2P,
    /// inbound poll). Idempotent and cheap to call when already shut down.
    ///
    /// Called from the e2e reset/sign-out path *before* `current_client` is
    /// cleared, so the old client's background work stops immediately even
    /// though leaked signal-handler closures still hold `Rc<FaunaClient>`
    /// clones (the authenticated-window widget-tree reference cycle,
    /// tracked internally). Without this, every leaked
    /// client keeps reconnecting/polling, starving the GTK main thread 16–21 s
    /// and exhausting inotify instances across the full suite.
    pub fn shutdown(&self) {
        // Drop the persistent admin-dns machine so a sign-out/reset rebuilds it
        // against the next connection (and any suspended manual cert order is
        // abandoned with the session, not leaked into the new one).
        self.dns_machine.borrow_mut().take();
        if let Some(rt) = self.runtime.borrow_mut().take() {
            rt.shutdown_background();
        }
    }

    /// Returns the current bearer token from LaunchMachine, or None if
    /// not Online (e.g. mid-refresh, or the launch flow is still routing).
    pub fn token(&self) -> Option<String> {
        self.machine.current_bearer()
    }

    /// Returns a clone of the internal `reqwest::Client`.
    pub fn http_client(&self) -> reqwest::Client {
        self.http.clone()
    }

    // -----------------------------------------------------------------------
    // Notifications
    // -----------------------------------------------------------------------

    /// Fetch the latest unified notifications (likes, replies, follows,
    /// reposts, mentions, quotes — across fauna, bluesky, nostr, AP).
    /// No-op if the client isn't yet authenticated.
    pub fn fetch_notifications(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let notifs = fauna_client_notifications::NotificationsClient::new(nest_rpc);
            let msg = match notifs.notifications_list(None, Some(20)).await {
                Ok(reply) => UiMessage::Data(DataMessage::NotificationsLoaded {
                    notifications: reply.notifications,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.notifications.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Bluesky thread
    // -----------------------------------------------------------------------

    /// Fetch the Bluesky thread context for a crossposted post via the
    /// `bluesky.feed.thread` WS-RPC kind (`PostId` variant — the post-detail
    /// surface holds the hex `[u8; 32]` Fauna post id, which the handler
    /// resolves to its AT-URI through the `bluesky_posts` crosspost mapping).
    /// The reply is the flat thread list: ancestors oldest-first, the focal
    /// post, then its direct replies; `focal_index` names the focal post. Sends
    /// `DataMessage::BlueskyThreadLoaded`.
    pub fn fetch_bluesky_thread(&self, post_id: &str) {
        use fauna_client_bluesky::bluesky::BlueskyThreadRequest;
        let pid = post_id.to_string();
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let post_id = match hex32(&pid) {
                Some(bytes) => bytes.to_vec(),
                None => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "bluesky.feed.thread".into(),
                        error: "invalid post id: expected 32 hex-encoded bytes".into(),
                    }));
                    return;
                }
            };
            let client = fauna_client_bluesky::BlueskyClient::new(nest_rpc);
            let msg = match client
                .thread(BlueskyThreadRequest::PostId { post_id })
                .await
            {
                Ok(reply) => UiMessage::Data(DataMessage::BlueskyThreadLoaded {
                    post_id: pid,
                    posts: reply.posts,
                    focal_index: reply.focal_index as usize,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "bluesky.feed.thread".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // WS-RPC — long-lived NestClient connection with typed push routing
    // -----------------------------------------------------------------------

    /// Connect the shared `NestClient` and spawn the connection-state +
    /// push pumps. The supervisor inside `NestClient::connect()` handles
    /// reconnect + bearer refresh; the pumps translate its outputs into
    /// `WsEvent::{Connected,Disconnected,Push(_)}` for the UI thread.
    ///
    /// Called once from the `DataMessage::AuthSuccess` arm post-login.
    pub fn start_ws_rpc(&self) {
        use crate::app::WsEvent;
        use fauna_client::ConnectionState;
        use tokio::sync::broadcast::error::RecvError;

        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx_connect = self.tx.clone();
        let tx_state = self.tx.clone();
        let tx_push = self.tx.clone();
        let tx_reconn = self.tx.clone();

        // (a) Drive the reconnect supervisor. `connect()` only initiates the
        // handshake, so its `Err` is only ever the authentication step. What
        // the supervisor meets later — including a terminal stop such as a
        // version skew — arrives on requests (`NestClient::supervisor_stop`),
        // and the pumps below observe the watch transition to Disconnected.
        let nest_rpc_connect = Arc::clone(&nest_rpc);
        self.spawn_bg(async move {
            match nest_rpc_connect.connect().await {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!("ws-rpc: connect error: {}", e);
                    tx_connect.send(UiMessage::Realtime(WsEvent::Disconnected));
                }
            }
        });

        // (b) Connection-state pump. `connection_state()` is a
        // `watch::Receiver`, which only stores the latest value — rapid
        // transitions may collapse to the final state, which is fine: the
        // indicator only needs the latest. Forward all four states so the
        // sidebar connection-status indicator reads Connected / Connecting /
        // Disconnected / Cannot connect.
        fn ws_event_for(state: ConnectionState) -> WsEvent {
            match state {
                ConnectionState::Connected => WsEvent::Connected,
                ConnectionState::Connecting => WsEvent::Connecting,
                ConnectionState::Disconnected => WsEvent::Disconnected,
                ConnectionState::Unreachable => WsEvent::Unreachable,
            }
        }
        let nest_rpc_state = Arc::clone(&nest_rpc);
        self.spawn_bg(async move {
            let mut cs_rx = nest_rpc_state.connection_state();
            // Emit the initial state once.
            let initial = *cs_rx.borrow_and_update();
            tx_state.send(UiMessage::Realtime(ws_event_for(initial)));
            while cs_rx.changed().await.is_ok() {
                let state = *cs_rx.borrow_and_update();
                tx_state.send(UiMessage::Realtime(ws_event_for(state)));
                // A supervisor that stopped for good records why before it
                // announces `Disconnected`. A session-ending verdict — above
                // all a post-4401 re-mint the nest refused because the user
                // was suspended — escalates to the launch surface
                // (`security.md` § Post-auth surfacing).
                if state == ConnectionState::Disconnected
                    && let Some(escalation) = nest_rpc_state
                        .supervisor_stop()
                        .and_then(|stop| session_ending_escalation(&stop))
                {
                    tx_state.send(UiMessage::Data(escalation));
                    break;
                }
            }
        });

        // (b2) Reconnect pump. The shared `NestClient` bumps a counter on every
        // reconnect (Connected after the first connect); each bump → one
        // `WsEvent::Reconnected`, which re-hydrates the visible snapshot surfaces
        // (the feed has no poll backstop). Distinct from the state pump above,
        // which fires `Connected` on the initial connect too. transport.md § Push
        // events: observers re-pull through their snapshot-refresh path.
        let nest_rpc_reconn = Arc::clone(&nest_rpc);
        self.spawn_bg(async move {
            let mut rx = nest_rpc_reconn.subscribe_reconnects();
            while rx.changed().await.is_ok() {
                let _ = *rx.borrow_and_update();
                tx_reconn.send(UiMessage::Realtime(WsEvent::Reconnected));
            }
        });

        // (c) Push pump. `subscribe_pushes()` is a `broadcast::Receiver`;
        // on `Lagged(n)` the broker drops the oldest events but the
        // receiver remains usable — log + continue. `Closed` only fires
        // when every sender is dropped (the broker is `Arc`d alongside
        // `NestClient`), so it indicates terminal shutdown.
        let nest_rpc_push = Arc::clone(&nest_rpc);
        self.spawn_bg(async move {
            let mut pushes = nest_rpc_push.subscribe_pushes();
            loop {
                match pushes.recv().await {
                    Ok(event) => {
                        tx_push.send(UiMessage::Realtime(WsEvent::Push(Box::new(event))));
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "ws-rpc: push pump lagged, dropped {} events \
                             (server will emit ResyncRequired)",
                            n
                        );
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });

        // (d) Auto-renew cadence (tls-certificates.md § C.3 C2 — the native
        // auto-issue half). Periodically re-read the served-cert health + the
        // shared auto-renew decision and auto-issue any at-risk managed/delegated
        // domain, so certificate renewal is hands-off from any synced native
        // device — no admin tap. Native-only (the DNS-01 order core is native).
        // Uses its OWN machine, independent of the page's persistent one
        // (`dns_machine()`), so a background `Refresh` never disturbs the page's
        // rendered red/green verdicts; the ACME account is shared via the
        // account's DNS record (`fauna.state.dns`), so account reuse holds across both. The loop fires
        // only after the first full interval, so it never disturbs a short-lived
        // session (incl. e2e). Skipped only on the impossible keypair-derive
        // failure (launch already validated `secret_hex`).
        if let Ok(keypair) = secret_to_keypair(&self.secret_hex) {
            let nest_rpc_renew = Arc::clone(&nest_rpc);
            let tx_renew = self.tx.clone();
            self.spawn_bg(async move {
                let machine = crate::mail_glue::build_dns_management_machine_with_credentials(
                    Arc::clone(&nest_rpc_renew),
                    keypair,
                    Arc::new(crate::account_runtime::handle),
                );
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(
                        fauna_client_dns::auto_renew_poll_secs(),
                    ))
                    .await;
                    run_auto_renew_cadence_tick(&machine, &nest_rpc_renew, &tx_renew).await;
                }
            });
        }
    }

    /// Round-trip `fauna.protocol.echo` over WS-RPC, write the reply
    /// (hex of `EchoReply::data`) into `shared.rpc_echo_reply` for the
    /// test agent to surface. Test-agent only; the production code path
    /// uses `nest_rpc().request(...)` directly.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn rpc_echo(
        &self,
        data: Vec<u8>,
        shared: std::sync::Arc<std::sync::Mutex<crate::test_agent::SharedState>>,
    ) {
        use fauna_client::{EchoReply, EchoRequest};

        let nest_rpc = Arc::clone(&self.nest_rpc);
        self.spawn_bg(async move {
            let req = EchoRequest {
                data,
                extra: std::collections::BTreeMap::new(),
            };
            let reply: Result<EchoReply, _> = nest_rpc.request("fauna.protocol.echo", req).await;
            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            guard.rpc_echo_reply = Some(match reply {
                Ok(r) => crate::test_agent::RpcEchoOutcome::Ok {
                    data_hex: hex::encode(&r.data),
                },
                Err(e) => crate::test_agent::RpcEchoOutcome::Err {
                    error: e.to_string(),
                },
            });
        });
    }

    /// Mint this actor's shared MSEK + `default` credential via the CalDAV-enable
    /// recipe (`MailSettingsMachine::enable_caldav_mailbox_with_generated_password`,
    /// the read-only mailbox material — no submission token, no deployment toggle),
    /// writing the outcome into `shared.caldav_mailbox_reply` for the test agent to
    /// surface. **Test-agent only.** It mints a CalDAV mailbox for the *currently
    /// logged-in* (possibly non-admin) actor, which has no production caller yet:
    /// the production paths are the admin onboarding glue
    /// (the CalDAV-only mint step of [`Self::apply_post_claim_serving_enablement`]) and the not-yet-built
    /// non-admin CalDAV auto-enable policy (`caldav-server.md` § Independent
    /// enablement). Slice C (`test_caldav_autoschedule_mailbox_less.py`) uses it to
    /// give a mailbox-less GUI attendee — registered with a handle but no alias, so
    /// not onboarded through the launch glue — the MSEK her `NestSchedulingSink`
    /// needs to materialize a server-side auto-schedule invite.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn enable_caldav_mailbox_for_test(
        &self,
        password: Option<String>,
        shared: std::sync::Arc<std::sync::Mutex<crate::test_agent::SharedState>>,
    ) {
        let machine = match crate::mail_glue::build_mail_settings_machine(self) {
            Ok(m) => m,
            Err(e) => {
                let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                guard.caldav_mailbox_reply = Some(crate::test_agent::CalDavMailboxOutcome::Err {
                    error: format!("build machine: {e}"),
                });
                return;
            }
        };
        self.spawn_bg(async move {
            // A caller-provided password mints a credential the test knows (so a
            // stock CalDAV client can AUTH as this actor); otherwise generate one.
            let outcome = match password {
                Some(pw) => {
                    machine
                        .enable_caldav_mailbox_with_password("Default".to_string(), pw)
                        .await
                }
                None => machine
                    .enable_caldav_mailbox_with_generated_password("Default".to_string())
                    .await
                    .map(|_password| ()),
            };
            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            guard.caldav_mailbox_reply = Some(match outcome {
                Ok(()) => crate::test_agent::CalDavMailboxOutcome::Ok,
                Err(e) => crate::test_agent::CalDavMailboxOutcome::Err {
                    error: format!("{e:?}"),
                },
            });
        });
    }

    /// Arrange a WebDAV-served, content-keyed folder for the tier_3 read+write
    /// e2e (`tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py`): if
    /// `create`, mint an empty Sync set; run the serve-ON orchestration
    /// ([`FoldersAuthor::serve_enable`] — content-key genesis and the nest
    /// `webdav_enabled` flag) for the unshared set; then provision the
    /// MSEK-sealed `WebdavKeysBlob` ([`reconcile_webdav_keys_blob`]). Writes the
    /// outcome into `shared.webdav_serve_reply`. **Test-agent only.** It is the
    /// FIRST caller of `serve_enable`/`reconcile_webdav_keys_blob` — the
    /// production caller is the not-yet-built slice-6 per-set serve toggle
    /// (`webdav-server.md` § Independent enablement), so this drives the real
    /// slice-2 orchestration + the real MDA cross-binary seal the e2e proves.
    /// Requires mail already enabled (the reconcile seals under the MSEK
    /// `EnableMail` mints) and the conversations session live (the shared
    /// `MlsEngine` the author is built over).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn serve_enable_folder_for_test(
        &self,
        set: String,
        create: bool,
        shared: std::sync::Arc<std::sync::Mutex<crate::test_agent::SharedState>>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        self.spawn_bg(async move {
            let outcome = serve_enable_folder_flow(&nest_rpc, &secret_hex, &set, create).await;
            let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            guard.webdav_serve_reply = Some(match outcome {
                Ok(served_sets) => crate::test_agent::WebdavServeOutcome::Ok { served_sets },
                Err(e) => crate::test_agent::WebdavServeOutcome::Err { error: e },
            });
        });
    }

    // -----------------------------------------------------------------------
    // Update check
    // -----------------------------------------------------------------------

    /// The once-per-sign-in look for a newer version (`installers/README.md`
    /// § Knowing a newer version is out): one unasked round trip through the
    /// shared `fauna_client::update_look`, sending `DataMessage::UpdateAvailable`
    /// only when a newer release is out. A failed look is silent by rule; the
    /// asked check on Settings → General keeps its own error.
    pub fn check_for_updates(&self) {
        let tx = self.tx.clone();
        let http = self.http.clone();

        self.spawn_bg(async move {
            // Small delay so the look doesn't race startup I/O.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            if let Some(tag) = crate::updater::look_at_sign_in(&http).await {
                tx.send(UiMessage::Data(DataMessage::UpdateAvailable {
                    version: tag.trim_start_matches('v').to_string(),
                    url: fauna_core::version::release_page_url(&tag),
                }));
            }
        });
    }

    // -----------------------------------------------------------------------
    // SMTP send API
    // -----------------------------------------------------------------------

    // A hand-rolled `send_email_smtp` RFC 5322 composer lived here but was dead
    // (no UI caller, no `ui.yaml` element, no e2e) and diverged from web's twin
    // while both bypassed the canonical WASM-safe composer
    // `fauna_conversations::rfc5322::build_message`. Removed (drift, #2/#4). The
    // live mail-send path is `fauna_client_email::EmailClient::send` (used by the
    // CalDAV iMIP fan-out); the general mail-compose surface (`ui.yaml`
    // "mail-write" track) will compose via `build_message` and submit through it.

    // -----------------------------------------------------------------------
    // Feed — single-feed fetch & update
    // -----------------------------------------------------------------------

    /// Fetch a single custom feed by ID via `fauna.feed.get` (WS-RPC). The
    /// typed reply (including its `rules`) is serialized back to JSON under
    /// the `feed_loaded:` context — a transport-only swap of the legacy GET
    /// (no UI consumer wires the edit flow yet; a future feature session would
    /// replace the string-packed JSON with a typed `DataMessage`).
    pub fn get_feed(&self, feed_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let feed_id = feed_id.to_string();
        self.spawn_bg(async move {
            let feed = fauna_client_feed::FeedClient::new(nest_rpc);
            let msg = match feed.feed_get(feed_id).await {
                Ok(reply) => {
                    let json = serde_json::to_string(&reply).unwrap_or_default();
                    UiMessage::Action(ActionResult::Success {
                        context: format!("feed_loaded:{}", json),
                    })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.feed.get".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Update a custom feed's name, combination rule, and filter rules via
    /// `fauna.feed.update` (WS-RPC). `rules` ride typed — build them from
    /// the form with the shared `fauna_client_feed::encode_filter_rules`.
    pub fn update_feed(
        &self,
        feed_id: &str,
        name: &str,
        combination: &str,
        rules: Vec<fauna_core::scoring::FilterRule>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let feed_id = feed_id.to_string();
        let name = name.to_string();
        let combination = combination.to_string();
        self.spawn_bg(async move {
            let feed = fauna_client_feed::FeedClient::new(nest_rpc);
            let msg = match feed
                .feed_update(feed_id, name, rules, combination, None)
                .await
            {
                Ok(_reply) => UiMessage::Action(ActionResult::Success {
                    context: "feed_updated".into(),
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.feed.update".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    // -----------------------------------------------------------------------
    // Event — Reminders
    // -----------------------------------------------------------------------

    /// Load the reminder offset for an event from the encrypted store's VEVENT
    /// `VALARM`. Emits `EventReminderLoaded` (offset `None` when no alarm is
    /// set). `uid_hash_hex` is the `EventRow::id` the detail panel holds.
    pub fn get_reminder(&self, calendar_id: &str, uid_hash_hex: &str) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                return;
            };
            let client = CalDavClient::new(nest);
            let Ok(decoded) =
                query_events_in(&client, &actor_id, &cal_id, &msek, &prior_mseks).await
            else {
                return;
            };
            let offset = decoded
                .iter()
                .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hex)
                .and_then(|d| fauna_client_caldav::parse_ical(&d.ics).ok())
                .map(|f| f.alarm)
                .filter(|a| !a.is_empty());
            tx.send(UiMessage::Data(DataMessage::EventReminderLoaded {
                event_id: uid_hex.clone(),
                offset,
            }));
        });
    }

    /// Set a reminder offset (e.g. `PT1H`) on an event — a read-mutate-rewrite
    /// that writes the VEVENT `VALARM` and re-PUTs the sealed body. Emits the new
    /// `EventReminderLoaded` so the detail panel flips to the current-reminder
    /// state.
    pub fn set_reminder(&self, calendar_id: &str, uid_hash_hex: &str, offset: &str) {
        self.rewrite_reminder(
            calendar_id,
            uid_hash_hex,
            offset.to_string(),
            "reminder_set",
        );
    }

    /// Remove the reminder (clears the `VALARM`) via the same read-mutate-rewrite
    /// as [`Self::set_reminder`] with an empty offset.
    pub fn remove_reminder(&self, calendar_id: &str, uid_hash_hex: &str) {
        self.rewrite_reminder(calendar_id, uid_hash_hex, String::new(), "reminder_removed");
    }

    fn rewrite_reminder(
        &self,
        calendar_id: &str,
        uid_hash_hex: &str,
        offset: String,
        ok_context: &'static str,
    ) {
        let nest = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let secret_hex = self.secret_hex.clone();
        let cal_hex = calendar_id.to_string();
        let uid_hex = uid_hash_hex.to_string();
        self.spawn_bg(async move {
            let Some(cal_id) = hex32(&cal_hex) else {
                return;
            };
            let Some(DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            }) = caldav_context(&nest, &secret_hex).await
            else {
                return;
            };
            let client = CalDavClient::new(nest);
            let Ok(decoded) =
                query_events_in(&client, &actor_id, &cal_id, &msek, &prior_mseks).await
            else {
                return;
            };
            let Some(target) = decoded
                .iter()
                .find(|d| fauna_core::format::hex_full(&d.uid_hash) == uid_hex)
            else {
                return;
            };
            let rw =
                match caldav_backend::set_reminder(&target.ics, target.fauna_ext.as_ref(), &offset)
                {
                    Ok(rw) => rw,
                    Err(e) => {
                        tx.send(UiMessage::Action(ActionResult::Failed {
                            context: "reminder".into(),
                            error: e,
                        }));
                        return;
                    }
                };
            let put = client
                .seal_and_put_event(
                    &actor_id,
                    &cal_id,
                    &uid_hash(&rw.fields.uid),
                    &msek,
                    &rw.fields,
                    &rw.attendees,
                    &rw.organizer_email,
                    rw.fauna_ext.as_ref(),
                    now_secs(),
                    None,
                )
                .await;
            match put {
                Ok(_) => {
                    tx.send(UiMessage::Data(DataMessage::EventReminderLoaded {
                        event_id: uid_hex.clone(),
                        offset: if offset.is_empty() {
                            None
                        } else {
                            Some(offset)
                        },
                    }));
                    tx.send(UiMessage::Action(ActionResult::Success {
                        context: ok_context.into(),
                    }));
                }
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.bridges.put_event_ciphertext".into(),
                        error: e.to_string(),
                    }));
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Sync Devices API
    // -----------------------------------------------------------------------

    /// Fetch the enrolled-device roster for one folder via
    /// `fauna.folders.members.list`, lazy-loaded when a Devices-page
    /// folder row expands. Sends `DataMessage::FolderMembersLoaded`.
    pub fn fetch_folder_members(&self, name: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            let folders = fauna_client_folders::FoldersClient::new(nest_rpc);
            let msg = match folders.members_list(name.clone()).await {
                Ok(reply) => UiMessage::Data(DataMessage::FolderMembersLoaded {
                    name,
                    members: reply.members,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.folders.members.list".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the cross-user **actor** roster (the owner-side "Shared with" list)
    /// for one folder via `fauna.folders.members.list_actors`, lazy-loaded when
    /// a Folders-page row expands (and eagerly for already-shared rows so the
    /// `folder-shared-badge` renders without an expand). `mls_group_id` is the
    /// set's raw MLS group id (hex) from the snapshot when the set is shared, `None`
    /// for an owner-only set; when present it is folded into the derived `ChannelId`
    /// echoed on `FolderActorsLoaded` so the row's `folder-member-remove-button`s
    /// can address the set. An owner-only (unshared) set returns
    /// `fauna.folders.not_shared`, surfaced as an EMPTY roster (never a page
    /// error). Mirrors [`Self::fetch_folder_members`] (the device roster).
    pub fn fetch_folder_actors(&self, name: &str, mls_group_id: Option<String>) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            // Derive the custody `ChannelId` (hex) from the raw group id so the
            // remove buttons can address the set. See `channel_id_from_group_id_hex`
            // for why this is NOT `hex32::decode` — that swallowed every real
            // group id via `.ok()`, leaving `channel_id: None` on every roster
            // re-fetch (found 2026-07-13).
            let channel_id = mls_group_id
                .as_deref()
                .and_then(|g| channel_id_from_group_id_hex(g).ok())
                .map(|cid| cid.to_string());
            let folders = fauna_client_folders::FoldersClient::new(nest_rpc);
            let msg = match folders.actor_members_list(name.clone()).await {
                Ok(reply) => UiMessage::Data(DataMessage::FolderActorsLoaded {
                    name,
                    members: reply.members,
                    channel_id,
                }),
                // An owner-only (unshared) set → empty roster, NOT an error
                // (`docs/goal/ui/folders.md` § Sharing, § Where logic lives).
                Err(NestClientError::Rpc(ref e)) if e.code == "fauna.folders.not_shared" => {
                    UiMessage::Data(DataMessage::FolderActorsLoaded {
                        name,
                        members: Vec::new(),
                        channel_id: None,
                    })
                }
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.folders.members.list_actors".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the per-set **device-activity** roster via `fauna.folders.devices`
    /// — the ordinary sync change signal (`label` + `change_count` per
    /// device), distinct from the enrolled-*member* roster
    /// ([`Self::fetch_folder_members`]) and the cross-user *actor* roster
    /// ([`Self::fetch_folder_actors`]). Lazy-loaded on first expand
    /// (`build_folder_row`'s expand handler) and re-fetched on every
    /// `fauna.sync.changed` push while the row stays expanded (`app.rs`'s
    /// `PushEvent::SyncChanged` arm, gated on
    /// `views::devices_folders::folders::expanded_folder_row_where` so a
    /// collapsed row is never wastefully re-fetched) — the remote-change nudge
    /// this exists to make e2e-pinnable
    /// (`docs/goal/behavior/file-sync.md` § Implementation status today).
    /// Sends `DataMessage::FolderDevicesLoaded`.
    pub fn fetch_folder_devices(&self, name: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            let folders = fauna_client_folders::FoldersClient::new(nest_rpc);
            let msg = match folders.devices(name.clone()).await {
                Ok(reply) => UiMessage::Data(DataMessage::FolderDevicesLoaded {
                    name,
                    devices: reply.devices,
                }),
                Err(e) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.folders.devices".into(),
                    error: e.to_string(),
                }),
            };
            tx.send(msg);
        });
    }

    /// Fetch the per-set **destination places** — this owner's enrolled backup
    /// destinations, each marked attached-or-not for `folder_id`
    /// (`fauna.backup.destination.list` joined with this box's
    /// `fauna.state.backup` display names —
    /// `docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage). Lazy-loaded on first expand, same shape as
    /// [`Self::fetch_folder_devices`]. Sends `DataMessage::FolderDestinationsLoaded`.
    pub fn fetch_folder_destinations(&self, name: &str, folder_id: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            let places = match bound_source_nest(&nest_rpc).await {
                Ok(source_nest) => fauna_client_config::list_folder_destinations(
                    nest_rpc,
                    crate::account_runtime::backup_seam().as_ref(),
                    source_nest,
                    folder_id,
                )
                .await
                .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let msg = match places {
                Ok(places) => UiMessage::Data(DataMessage::FolderDestinationsLoaded {
                    name,
                    folder_id,
                    places,
                }),
                Err(error) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.backup.destination.list".into(),
                    error,
                }),
            };
            tx.send(msg);
        });
    }

    /// Attach `folder_id` to an enrolled destination
    /// (`fauna_client_config::attach_folder_to_destination`), then repaint
    /// from the nest's answer — never an optimistic flip, the same posture
    /// [`Self::fetch_folder_destinations`]'s callers rely on. Mirrors
    /// [`Self::detach_folder_destination`].
    pub fn attach_folder_destination(&self, name: &str, folder_id: i64, destination_id: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        let destination_id = destination_id.to_string();
        self.spawn_bg(async move {
            let store = crate::account_runtime::backup_seam();
            let source_nest = match bound_source_nest(&nest_rpc).await {
                Ok(id) => id,
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.backup.destination.attach_folder".into(),
                        error,
                    }));
                    return;
                }
            };
            let msg = match fauna_client_config::attach_folder_to_destination(
                Arc::clone(&nest_rpc),
                store.as_ref(),
                source_nest,
                &destination_id,
                folder_id,
            )
            .await
            {
                Ok(_) => {
                    match fauna_client_config::list_folder_destinations(
                        nest_rpc,
                        store.as_ref(),
                        source_nest,
                        folder_id,
                    )
                    .await
                    {
                        Ok(places) => DataMessage::FolderDestinationsLoaded {
                            name,
                            folder_id,
                            places,
                        },
                        Err(_) => DataMessage::FolderDestinationsLoaded {
                            name,
                            folder_id,
                            places: Vec::new(),
                        },
                    }
                }
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.backup.destination.attach_folder".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            tx.send(UiMessage::Data(msg));
        });
    }

    /// Detach `folder_id` from `destination_id`
    /// (`fauna_client_config::detach_folder_from_destination`), then repaint
    /// from the nest's answer. `folder_set` is the attached row's own
    /// `__folder/<hex>/<id>` name, carried by the [`FolderDestinationPlace`]
    /// the detach button's row was built from — never re-derived here.
    pub fn detach_folder_destination(
        &self,
        name: &str,
        folder_id: i64,
        destination_id: &str,
        folder_set: &str,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        let destination_id = destination_id.to_string();
        let folder_set = folder_set.to_string();
        self.spawn_bg(async move {
            let store = crate::account_runtime::backup_seam();
            let source_nest = match bound_source_nest(&nest_rpc).await {
                Ok(id) => id,
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.backup.destination.detach_folder".into(),
                        error,
                    }));
                    return;
                }
            };
            let msg = match fauna_client_config::detach_folder_from_destination(
                Arc::clone(&nest_rpc),
                store.as_ref(),
                source_nest,
                &destination_id,
                folder_id,
                &folder_set,
            )
            .await
            {
                Ok(_) => {
                    match fauna_client_config::list_folder_destinations(
                        nest_rpc,
                        store.as_ref(),
                        source_nest,
                        folder_id,
                    )
                    .await
                    {
                        Ok(places) => DataMessage::FolderDestinationsLoaded {
                            name,
                            folder_id,
                            places,
                        },
                        Err(_) => DataMessage::FolderDestinationsLoaded {
                            name,
                            folder_id,
                            places: Vec::new(),
                        },
                    }
                }
                Err(e) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "fauna.backup.destination.detach_folder".into(),
                        error: e.to_string(),
                    }));
                    return;
                }
            };
            tx.send(UiMessage::Data(msg));
        });
    }

    /// Share a folder with one recipient (by bare handle) end-to-end
    /// ([`fauna_client_folders::orchestration::FoldersAuthor::share_set`]):
    /// resolve the handle to its actor, then run the share (fetch the member's
    /// KeyPackage → create the MLS group → `fauna.folders.share` bind → deliver
    /// the Welcome), reusing the conversations rail's shared per-actor `MlsEngine`.
    /// On success re-reads the actor roster (posting `FolderActorsLoaded`) so the
    /// new member appears. Same-nest only (a cross-nest share by `handle@domain` is
    /// a follow-on). Owner-only per `docs/goal/ui/folders.md` § Sharing.
    /// `access`: the invited member's grant from `folder-share-role-select` —
    /// `Some("writer")` for read-write, `None` = reader (multi-writer Phase 1).
    pub fn share_folder(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        name: &str,
        handle: &str,
        access: Option<String>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let name = name.to_string();
        let handle = handle.trim().to_string();
        self.spawn_bg(async move {
            let msg = match do_share_folder(&nest_rpc, &secret_hex, &name, &handle, access).await {
                Ok((members, channel_id)) => {
                    // The share MINTED the folder's MLS group, so the summary
                    // `folder-audience-select` reads (`fs.mls_group_id` is its
                    // `bound` input) is now stale in the machine — without this
                    // the rebuilt row keeps painting `private` on a bound
                    // folder. Refresh before posting the roster so both land
                    // together; tui's `Op::ShareFolder` fold is the ratified
                    // shape. Only on success: a failed share bound nothing.
                    machine.refresh().await;
                    UiMessage::Data(DataMessage::FolderActorsLoaded {
                        name,
                        members,
                        channel_id,
                    })
                }
                Err(error) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: crate::i18n::strings::devices::error_share_set(&error),
                }),
            };
            tx.send(msg);
        });
    }

    /// Remove one member from a shared folder
    /// ([`fauna_client_folders::orchestration::FoldersAuthor::remove_member`]) —
    /// evicts the roster row and **rotates the content key** for forward secrecy.
    /// `channel_id_hex` is the set's derived `ChannelId` (echoed on
    /// `FolderActorsLoaded`); `member_hex` is the member's actor id. On success
    /// re-reads the actor roster so the removed member disappears.
    pub fn remove_folder_member(&self, name: &str, channel_id_hex: &str, member_hex: &str) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let name = name.to_string();
        let channel_id_hex = channel_id_hex.to_string();
        let member_hex = member_hex.to_string();
        self.spawn_bg(async move {
            match do_remove_folder_member(
                &nest_rpc,
                &secret_hex,
                &name,
                &channel_id_hex,
                &member_hex,
            )
            .await
            {
                Ok(members) => {
                    // The removal rotated the set's M2 content key; the custody
                    // write is the account-state change the sync agent re-keys its
                    // engine on (`mls-group-key-material.md` § M2
                    // Rotate-on-removal). Refresh the roster so the removed member
                    // disappears.
                    tx.send(UiMessage::Data(DataMessage::FolderActorsLoaded {
                        name,
                        members,
                        channel_id: Some(channel_id_hex),
                    }));
                }
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: crate::i18n::strings::devices::error_remove_member(&error),
                    }));
                }
            }
        });
    }

    /// Set a shared-set member's access grant (`folder-member-role-select` /
    /// `folder-member-cap-input` — multi-writer Phase 1, `ui/folders.md`
    /// § Sharing): `access` is `"reader"`/`"writer"`, `byte_cap` `None` =
    /// uncapped. Owner-scoped + claimant-gated nest-side; never rotates. On
    /// success re-reads the roster so the row's select/cap/warning re-render
    /// from the nest's authoritative row.
    pub fn set_folder_member_access(
        &self,
        name: &str,
        channel_id_hex: Option<&str>,
        member_hex: &str,
        access: &str,
        byte_cap: Option<i64>,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        let channel_id = channel_id_hex.map(str::to_string);
        let member_hex = member_hex.to_string();
        let access = access.to_string();
        self.spawn_bg(async move {
            let folders = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_rpc));
            let result = folders
                .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
                    name: name.clone(),
                    actor_id: member_hex,
                    access,
                    byte_cap,
                    ..Default::default()
                })
                .await
                .map_err(|e| e.to_string());
            let msg = match result {
                Ok(_) => match read_folder_actors(&nest_rpc, &name).await {
                    Ok(members) => UiMessage::Data(DataMessage::FolderActorsLoaded {
                        name,
                        members,
                        // Echo the row's channel id so the re-render keeps the
                        // remove buttons addressable.
                        channel_id,
                    }),
                    Err(error) => UiMessage::Action(ActionResult::FailedLocalized {
                        message: crate::i18n::strings::devices::error_set_member_access(&error),
                    }),
                },
                Err(error) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: crate::i18n::strings::devices::error_set_member_access(&error),
                }),
            };
            tx.send(msg);
        });
    }

    /// Flip a folder's WebDAV serve flag (`folder-webdav-toggle`, every
    /// owner row) through the shared [`fauna_client_folders::orchestration::FoldersAuthor::serve_set`]
    /// — the same composition the FFI (`folders_serve_set`) / wasm
    /// (`foldersServeSet`) faces call: content-key genesis/rotation + the nest
    /// `webdav_enabled` flag + the MSEK-sealed `WebdavKeysBlob` re-provision
    /// (`webdav-server.md` § Independent enablement point 2). `mls_group_id` is
    /// the set's raw group id (hex) from the snapshot, `None` for an unshared
    /// set. On success refresh `machine` so the row's `webdav_enabled` reflects
    /// the persisted state. A locally bound engine re-keys on its own: the serve
    /// flip rotates/migrates content-key custody, and the sync agent re-resolves
    /// on that write's nudge and on the row's moved serve flag.
    ///
    /// Gate the toggle on [`can_serve_webdav`](Self::can_serve_webdav): this
    /// flips the nest flag *before* re-provisioning the blob, so an enable by an
    /// MSEK-less actor commits the flag and only then fails `NoMsek`.
    pub fn serve_set_folder(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        name: &str,
        mls_group_id: Option<String>,
        enable: bool,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            match do_serve_set_folder(&nest_rpc, &secret_hex, &name, mls_group_id, enable).await {
                Ok(()) => {
                    machine.refresh().await;
                }
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::Failed {
                        context: "serve_webdav".into(),
                        error,
                    }));
                }
            }
        });
    }

    /// Paywall a website-enabled folder to a subscription tier
    /// (`folder-paywall-tier-select`, website-enabled rows only) through the shared
    /// [`fauna_client_folders::orchestration::FoldersAuthor::paywall_set`] — the
    /// same composition the FFI (`folders_paywall_set`) / wasm
    /// (`foldersPaywallSet`) faces call: content-key genesis/re-seal + the nest
    /// `web_paywall_tier` flag + a `content.read{folder:set}` grant minted to the
    /// nest's web-serve holder (`monetization.md` § Pillar 2, the folder half).
    /// `mls_group_id` is the set's raw group id (hex) from the snapshot, `None`
    /// for an owner-only set. The holder is discovered via
    /// `fauna.bridges.fetch_bridge_pubkey` (role `content-processor`, id
    /// `web-serve`) — the nest self-enrolls it at boot. On success refresh
    /// `machine` so the row's `web_paywall_tier` reflects the persisted state (a
    /// locally bound engine re-keys on the custody write's nudge, as for serve).
    ///
    /// v1 is set-only — the client offers no clear affordance, so `tier` is always
    /// a real tier name (the placeholder is not a selectable write).
    pub fn paywall_set_folder(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        name: &str,
        mls_group_id: Option<String>,
        tier: &str,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        let tx = self.tx.clone();
        let name = name.to_string();
        let tier = tier.to_string();
        self.spawn_bg(async move {
            match do_paywall_set_folder(&nest_rpc, &secret_hex, &name, mls_group_id, &tier).await {
                Ok(()) => {
                    machine.refresh().await;
                }
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: crate::i18n::strings::devices::error_paywall_set(&error),
                    }));
                }
            }
        });
    }

    /// Set a folder's **audience** (`folder-audience-select`, phase 4 slice 4d —
    /// `ui/folders.md` § Audience and website serving) through the keyless
    /// [`fauna_client_folders::FoldersClient::set_audience`], the same face tui
    /// calls crate-direct and the FFI/wasm apps reach as
    /// `FfiFoldersClient::set_audience` / `setFolderAudience`.
    ///
    /// **Keyless for every direction the picker offers**, the bound `→shared`
    /// flip-back included: all are plain `fauna.folders.update` writes. Nothing
    /// here touches a content key, because the back-catalogue is moved by each
    /// device's own engine at its next catch-up or rescan tick off the
    /// *projected* audience (`SyncEngine::converge_corpus_to_audience`, which
    /// dispatches the sealed direction on bound-ness) — and the projection
    /// reaches every member's engine, which a per-actor custody sentinel never
    /// could. So this is NOT the `serve_set_folder` / `paywall_set_folder` shape
    /// beside it: no `FoldersAuthor`, no secret, no rotation signal.
    ///
    /// `public` must arrive here only from an ANSWERED
    /// `folder-audience-public-confirm` — a public folder rests unsealed,
    /// content and names/paths alike (`principles.md` § The user always controls
    /// their data owns that one exception). On success, refresh `machine` so the
    /// row repaints from the nest's authoritative `audience`. The nest's refusal
    /// text is the actionable half of any failure (it names the repair — "turn
    /// off WebDAV serving first" and kin), so it travels whole into the banner.
    pub fn set_folder_audience(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        name: &str,
        audience: &str,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        let audience = audience.to_string();
        // The owner's identity key: a `→public` flip is the confirm's landing
        // point, so `set_audience` mints the owner's signed attestation under
        // it (`encryption-at-rest.md` § Readable classes → *The
        // declassification is owner-ATTESTED*); a seat unseals only on that
        // signature. `FaunaClient::new` already refused a malformed secret, so
        // this derivation cannot fail on a live client.
        let attestor = Arc::new(
            secret_to_keypair(&self.secret_hex)
                .expect("set_folder_audience: secret_hex was validated at FaunaClient::new"),
        );
        self.spawn_bg(async move {
            let result = fauna_client_folders::FoldersClient::new(nest_rpc)
                .with_audience_attestor(attestor)
                .set_audience(&name, &audience)
                .await
                .map_err(|e| e.to_string());
            match result {
                Ok(_) => machine.refresh().await,
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: crate::i18n::strings::devices::error_set_audience(&error),
                    }));
                }
            }
        });
    }

    /// Flip a folder's **website serving** (`folder-website-toggle`, phase 4
    /// slice 4d) through the keyless
    /// [`fauna_client_folders::FoldersClient::set_website_enabled`].
    ///
    /// The door to a website folder: phase 2 slice e retired the wizard's mode
    /// step, so between then and this slice there was no way to create one at
    /// all (`ui/folders.md` § Implementation status today records that accepted
    /// gap, and explicitly rules out patching it by re-adding a mode control).
    /// Keyless for the same reason as [`set_folder_audience`](Self::set_folder_audience)
    /// — a plain `fauna.folders.update`.
    ///
    /// Orthogonal to the audience by design: the flag publishes the folder's
    /// *head*, the audience decides who may *read* it. So the toggle is offered
    /// — and works — on a folder nobody can read yet, which is why the row hints
    /// rather than disabling.
    pub fn set_folder_website(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        name: &str,
        enable: bool,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        let name = name.to_string();
        self.spawn_bg(async move {
            let result = fauna_client_folders::FoldersClient::new(nest_rpc)
                .set_website_enabled(&name, enable)
                .await
                .map_err(|e| e.to_string());
            match result {
                Ok(_) => machine.refresh().await,
                Err(error) => {
                    tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                        message: crate::i18n::strings::devices::error_serve_website(&error),
                    }));
                }
            }
        });
    }

    /// The creator's own subscription tier names (ascending by rank) — the option
    /// set the `folder-paywall-tier-select` offers. Returns a `Send`
    /// future the Folders view drives off the GTK thread and caches, mirroring
    /// [`can_serve_webdav`](Self::can_serve_webdav). A failed read (or no tiers)
    /// degrades to an empty list — the select renders disabled with a "create a
    /// tier first" hint, since there is nothing to paywall to.
    pub fn fetch_own_tier_names(
        &self,
    ) -> impl std::future::Future<Output = Vec<String>> + Send + 'static {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        async move {
            let subs = fauna_client_subscriptions::SubscriptionsClient::new(nest_rpc);
            match subs.tiers_list().await {
                Ok(tiers) => tiers.into_iter().map(|t| t.name).collect(),
                Err(_) => Vec::new(),
            }
        }
    }

    /// Whether this actor can serve a set over WebDAV — the capability gating
    /// every owner row's `folder-webdav-toggle`. Returns a `Send` future so
    /// the Folders view can drive it off the GTK thread through
    /// `async_helper::spawn_with_snapshot` and cache the answer.
    ///
    /// A failed read degrades to `false` — the toggle stays disabled + hinted.
    /// Not offering a control is the safe failure; offering one that cannot
    /// succeed is not (`serve_set_folder` commits the nest flag before it can
    /// discover the missing MSEK).
    pub fn can_serve_webdav(&self) -> impl std::future::Future<Output = bool> + Send + 'static {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.clone();
        async move {
            do_can_serve_webdav(&nest_rpc, &secret_hex)
                .await
                .unwrap_or(false)
        }
    }

    /// Fetch the recipient-side **pending folder shares** — the staged
    /// ("knocked") cross-user shares a stranger sent that the contact-gate left
    /// un-acked (`docs/goal/ui/folders.md` § Sharing — Recipient side). A
    /// **peek** (never acks); posts `DataMessage::FolderPendingSharesLoaded` for
    /// the page-level "Shared with you" section. Reuses the shared
    /// [`fauna_client_inbox::list_folder_pending_shares`] (native + wasm, one
    /// source), the crate-direct mirror of the `fauna-ffi`
    /// `folders_pending_shares` recipe.
    pub fn fetch_folder_pending_shares(&self) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match do_list_pending_shares(&nest_rpc).await {
                Ok(shares) => UiMessage::Data(DataMessage::FolderPendingSharesLoaded { shares }),
                Err(error) => UiMessage::Action(ActionResult::Failed {
                    context: "fauna.folders.pending_shares".into(),
                    error,
                }),
            };
            tx.send(msg);
        });
    }

    /// Accept a staged folder share (`folder-share-accept-button`): join the MLS
    /// group **off the chat rail** then ack the durable row (accept bypasses the
    /// contact gate — the user explicitly accepted). On success re-fetches the
    /// pending list so the row disappears. Crash-safe (join before ack; both
    /// idempotent). Mirrors the `fauna-ffi` `folders_accept_share` recipe.
    pub fn accept_folder_share(&self, inbox_id: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match do_accept_share(&nest_rpc, inbox_id).await {
                Ok(shares) => UiMessage::Data(DataMessage::FolderPendingSharesLoaded { shares }),
                Err(error) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: crate::i18n::strings::devices::error_accept_share(&error),
                }),
            };
            tx.send(msg);
        });
    }

    /// Decline a staged folder share (`folder-share-decline-button`): a bare ack
    /// of the durable row — the Welcome is dropped **unprocessed**, so declining
    /// never joins the group. On success re-fetches the pending list. Mirrors the
    /// `fauna-ffi` `folders_decline_share` recipe.
    pub fn decline_folder_share(&self, inbox_id: i64) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match do_decline_share(&nest_rpc, inbox_id).await {
                Ok(shares) => UiMessage::Data(DataMessage::FolderPendingSharesLoaded { shares }),
                Err(error) => UiMessage::Action(ActionResult::FailedLocalized {
                    message: crate::i18n::strings::devices::error_decline_share(&error),
                }),
            };
            tx.send(msg);
        });
    }

    // ── The co-present ceremony (`p2p-share`; row 334) ──────────────────────

    /// Fetch the co-present ceremony's group-share surface — pending
    /// consent-card invitations AND the shared sets this device can actually
    /// read (`crate::offline_share::load_group_shares`, `p2p.md` § Offline
    /// share initiation). A **peek** (never acks); posts
    /// `DataMessage::GroupSharesLoaded`. Mirrors [`Self::fetch_folder_pending_shares`]
    /// exactly — the same page-level-read shape, called once at sign-in and,
    /// through [`Self::fetch_knock_lists`], on every edge the Folders page
    /// becomes visible on.
    #[cfg(feature = "p2p-share")]
    pub fn fetch_group_shares(&self) {
        let secret_hex = self.secret_hex.as_str().to_string();
        let tx = self.tx.clone();
        // Read out HERE, on the GTK thread: the slot lives in the window's own
        // panel state, and its bound seat's replica answers for a frame
        // ingested before the account runtime lent the record.
        let session_seat = crate::offline_share::session_seat();
        self.spawn_bg(async move {
            let Ok(keypair) = ActorKeypair::from_secret_hex(&secret_hex) else {
                return;
            };
            // The record rests on the account store, which answers with the
            // nest unreachable — the co-present ceremony's whole case.
            let account = crate::account_runtime::handle();
            let seat = session_seat.as_ref().and_then(|s| s.seat());
            let views = crate::offline_share::load_group_shares(
                account,
                seat.as_deref(),
                &keypair.actor_id(),
            )
            .await;
            tx.send(UiMessage::Data(DataMessage::GroupSharesLoaded { views }));
        });
    }

    /// Re-read BOTH lists the Folders page's "Shared with you" section shows as
    /// one `folder-pending-share` family: the folder-share knocks and the
    /// co-present ceremony's consent cards (`p2p.md` § Offline share initiation
    /// → *Built — the affordance, both roles*). Neither is pushed, so every edge
    /// the page becomes visible on calls this rather than either fetch alone —
    /// a consent card that lands after sign-in otherwise never paints.
    pub fn fetch_knock_lists(&self) {
        self.fetch_folder_pending_shares();
        #[cfg(feature = "p2p-share")]
        self.fetch_group_shares();
    }

    /// `offline-share-button` / `offline-receive-button` — this session's
    /// seat for the panel: handed straight back when either door already
    /// bound it (`offline_share::SessionSeat` — one actor-keyed endpoint per
    /// session), else rule 7's brake read, the ceremony record loaded if the
    /// account runtime has lent it, and the actor-keyed ceremony listener
    /// brought up. Mirrors tui's `Op::BindOfflineShareSeat`.
    #[cfg(feature = "p2p-share")]
    pub fn bind_offline_share_seat(&self, session_seat: crate::offline_share::SessionSeat) {
        let nest = Arc::clone(&self.nest_rpc);
        let secret_hex = self.secret_hex.as_str().to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match ActorKeypair::from_secret_hex(&secret_hex) {
                Ok(_) => {
                    match crate::offline_share::bind_seat(&session_seat, nest, secret_hex).await {
                        Ok(_seat) => UiMessage::Data(DataMessage::OfflineShareSeatBound),
                        Err(error) => {
                            // A refused bind is the brake doing its job, not a
                            // failure to hide.
                            tracing::debug!("[offline-share] seat not bound: {error}");
                            UiMessage::Data(DataMessage::OfflineShareFailed { message: error })
                        }
                    }
                }
                Err(e) => UiMessage::Data(DataMessage::OfflineShareFailed {
                    message: format!("invalid secret: {e}"),
                }),
            };
            tx.send(msg);
        });
    }

    /// `offline-share-begin-button` — the initiator's whole walk (mint,
    /// offer, consent poll, deliver), then the record flush. Mirrors tui's
    /// `Op::BeginOfflineShare`.
    #[cfg(feature = "p2p-share")]
    pub fn begin_offline_share(
        &self,
        seat: Arc<crate::offline_share::CeremonySeat>,
        peer: fauna_client_capabilities::group_ceremony_view::PeerCode,
    ) {
        let secret_hex = self.secret_hex.as_str().to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let Ok(keypair) = ActorKeypair::from_secret_hex(&secret_hex) else {
                tx.send(UiMessage::Data(DataMessage::OfflineShareFailed {
                    message: "invalid secret".to_string(),
                }));
                return;
            };
            let account = crate::account_runtime::handle();
            let msg = match crate::offline_share::initiate(seat, account, keypair, peer).await {
                Ok(status) => UiMessage::Data(DataMessage::OfflineShareProgressed { status }),
                Err(error) => {
                    tracing::debug!("[offline-share] begin failed: {error}");
                    UiMessage::Data(DataMessage::OfflineShareFailed { message: error })
                }
            };
            tx.send(msg);
        });
    }

    /// `offline-receive-expect-button` — the receive act: admit exactly this
    /// initiator's ceremony frames for the expectation's TTL. **Synchronous**
    /// — minting the expectation is an in-memory write on the live seat, so
    /// it is true the instant the user says so (never a spawn).
    #[cfg(feature = "p2p-share")]
    pub fn expect_offline_share(
        &self,
        seat: &crate::offline_share::CeremonySeat,
        initiator: ActorId,
    ) {
        crate::offline_share::expect_from(seat, initiator);
    }

    /// `offline-share-cancel-button` on the recipient side (rule 6) —
    /// withdraw the receive act. **Synchronous**, the mirror of
    /// [`Self::expect_offline_share`].
    #[cfg(feature = "p2p-share")]
    pub fn cancel_offline_share_expectation(
        &self,
        seat: &crate::offline_share::CeremonySeat,
        initiator: &ActorId,
    ) {
        crate::offline_share::cancel_expectation(seat, initiator);
    }

    /// The consent card's Accept (`folder-share-accept-button`, ceremony
    /// arm): mint + rest the reception keypair, record the accept, wait out
    /// the delivery, admit, and write the machinery through. Needs the
    /// SEAT — the ceremony record lives on it, shared with the listener that
    /// ingested the offer — so a card whose panel never opened this session
    /// carries no seat to act through (`build_group_invitation_row`'s own
    /// caller withholds the click in that state). Mirrors tui's
    /// `Op::AcceptGroupShare`.
    #[cfg(feature = "p2p-share")]
    pub fn consent_group_share(
        &self,
        seat: Arc<crate::offline_share::CeremonySeat>,
        scope_id: [u8; 32],
    ) {
        let secret_hex = self.secret_hex.as_str().to_string();
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let Ok(keypair) = ActorKeypair::from_secret_hex(&secret_hex) else {
                tx.send(UiMessage::Data(DataMessage::OfflineShareFailed {
                    message: "invalid secret".to_string(),
                }));
                return;
            };
            let account = crate::account_runtime::handle();
            let msg = match crate::offline_share::consent(seat, account, keypair, scope_id).await {
                Ok(status) => UiMessage::Data(DataMessage::OfflineShareProgressed { status }),
                Err(error) => {
                    tracing::debug!("[offline-share] group consent failed: {error}");
                    UiMessage::Data(DataMessage::OfflineShareFailed { message: error })
                }
            };
            tx.send(msg);
        });
    }

    /// The consent card's Decline (`folder-share-decline-button`, ceremony
    /// arm): the monotone decline plus its flush — same seat requirement as
    /// [`Self::consent_group_share`]. Mirrors tui's `Op::DeclineGroupShare`.
    #[cfg(feature = "p2p-share")]
    pub fn decline_group_share(
        &self,
        seat: Arc<crate::offline_share::CeremonySeat>,
        scope_id: [u8; 32],
    ) {
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            let msg = match crate::offline_share::decline(seat, scope_id).await {
                // The card is gone from the projection the moment the record
                // says declined; Idle is the honest reading of "nothing in
                // flight here any more".
                Ok(()) => UiMessage::Data(DataMessage::OfflineShareProgressed {
                    status: fauna_client_capabilities::group_ceremony_view::CeremonyStatus::Idle,
                }),
                Err(error) => {
                    tracing::debug!("[offline-share] group decline failed: {error}");
                    UiMessage::Data(DataMessage::OfflineShareFailed { message: error })
                }
            };
            tx.send(msg);
        });
    }

    /// **Eager bind-time write verify for a CROSS-NEST set**.
    ///
    /// The `access` a foreign row renders from is **advisory** — it is refreshed
    /// on a poll, so it can be stale in either direction, and the design forbids
    /// treating it as authorization. Before a folder is actually bound, then, the
    /// client asks the authority: one `fauna.folders.write_token.get`, which
    /// this nest relays to the set's home nest, where `require_foreign_writer`
    /// answers from the roster row it wrote itself. A refusal fails the bind **at
    /// the gesture**, loudly — a stale-writer *row* is survivable, a stale-writer
    /// *binding* is not: it would leave a folder the user believes is syncing
    /// whose every edit the home nest refuses, breaching `file-sync.md`'s iron
    /// rule exactly as binding a reader would.
    ///
    /// Zero new wire — the mint the engine needs anyway *is* the writer oracle.
    /// Delivers `Ok(())` / `Err(detail)` on the GTK main thread.
    pub fn verify_foreign_write_access<F>(
        &self,
        home_nest_url: String,
        channel_id_hex: String,
        on_done: F,
    ) where
        F: FnOnce(Result<(), String>) + 'static,
    {
        // Guarded like `spawn_bg`: this fires from a GTK signal closure, a few of
        // which the window widget-tree cycle keeps alive past sign-out — spawning
        // on the torn-down runtime's handle would panic. No runtime ⇒ no verify
        // ⇒ no bind, which is the fail-closed direction anyway.
        // Answer synchronously rather than returning: the caller holds its form
        // inert until `on_done` fires, so a silent return would wedge the add
        // button rather than merely refusing the bind.
        let Some(rt) = self.runtime.borrow().as_ref().map(|r| r.handle().clone()) else {
            on_done(Err("client is shutting down".to_string()));
            return;
        };
        let nest_rpc = Arc::clone(&self.nest_rpc);
        crate::async_helper::spawn_with_snapshot(
            &rt,
            move || async move {
                fauna_client_folders::FoldersClient::new(nest_rpc)
                    .write_token_get(home_nest_url, channel_id_hex)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            on_done,
        );
    }

    /// Leave a folder shared *with* this client (`folder-leave-button` on the
    /// read-only shared-with-me `folder-row`): the recipient counterpart to the
    /// owner-side member remove, **self-scoped** — it drops only this client's own
    /// roster row (no `ownerSecret`, no content-key rotation; a voluntary leaver
    /// keeps the generations they held — `mls-group-key-material.md` § M2). Two
    /// idempotent halves, **nest roster-drop first** (`do_leave_folder_share`).
    /// On success refreshes the `DevicesMachine` so the row disappears from the
    /// folders list (`list_owned_and_shared` no longer unions the now-off-roster
    /// set). Crate-direct mirror of the `fauna-ffi` `folders_leave` recipe.
    pub fn leave_folder_share(
        &self,
        machine: Arc<fauna_devices_machine::DevicesMachine>,
        group_id: String,
    ) {
        let nest_rpc = Arc::clone(&self.nest_rpc);
        let tx = self.tx.clone();
        self.spawn_bg(async move {
            match do_leave_folder_share(&nest_rpc, group_id).await {
                // Off the roster now — refresh so the read-only shared-with-me row
                // drops out of the list on the next observer tick.
                Ok(()) => machine.refresh().await,
                Err(error) => tx.send(UiMessage::Action(ActionResult::FailedLocalized {
                    message: crate::i18n::strings::devices::error_leave_share(&error),
                })),
            }
        });
    }
}

#[cfg(test)]
mod predecessor_backup_keys_cache_tests {
    //! `label_custody()` and `sync_agent::install()` used
    //! to run two independent `AccountRegistry::predecessor_backup_keys` walks
    //! that could observe different registry states after predecessor material
    //! arrived mid-session. `predecessor_backup_keys()` now resolves once and
    //! caches — this pins the caching mechanism directly (a shape pin, not a
    //! behavioural one: an offline test fixture's registry is always empty, so
    //! nothing here can make a genuine registry walk return a non-empty list to
    //! observe a live-vs-cached DIVERGENCE end to end).
    use super::*;

    /// The cache starts empty and is populated by the first call.
    #[test]
    fn first_call_populates_the_cache() {
        let client = FaunaClient::offline_for_test();
        assert!(
            client.predecessor_backup_keys_cache.borrow().is_none(),
            "cache must start empty"
        );
        let _ = client.predecessor_backup_keys();
        assert!(
            client.predecessor_backup_keys_cache.borrow().is_some(),
            "the first call must populate the cache"
        );
    }

    /// Once cached, a call reads the cache rather than re-deriving — pinned by
    /// seeding a SENTINEL value directly (bypassing a real registry walk,
    /// which an offline fixture can't make non-empty) and asserting it comes
    /// back unchanged. A regression that reverted to a live re-walk would
    /// return the empty registry result instead and fail this assertion.
    #[test]
    fn a_cached_value_is_returned_verbatim_not_rederived() {
        let client = FaunaClient::offline_for_test();
        let sentinel = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        *client.predecessor_backup_keys_cache.borrow_mut() = Some(vec![sentinel.clone()]);
        let read_back = client.predecessor_backup_keys();
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].to_bytes(), sentinel.to_bytes());
    }

    /// `label_custody()` must read the same cache `predecessor_backup_keys()`
    /// does, not a second independent resolve — the exact two-walk shape the
    /// finding named. Seeding the cache and reading it back through
    /// `label_custody()`'s own output (`predecessor_count()`) proves the two
    /// call sites share one value rather than each deriving their own.
    #[test]
    fn label_custody_reads_the_same_cache() {
        let client = FaunaClient::offline_for_test();
        let sentinel = fauna_core::crypto::BackupKey::from_bytes([9u8; 32]);
        *client.predecessor_backup_keys_cache.borrow_mut() = Some(vec![sentinel]);
        assert_eq!(client.label_custody().predecessor_count(), 1);
    }
}

// ---------------------------------------------------------------------------
// Async helpers
// ---------------------------------------------------------------------------

/// Map one `fauna.family.status` read outcome onto the UI message — or onto
/// **no message** for a failed read. This split is the load-bearing decision of
/// family-safety.md § Content policy's unfetched-policy ruling (clause 1): a
/// failed read yields no information, so the handler must never see it wearing
/// the successful-unsupervised shape (an all-`None` `FamilyStatusLoaded`),
/// which would clear a loaded content floor / screen-time policy. A successful
/// read that really reports unsupervised still produces exactly that all-`None`
/// message — that one legitimately clears.
fn family_status_loaded<E: std::fmt::Display>(
    reply: Result<fauna_protocol::family::FamilyStatusReply, E>,
) -> Option<DataMessage> {
    match reply {
        Ok(reply) => {
            // Folded before the literal below moves `supervised_by` out of the
            // reply — the shared derivation reads the whole reply, so it has to
            // run while the reply is still whole.
            let snapshot = fauna_client_family::SupervisionSnapshot::from_status(&reply);
            Some(DataMessage::FamilyStatusLoaded {
                supervised_by: reply.supervised_by.map(|g| g.handle),
                ward_count: reply.wards.len(),
                incoming_transfer_count: reply.incoming_transfers.len(),
                // The three client-enforced pillars, folded by the SHARED
                // derivation (family-safety.md § Content policy, the
                // unfetched-policy ruling): the guardian content floor, the
                // Guardian Notify knob, and the screen-time policy.
                //
                // ⚠ This replaced three hand-read `reply.policy.and_then(...)`
                // fields. Those were gated on the policy DOCUMENT being present,
                // not on a guardianship existing — i.e. linux trusted the nest
                // never to send a policy to an unsupervised caller. The shared fold
                // keys every pillar on `supervised_by` instead, so a graduated
                // ward's client cannot keep enforcing a floor it no longer has.
                snapshot,
                // …and the ward's OWN usage total for their local day (§ Screen
                // time), which seeds the heartbeat and is what their read-only
                // summary shows. `None` = no daily budget, no accounting at all.
                usage_today_minutes: reply.usage_today_minutes,
                // The ward's own asks — the handler gates them on
                // `supervised_by` when folding them into `crate::ward_asks`.
                contact_requests: reply.contact_requests,
                feed_requests: reply.feed_requests,
            })
        }
        Err(e) => {
            tracing::info!(
                "fauna.family.status failed (family surfaces stay as they are; \
                 enforcement state unchanged): {e}"
            );
            None
        }
    }
}

#[cfg(test)]
mod family_status_loaded_tests {
    use super::*;
    use fauna_protocol::family::{FamilyGuardianInfo, FamilyStatusReply, ReachPolicy};

    /// The probe's linux arm, producer half: a failed read maps to NO
    /// message, so nothing downstream can mistake it for "read says
    /// unsupervised" and clear a loaded floor.
    #[test]
    fn a_failed_read_produces_no_message() {
        assert!(family_status_loaded(Err("nest unreachable")).is_none());
    }

    /// The one outcome that legitimately clears: a successful read reporting
    /// no supervision produces the all-`None` message.
    #[test]
    fn a_successful_unsupervised_read_produces_the_clearing_message() {
        let msg = family_status_loaded::<&str>(Ok(FamilyStatusReply::default()))
            .expect("success always produces a message");
        match msg {
            DataMessage::FamilyStatusLoaded {
                supervised_by,
                snapshot,
                ..
            } => {
                assert_eq!(supervised_by, None);
                assert_eq!(snapshot.content_policy, None);
                assert_eq!(snapshot.screen_time, None);
                assert!(!snapshot.is_supervised());
            }
            other => panic!("wrong message: {other:?}"),
        }
    }

    /// A supervised reply's floor survives the mapping intact.
    #[test]
    fn a_supervised_read_carries_the_floor() {
        let reply = FamilyStatusReply {
            supervised_by: Some(FamilyGuardianInfo {
                handle: "guardian".into(),
                ..Default::default()
            }),
            policy: Some(ReachPolicy {
                content_policy: Some(fauna_core::obligation::ContentPolicy {
                    nsfw: fauna_core::obligation::ContentFloor::Block,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let msg = family_status_loaded::<&str>(Ok(reply)).expect("success produces a message");
        match msg {
            DataMessage::FamilyStatusLoaded {
                supervised_by,
                snapshot,
                ..
            } => {
                assert_eq!(supervised_by.as_deref(), Some("guardian"));
                assert_eq!(
                    snapshot.content_policy.map(|p| p.nsfw),
                    Some(fauna_core::obligation::ContentFloor::Block)
                );
            }
            other => panic!("wrong message: {other:?}"),
        }
    }

    /// The graduation gate, which linux did NOT have before adopting the shared
    /// fold: a reply that still carries a policy document but names no guardian
    /// must yield nothing enforceable.
    ///
    /// linux previously read `content_policy` / `content_notify` / `screen_time`
    /// straight off `status.policy`, so this reply would have handed a floor,
    /// the Notify knob and a bedtime lock to an account with no guardian at all
    /// — the client trusting the nest never to send that combination rather than
    /// refusing it. `SupervisionSnapshot::from_status` keys every pillar on
    /// `supervised_by`, so the refusal is now structural and shared with the
    /// other apps instead of being one server's promise.
    #[test]
    fn a_policy_without_a_guardian_carries_nothing_enforceable() {
        let reply = FamilyStatusReply {
            supervised_by: None,
            policy: Some(ReachPolicy {
                content_policy: Some(fauna_core::obligation::ContentPolicy {
                    nsfw: fauna_core::obligation::ContentFloor::Block,
                    ..Default::default()
                }),
                content_notify: Some(true),
                screen_time: Some(fauna_core::screen_time::ScreenTimePolicy {
                    window_start: Some(1260),
                    window_end: Some(420),
                    daily_minutes: Some(90),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let msg = family_status_loaded::<&str>(Ok(reply)).expect("success produces a message");
        match msg {
            DataMessage::FamilyStatusLoaded {
                supervised_by,
                snapshot,
                ..
            } => {
                assert_eq!(supervised_by, None);
                assert!(!snapshot.is_supervised());
                assert_eq!(snapshot.content_policy, None, "no guardian, no floor");
                assert!(!snapshot.content_notify, "no guardian, no notify counting");
                assert_eq!(snapshot.screen_time, None, "no guardian, no bedtime lock");
            }
            other => panic!("wrong message: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Onboarding helpers (5-stage redesign)
// ---------------------------------------------------------------------------
//
// These sign the request bodies expected by the new onboarding endpoints in
// bins/fauna-nest/src/{challenge_auth,invite_requests,registration}.rs. Each
// signature covers a different byte layout — don't reuse build_signed_auth_body
// (which signs actor_id || timestamp_be) for these.

fn secret_to_keypair(secret_hex: &str) -> Result<ActorKeypair, anyhow::Error> {
    ActorKeypair::from_secret_hex(secret_hex).map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Custody-hosting registry (`admin-custody-hosting`)
// ---------------------------------------------------------------------------

/// The `admin-custody-hosting` read wrapper: linux's call sites pass
/// `&Arc<NestClient>` (borrowed, reused across the remove-then-reread
/// sequence below), while the shared
/// `fauna_client_capabilities::custody_hosting::load_admin_hosting_snapshot`
/// takes its transport by value.
async fn load_custody_hosting_snapshot(
    nest: &Arc<NestClient>,
    error: Option<String>,
) -> crate::views::admin::AdminHostingSnapshot {
    fauna_client_capabilities::custody_hosting::load_admin_hosting_snapshot(nest.clone(), error)
        .await
}

/// One Contacts Find User resolve as the page's message: the shared
/// [`fauna_client_core::find_user::find_user_by_handle`] (so the result names
/// the typed `handle@domain` whenever a qualifier was typed — the dial names the
/// peer, `foreign-handle-resolution.md` § Peer-auth model), serialized as the
/// `{ actor_id, handle, domain }` the `HandleResolved` consumer reads. A
/// not-found / unreachable resolve is `ActionResult::Failed` (the contacts error
/// element), never a raw body.
async fn find_user_message<R: fauna_protocol::RpcRequester>(
    rpc: &R,
    handle: &str,
    domain: Option<&str>,
) -> UiMessage {
    match fauna_client_core::find_user::find_user_by_handle(rpc, handle, domain).await {
        Ok(found) => UiMessage::Data(DataMessage::HandleResolved {
            result: serde_json::to_value(&found).unwrap_or(serde_json::Value::Null),
        }),
        Err(e) => UiMessage::Action(ActionResult::Failed {
            context: "fauna.actor.by_handle".into(),
            error: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod find_user_message_tests {
    use super::*;

    /// A peer that answers `by_handle` with an identity of its own choosing.
    struct EchoingPeer;

    impl fauna_protocol::RpcRequester for EchoingPeer {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Ok(serde_json::from_value(serde_json::json!({
                "actor_id": "ab".repeat(32),
                "handle": "alice",
                "domain": "trusted.test",
                "addresses": [],
                "addressable": true,
            }))
            .unwrap())
        }
    }

    /// The find row linux paints reads `result.handle@result.domain`,
    /// so a foreign peer echoing a domain it was not dialed at must not reach it.
    #[tokio::test]
    async fn a_foreign_peers_echo_never_names_the_find_result() {
        let msg = find_user_message(&EchoingPeer, "bob", Some("attacker.test")).await;
        let UiMessage::Data(DataMessage::HandleResolved { result }) = msg else {
            panic!("expected HandleResolved");
        };
        assert_eq!(result["handle"], "bob");
        assert_eq!(result["domain"], "attacker.test");
        assert_eq!(result["actor_id"], "ab".repeat(32));
    }
}

// ---------------------------------------------------------------------------
// Shared-folder (cross-user) owner-side helpers — folders.md § Sharing
// ---------------------------------------------------------------------------

/// The issuer key set's read (`fauna.oauth.issuer_key_status`), folded by the
/// shared `AdminClient::issuer_key_status_view` door — and non-fatal by
/// construction: a failure (any read error, e.g. a
/// transient transport fault) becomes the `admin-nest-oauth-*` section's own worded
/// reason line, never a page error, so the rest of admin-nest still paints.
/// Mirrors tui's `read_oauth_keys` (`apps/fauna-tui/src/admin/mod.rs`).
async fn read_oauth_issuer_keys(nest_rpc: Arc<NestClient>) -> crate::views::admin::OauthKeysRead {
    match fauna_client_admin::AdminClient::new(nest_rpc)
        .issuer_key_status_view()
        .await
    {
        Ok(view) => crate::views::admin::OauthKeysRead::Ready(view),
        Err(e) => crate::views::admin::OauthKeysRead::Failed(
            crate::i18n::strings::admin::nest_page::oauth_keys_error(&e.to_string()),
        ),
    }
}

/// The linux instantiation of the owner-side shared-folder author, over the
/// shared `fauna_client_folders::build_folders_author` recipe every native app
/// (and `fauna-ffi`) now calls: the WS-RPC `FoldersClient`, the owner's
/// account-plane content-key custody, and the conversations rail's shared
/// per-actor `MlsEngine` as the group-crypto seam.
type FoldersAuthorNative = fauna_client_folders::orchestration::FoldersAuthor<
    Arc<NestClient>,
    Arc<fauna_mls::engine::MlsEngine>,
>;

/// Build the shared-folder author + the thin `ConversationsClient` its
/// `share_set` reuses for KeyPackage/Welcome transport. Reuses the ONE live
/// per-actor `MlsEngine` (`ConversationsSession::engine`, over the single
/// `mls_state.db`) — never a second engine racing the SQLite file. Errors when
/// the conversations session isn't live yet (MLS not ready).
fn build_folders_author(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
) -> Result<(FoldersAuthorNative, ConversationsClient<Arc<NestClient>>), String> {
    let session = crate::conversations::conv_backend::active_session()
        .ok_or_else(|| "conversations session not ready".to_string())?;
    let keypair = ActorKeypair::from_secret_hex(secret_hex).map_err(|e| e.to_string())?;
    let author = fauna_client_folders::build_folders_author(
        Arc::clone(nest_rpc),
        keypair,
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
        &session,
    )
    .with_grant_log(crate::account_runtime::ledger_seam());
    let convs = ConversationsClient::new(Arc::clone(nest_rpc));
    Ok((author, convs))
}

/// Create-if-requested + serve-enable + reconcile the `WebdavKeysBlob` for `set`,
/// returning the number of served sets the reconciled blob now carries. The
/// deterministic, live-sync-engine-free precondition the WebDAV read+write e2e
/// needs: the WebDAV client's own PUT does the chunk+seal+upload+record
/// (`webdav backend.go chunkSealUpload`), so an *empty* served set suffices — no
/// folder bind, no back-catalogue re-seal. Unshared throughout (`channel_id =
/// None` — the serve pseudo-channel keys custody).
async fn serve_enable_folder_flow(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
    set: &str,
    create: bool,
) -> Result<u64, String> {
    let (author, _convs) = build_folders_author(nest_rpc, secret_hex)?;
    if create {
        author
            .create_set(fauna_client_folders::folders::FolderCreateRequest {
                name: set.to_string(),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("create set {set:?}: {e}"))?;
    }
    // The production serve-toggle composition (serve_enable + WebdavKeysBlob
    // re-provision) — `FoldersAuthor::serve_set`, the same shared path the
    // FFI/wasm faces the slice-6b UI drives call. `None` channel = an unshared
    // set (the common test case).
    let count = author
        .serve_set(set, None, true)
        .await
        .map_err(|e| format!("serve_set {set:?}: {e}"))?;
    Ok(count as u64)
}

/// Re-read the cross-user actor roster after a share/remove write (for the
/// `FolderActorsLoaded` refresh). An owner-only set → empty roster, not an error.
async fn read_folder_actors(
    nest_rpc: &Arc<NestClient>,
    name: &str,
) -> Result<Vec<fauna_client_folders::folders::FolderActorMember>, String> {
    let folders = fauna_client_folders::FoldersClient::new(Arc::clone(nest_rpc));
    match folders.actor_members_list(name.to_string()).await {
        Ok(reply) => Ok(reply.members),
        Err(NestClientError::Rpc(ref e)) if e.code == "fauna.folders.not_shared" => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Resolve the bare handle → its actor, then run `FoldersAuthor::share_set`
/// (same-nest), and re-read the roster. Returns the fresh member list + the
/// shared set's derived `ChannelId` (hex) for the row's remove buttons.
/// `access` is the invited member's grant (`Some("writer")` for read-write;
/// `None` = reader — the `folder-share-role-select` value).
async fn do_share_folder(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
    name: &str,
    handle: &str,
    access: Option<String>,
) -> Result<
    (
        Vec<fauna_client_folders::folders::FolderActorMember>,
        Option<String>,
    ),
    String,
> {
    let (author, convs) = build_folders_author(nest_rpc, secret_hex)?;
    let resolved = convs
        .actor_by_handle(handle.to_string())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no actor for handle '{handle}'"))?;
    let member = ActorId::from_hex(&resolved.actor_id).map_err(|e| e.to_string())?;
    let outcome = author
        .share_set(&convs, name, member, None, access)
        .await
        .map_err(|e| e.to_string())?;
    let channel_id = fauna_mls::types::ChannelId(outcome.channel_id).to_string();
    let members = read_folder_actors(nest_rpc, name).await?;
    Ok((members, Some(channel_id)))
}

/// Run `FoldersAuthor::remove_member` (evict + rotate the content key), then
/// re-read the roster. `channel_id_hex` is the set's derived `ChannelId`.
async fn do_remove_folder_member(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
    name: &str,
    channel_id_hex: &str,
    member_hex: &str,
) -> Result<Vec<fauna_client_folders::folders::FolderActorMember>, String> {
    let (author, _convs) = build_folders_author(nest_rpc, secret_hex)?;
    let channel_id =
        fauna_mls::types::ChannelId::from_hex(channel_id_hex).map_err(|e| e.to_string())?;
    let member = ActorId::from_hex(member_hex).map_err(|e| e.to_string())?;
    author
        .remove_member(name, channel_id.0, member)
        .await
        .map_err(|e| e.to_string())?;
    read_folder_actors(nest_rpc, name).await
}

/// Decode a raw hex MLS group id into its derived [`ChannelId`](fauna_mls::types::ChannelId)
/// — the shared `fauna_mls::types::ChannelId::from_group_id_hex`.
/// **Plain `hex::decode`, NOT `hex32::decode`** — `MlsGroup::new` never sets an
/// explicit `.group_id(...)`, so OpenMLS mints its own random group id, which is
/// **16 bytes**, not 32; `ChannelId::from_group_id` (a BLAKE3 KDF) already treats
/// it as an arbitrary-length slice. `hex32::decode`'s exactly-32-bytes
/// requirement silently 404'd every real group id here (found 2026-07-13,
/// mirroring the same bug fixed in `fauna-ffi/src/folders_author.rs`
/// `channel_id_from_hex`, found 2026-07-12 driving the apple leg). Shared by
/// [`fetch_folder_actors`](FaunaClient::fetch_folder_actors) (roster
/// re-fetch) and [`do_serve_set_folder`] (WebDAV serve toggle) — the two sites
/// that decode a *raw* group id, as opposed to an already-derived `ChannelId`
/// hex ([`do_remove_folder_member`] takes one of those and rightly uses
/// `ChannelId::from_hex`, which *is* 32 bytes). Also the one derivation the
/// folders page uses — `LinuxMlsQuery`'s join filter and the cross-nest
/// bind gesture's `ForeignBind` — so a foreign set is addressed by the same
/// channel its home nest knows it by; a second derivation here would refuse
/// every bind on a legitimately-granted set.
pub(crate) fn channel_id_from_group_id_hex(
    mls_group_id_hex: &str,
) -> Result<fauna_mls::types::ChannelId, hex::FromHexError> {
    fauna_mls::types::ChannelId::from_group_id_hex(mls_group_id_hex)
}

#[cfg(test)]
mod channel_id_from_group_id_hex_tests {
    //! Regression for the `hex32::decode`-on-a-16-byte-group-id bug (found
    //! 2026-07-13 by grep from the apple leg; mirrors
    //! `fauna-ffi/src/folders_author.rs`'s `channel_id_from_hex` tests,
    //! found 2026-07-12). Before the fix, `fetch_folder_actors` silently
    //! swallowed every real group id via `.ok()` (→ `channel_id: None` on a
    //! roster re-fetch) and `do_serve_set_folder` errored outright on the
    //! WebDAV serve toggle.
    use super::channel_id_from_group_id_hex;

    /// A real OpenMLS-minted group id: 16 bytes, not the 32 `hex32::decode`
    /// used to require.
    const REAL_GROUP_ID_HEX: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn decodes_a_real_16_byte_openmls_group_id() {
        assert_eq!(REAL_GROUP_ID_HEX.len(), 32, "32 hex chars = 16 bytes");
        let raw = hex::decode(REAL_GROUP_ID_HEX).unwrap();
        assert_eq!(raw.len(), 16);
        let want = fauna_mls::types::ChannelId::from_group_id(&raw);
        let got =
            channel_id_from_group_id_hex(REAL_GROUP_ID_HEX).expect("16-byte group id is valid");
        assert_eq!(got, want);
    }

    #[test]
    fn hex32_decode_would_have_rejected_this_same_id_pre_fix() {
        // Documents exactly the bug this module guards against: `hex32::decode`
        // requires exactly 32 bytes (64 hex chars) and 404s on a real 16-byte
        // group id. Never call `fauna_core::hex32::decode` on a raw MLS group
        // id again — see the doc comment on `channel_id_from_group_id_hex`.
        assert!(fauna_core::hex32::decode(REAL_GROUP_ID_HEX).is_err());
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!(channel_id_from_group_id_hex("not-hex").is_err());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let padded = format!("  {REAL_GROUP_ID_HEX}\n");
        let want = channel_id_from_group_id_hex(REAL_GROUP_ID_HEX).unwrap();
        assert_eq!(channel_id_from_group_id_hex(&padded).unwrap(), want);
    }
}

/// Run `FoldersAuthor::serve_set` (`enable` = ON: genesis/migration + blob
/// provision; OFF: content-key rotation + blob re-provision without the set).
/// `mls_group_id` is the raw hex group id (`None` for unshared), decoded via
/// [`channel_id_from_group_id_hex`], mirroring the `folders_serve_set` FFI
/// face.
async fn do_serve_set_folder(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
    name: &str,
    mls_group_id: Option<String>,
    enable: bool,
) -> Result<(), String> {
    let (mut author, _convs) = build_folders_author(nest_rpc, secret_hex)?;
    // The flipping client's walk over the set's pre-serve files
    // (`webdav-server.md` § Key model (c)), recorded under the device the
    // Media page records under. The local sync agent's own pass may run too —
    // both converge on the same store keys.
    if let Ok(device_id) = crate::sync::device_id() {
        let keypair = ActorKeypair::from_secret_hex(secret_hex).map_err(|e| e.to_string())?;
        author = author.with_served_set_converge(fauna_client_folders::served_set_converge(
            Arc::clone(nest_rpc),
            &keypair,
            crate::account_runtime::folder_key_store(),
            hex::encode(device_id),
            crate::attested_predecessor_ids_for_secret_hex(secret_hex),
        ));
    }
    let channel_id = match mls_group_id {
        Some(hex) => {
            let cid =
                channel_id_from_group_id_hex(&hex).map_err(|e| format!("mls_group_id: {e}"))?;
            Some(cid.0)
        }
        None => None,
    };
    author
        .serve_set(name, channel_id, enable)
        .await
        .map(|_count| ())
        .map_err(|e| e.to_string())
}

/// Run `FoldersAuthor::paywall_set` (content-key genesis/re-seal + the nest
/// `web_paywall_tier` flag + the web-serve-holder `content.read{folder:set}`
/// grant mint). `mls_group_id` is the raw hex group id (`None` for owner-only),
/// decoded via [`channel_id_from_group_id_hex`]; the web-serve holder is
/// discovered via `fauna.bridges.fetch_bridge_pubkey`, mirroring the
/// `folders_paywall_set` FFI face's `discover_web_serve_holder` (priority #2 —
/// same discovery + orchestration, just crate-direct rather than over UniFFI).
async fn do_paywall_set_folder(
    nest_rpc: &Arc<NestClient>,
    secret_hex: &str,
    name: &str,
    mls_group_id: Option<String>,
    tier: &str,
) -> Result<(), String> {
    let (author, _convs) = build_folders_author(nest_rpc, secret_hex)?;
    let channel_id = match mls_group_id {
        Some(hex) => {
            let cid =
                channel_id_from_group_id_hex(&hex).map_err(|e| format!("mls_group_id: {e}"))?;
            Some(cid.0)
        }
        None => None,
    };
    // Discover the nest's web-serve holder (the grant's HPKE seal target) —
    // role `content-processor`, id `web-serve`; the nest self-enrolls it at boot,
    // so no seeding. `fauna.bridges.not_found` if none is enrolled.
    let holder = fauna_client_bridges::MailAdminClient::new(Arc::clone(nest_rpc))
        .fetch_bridge_pubkey("content-processor", "web-serve")
        .await
        .map_err(|e| e.to_string())?;
    let holder_pubkey: [u8; 32] = holder.x25519_pubkey.as_slice().try_into().map_err(|_| {
        format!(
            "web-serve holder returned a malformed X25519 pubkey ({} bytes, want 32)",
            holder.x25519_pubkey.len()
        )
    })?;
    let holder_mlkem_ek = holder.mlkem_ek.map(|ek| ek.to_vec());
    author
        .paywall_set(name, tier, channel_id, holder_pubkey, holder_mlkem_ek)
        .await
        .map_err(|e| e.to_string())
}

/// Whether this actor can serve any set over WebDAV — the crate-direct mirror of
/// the FFI's `folders_can_serve_webdav` (both answer the shared
/// `owner_can_serve_webdav`: does the account's mail custody hold an MSEK?).
///
/// Note it does **not** go through [`build_folders_author`] — the capability
/// needs only the mail custody, not the conversations rail, so the Folders page can
/// ask it before a session is ready, where the author path would fail
/// "conversations session not ready" and be indistinguishable from "cannot serve".
async fn do_can_serve_webdav(nest_rpc: &Arc<NestClient>, secret_hex: &str) -> Result<bool, String> {
    let _ = (nest_rpc, secret_hex);
    fauna_client_folders::owner_can_serve_webdav(crate::account_runtime::mail_store().as_ref())
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Shared-folder (cross-user) RECIPIENT-side helpers — folders.md § Sharing
// ---------------------------------------------------------------------------

/// One staged, not-yet-accepted cross-user folder share the recipient renders as
/// a `folder-pending-share`. The linux display projection of
/// [`fauna_client_inbox::FolderPendingShare`] (the raw `welcome_bytes` blob stays
/// server-side — accept re-resolves it by `inbox_id`); the crate-direct twin of the
/// FFI `FfiPendingShare`.
#[derive(Clone, Debug)]
pub struct PendingShareView {
    /// The durable-inbox row id — the accept/decline target.
    pub inbox_id: i64,
    /// The pre-computed "Shared by ‹…›" label from the shared crate
    /// (`fauna_core::format::account_display_label`: handle when present, else the
    /// canonical `short_id` of the sharer hex) — the one string the row renders, so
    /// the six apps cannot drift on the fallback truncation. Empty only for a
    /// fully unstamped cross-nest share; the row renders the unknown-sharer i18n
    /// label for that case.
    pub shared_by_display: String,
}

use fauna_client_inbox::PENDING_SHARE_PEEK_LIMIT;

/// Peek the recipient's staged folder shares (un-acked `channel_type=="folder"`
/// welcomes) → the display projection. A pure peek (never acks). Mirrors the
/// `fauna-ffi` `folders_pending_shares` recipe.
async fn do_list_pending_shares(
    nest_rpc: &Arc<NestClient>,
) -> Result<Vec<PendingShareView>, String> {
    let inbox = fauna_client_inbox::InboxClient::new(Arc::clone(nest_rpc));
    let shares = fauna_client_inbox::list_folder_pending_shares(&inbox, PENDING_SHARE_PEEK_LIMIT)
        .await
        .map_err(|e| e.to_string())?;
    Ok(shares
        .into_iter()
        .map(|s| PendingShareView {
            inbox_id: s.inbox_id,
            shared_by_display: s.shared_by_display,
        })
        .collect())
}

/// Accept a staged share by `inbox_id`: re-peek to resolve the Welcome bytes +
/// channel, `join_folder_welcome` (join off the chat rail — no chat thread), then
/// `ack`. Ack only after the join succeeds (crash-safe; both idempotent). Returns
/// the refreshed pending list (the accepted knock is now consumed). Runs the
/// shared `fauna_client_folders::accept_folder_share` recipe — the same one
/// fauna-ffi's `folders_accept_share`, tui and the wasm `foldersAcceptShare` twin
/// call, so the join-before-ack ordering is stated once (priority #2); only the
/// join is supplied here.
async fn do_accept_share(
    nest_rpc: &Arc<NestClient>,
    inbox_id: i64,
) -> Result<Vec<PendingShareView>, String> {
    let session = crate::conversations::conv_backend::active_session()
        .ok_or_else(|| "conversations session not ready".to_string())?;
    let inbox = fauna_client_inbox::InboxClient::new(Arc::clone(nest_rpc));
    fauna_client_folders::accept_folder_share(&inbox, inbox_id, |join| async move {
        let (channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx) = join.into_join_args();
        session
            .join_folder_welcome(channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx)
            .await
            .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?;
    do_list_pending_shares(nest_rpc).await
}

/// Decline a staged share by `inbox_id`: drop the recipient's roster row, then
/// `ack` — the Welcome goes unprocessed, so declining never joins, and the owner's
/// "Shared with" list stops over-reporting (`folders.md` § Sharing → *Adding the
/// 2nd..Nth member*). Returns the refreshed pending list. Runs the shared
/// `fauna_client_folders::decline_folder_share` recipe — the same one the
/// `fauna-ffi` `folders_decline_share` and the wasm `foldersDeclineShare` twins
/// call, so the ordering guarantee is stated once (priority #2).
async fn do_decline_share(
    nest_rpc: &Arc<NestClient>,
    inbox_id: i64,
) -> Result<Vec<PendingShareView>, String> {
    fauna_client_folders::decline_folder_share(
        &fauna_client_inbox::InboxClient::new(Arc::clone(nest_rpc)),
        &fauna_client_folders::FoldersClient::new(Arc::clone(nest_rpc)),
        inbox_id,
    )
    .await
    .map_err(|e| e.to_string())?;
    do_list_pending_shares(nest_rpc).await
}

/// Leave a set shared *with* the caller, addressed by the raw `mls_group_id` (hex)
/// the member holds in their B3 `FolderSummary`. Two idempotent, self-scoped
/// halves — the crate-direct mirror of the `fauna-ffi` `folders_leave` recipe:
/// (1) the nest roster self-drop `fauna.folders.leave` (`FoldersClient::leave`
/// removes the caller's own `actor_channels` row — the durable, security-meaningful
/// half: off the roster their `content_key.get` folds to `not_found`); then (2) the
/// local MLS forget (`ConversationsSession::leave_folder` → `MlsEngine::forget_group`),
/// so the set drops from the `has_group`-filtered member-visible list. The session
/// handle is resolved up-front (fail fast if MLS isn't live), but the **nest drop
/// runs first** so a failure there leaves local state untouched and a retry is clean;
/// both mutations are idempotent, so a re-run after a partial failure converges (the
/// nest leave then returns `left == false` and the forget is a no-op).
async fn do_leave_folder_share(nest_rpc: &Arc<NestClient>, group_id: String) -> Result<(), String> {
    let session = crate::conversations::conv_backend::active_session()
        .ok_or_else(|| "conversations session not ready".to_string())?;
    fauna_client_folders::leave_share(Arc::clone(nest_rpc), &session, group_id).await
}

/// `fauna.pending_actions.list`, filtered to still-`pending` rows (an
/// executed / cancelled / expired action has no cancel window left, so the
/// standing section has nothing to offer on it), newest-first order kept as
/// served. One helper because both the initial fetch and a landed cancel
/// must end on the SAME projection (mirrors tui's `list_pending_actions`,
/// `apps/fauna-tui/src/settings/mod.rs`).
async fn list_pending_actions(
    account: &fauna_client_account::AccountClient<Arc<NestClient>>,
) -> Result<Vec<fauna_protocol::pending_actions::PendingActionSummary>, String> {
    account
        .pending_actions_list()
        .await
        .map(|reply| {
            reply
                .actions
                .into_iter()
                .filter(|a| a.status == "pending")
                .collect()
        })
        .map_err(|e| format!("list pending actions: {e}"))
}

/// Resolve the 32-byte identity the connection is bound to — the `target_nest_id` every
/// DNS cert-issuance path feeds `DnsAction::IssueCert` / `BeginManualIssueCert`
/// (`tls-certificates.md` § C.3 D7: "the home nest the cert serves"; the
/// single-nest case is the connected nest itself). The shared
/// `fauna-client-pair::resolve_this_nest_id` (lifted from this fn — it and the
/// tui twin were byte-identical), so the linux issuance glue + auto-renew
/// cadence resolve the id exactly as tui and the other apps do. Reused by
/// [`FaunaClient::dns_issue_cert`], [`FaunaClient::dns_begin_manual_issue`],
/// and the auto-renew cadence.
use fauna_client_pair::resolve_this_nest_id;

/// [`resolve_this_nest_id`] as the 32-byte source-box key of the per-box
/// backup-destination list in `fauna.state.backup` — the id every backup read
/// and write names (the Backups page, the folder-coverage calls below, the
/// post-store-ready aftermath's re-grant leg). Unprovable ⇒ an error, never a
/// guess: reads then show a failure and writes refuse.
pub(crate) async fn bound_source_nest(nest: &Arc<NestClient>) -> Result<[u8; 32], String> {
    resolve_this_nest_id(nest)
        .await?
        .try_into()
        .map_err(|_| "this nest's id was not 32 bytes".to_string())
}

/// One auto-renew cadence tick (tls-certificates.md § C.3 C2). The tick itself —
/// refresh-then-ask order, the skip-if-empty rule, the per-domain non-fatal rule
/// and the trailing health re-read — lives in `fauna_client_dns`
/// (`auto_renew_scan` / `auto_renew_issue`), so every native app runs the
/// identical sequence. This wrapper owns only what is genuinely linux's: the
/// `target_nest_id` resolution ([`resolve_this_nest_id`] — linked-nests state,
/// D7, deliberately outside the DNS machine), the log sink, and shipping the
/// refreshed snapshot so an open admin-dns page reflects the new cert (nothing
/// due ⇒ ship nothing, no needless re-render).
///
/// Native-only — the order core is native (web renders the checkbox but runs no
/// cadence). The CA round-trip itself is covered by the shared machine's pebble
/// test, not the Python e2e (no CA in-harness).
async fn run_auto_renew_cadence_tick(
    machine: &fauna_client_dns::DnsManagementMachine,
    nest_rpc: &Arc<NestClient>,
    tx: &UiSender,
) {
    let domains = machine.auto_renew_scan().await;
    if domains.is_empty() {
        return;
    }
    let target_nest_id = match resolve_this_nest_id(nest_rpc).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("auto-renew cadence: resolve target nest id: {e}");
            return;
        }
    };
    let pass = machine.auto_renew_issue(domains, target_nest_id).await;
    for failure in &pass.failed {
        tracing::warn!(
            "auto-renew cadence: IssueCert {}: {}",
            failure.domain,
            failure.error
        );
    }
    tx.send(UiMessage::Data(DataMessage::AdminDnsRecordsLoaded {
        snapshot: machine.snapshot(),
    }));
}

// ---------------------------------------------------------------------------
// Encrypted-CalDAV helpers (events.md Decision B — the linux Events page talks
// to the encrypted `bridge_caldav_*` store via `fauna-client-caldav`).
// ---------------------------------------------------------------------------

/// Current Unix epoch seconds — the `internal_date` (CREATED / LAST-MODIFIED
/// surrogate) for a freshly written event.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// Decode a 64-char hex string into a 32-byte array (`calendar_id` / `uid_hash`);
/// `None` on malformed input. Thin `Option`-returning adapter over the shared
/// [`fauna_core::hex32::decode`].
fn hex32(s: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(s).ok()
}

/// Render a roster as the `attendee-item` display strings the detail panel shows
/// (`"<name-or-email> (<status>)"`). Shared by `rsvp_event` + `invite_to_event`,
/// which both push the new roster to the open panel via `EventAttendeesLoaded`.
/// Map the shared `AttendeeInfo` roster (the WS create/RSVP result) into the
/// detail panel's `CalDavAttendee` rows, carrying name + email + projected RSVP
/// through verbatim so the panel renders the monogram, name, email, and colored
/// status (events.md § Attendee list presentation) — the `fetch_attendees`
/// store path already yields `CalDavAttendee`s, so both producers agree.
fn attendee_rows(attendees: &[AttendeeInfo]) -> Vec<caldav_backend::CalDavAttendee> {
    attendees
        .iter()
        .map(|a| caldav_backend::CalDavAttendee {
            email: a.email.clone(),
            name: a.name.clone(),
            rsvp: a.fauna_status.clone(),
        })
        .collect()
}

/// The actor's identity (`actor_id`) and mail-sealing key (`msek`) — the two
/// inputs every encrypted-CalDAV op needs. `None` when the secret is malformed,
/// the mail custody read fails, or mail/CalDAV is not enabled (no `msek` minted yet —
/// `MailSettingsMachine::enable_mail`); callers degrade to an empty
/// "enable calendar" state rather than erroring. Thin wrapper over the shared
/// [`fauna_client_config::dav_store_context`] (also tui's `caldav_context` and
/// the FFI face's `dav_store_context`) — this seam supplies only what the linux
/// shell knows: the actor keypair rebuilt from the connection secret.
async fn caldav_context(nest: &Arc<NestClient>, secret_hex: &str) -> Option<DavStoreContext> {
    let _ = nest;
    let actor_id = secret_to_keypair(secret_hex).ok()?.actor_id().0;
    dav_store_context(crate::account_runtime::mail_store().as_ref(), actor_id).await
}

/// Query all events in one calendar and decode them (unseal + parse). Collapses
/// `CalendarNotFound` to an empty list (a fresh, not-yet-provisioned calendar);
/// returns the transport/decode error as a string on failure.
async fn query_events_in(
    client: &CalDavClient<Arc<NestClient>>,
    actor_id: &[u8; 32],
    calendar_id: &[u8; 32],
    msek: &[u8; 32],
    prior_mseks: &[[u8; 32]],
) -> Result<Vec<fauna_client_caldav::DecodedEvent>, String> {
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::from_mseks(msek, prior_mseks),
        )
        .await
        .map_err(|e| e.to_string())?;
    match page {
        DecodedEventsPage::Ok { events, .. } => Ok(events),
        DecodedEventsPage::CalendarNotFound => Ok(vec![]),
    }
}

/// [`query_events_in`] plus the delta-sync baseline: records the calendar's
/// `highestmodseq` as its sync-token, which is what lets the NEXT
/// `fetch_events` skip the read entirely when nothing changed.
///
/// Seeding here rather than from a `sync_calendar_since` reply is deliberate:
/// this modseq describes exactly the event set that was just rendered, so the
/// baseline and what the user sees cannot disagree. A calendar nest no longer
/// holds drops its token instead — the next call then has no baseline and reads
/// fully, which is the honest answer for a calendar that may have just been
/// recreated.
async fn query_events_in_seeded(
    client: &CalDavClient<Arc<NestClient>>,
    actor_id: &[u8; 32],
    calendar_id: &[u8; 32],
    msek: &[u8; 32],
    prior_mseks: &[[u8; 32]],
    sync_tokens: &Arc<std::sync::Mutex<CalendarSyncTokens>>,
) -> Result<Vec<fauna_client_caldav::DecodedEvent>, String> {
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::from_mseks(msek, prior_mseks),
        )
        .await
        .map_err(|e| e.to_string())?;
    match page {
        DecodedEventsPage::Ok {
            events,
            highestmodseq,
            ..
        } => {
            sync_tokens
                .lock()
                .expect("sync tokens")
                .seed(calendar_id, highestmodseq);
            Ok(events)
        }
        DecodedEventsPage::CalendarNotFound => {
            sync_tokens.lock().expect("sync tokens").forget(calendar_id);
            Ok(vec![])
        }
    }
}

/// Silent sign-in over WS-RPC: run the shared `fauna.auth.challenge` +
/// `fauna.auth.verify` ceremony (`fauna-launch-machine`'s production
/// `WsAuthConnector` over a fresh `AnonymousNestClient` — the same path
/// `LaunchMachine::start` drives) and use the returned `{handle, domain,
/// tier}` to refresh the per-launch server-data cache. Called from
/// `FaunaClient::silent_sign_in` on startup; the resulting fields are written
/// into libsecret and pushed to the UI via `DataMessage::IdentityRefreshed`.
///
/// Returns `Ok(None)` when the actor isn't registered on this nest — the
/// `fauna.auth.not_registered` rejection on verify (the WS twin of the former
/// HTTP 404): a normal "not yet registered" case during onboarding that
/// shouldn't surface as an error in the UI. Every other rejection and every
/// transport fault is an `Err` (logged and swallowed by the caller).
async fn do_silent_sign_in(
    node_url: &str,
    secret_hex: &str,
) -> Result<SilentSignIn, anyhow::Error> {
    use fauna_launch_machine::{AuthConnector, WsAuthConnector};

    let secret = fauna_core::hex32::decode(secret_hex)
        .map_err(|e| anyhow::anyhow!("invalid hex secret: {e}"))?;

    // The classification — the whole of `security.md` § Post-auth surfacing's
    // client-side rule ("only the identity verdict escalates; every other
    // failure class stays swallowed") — is SHARED
    // (`fauna_launch_machine::classify_silent_challenge`, lifted out of here
    // when tui became the second wired leg). Its pins moved with it. This
    // adapter only re-encodes the swallowed arm as this module's `Err`, which
    // its callers' `?`/log-and-drop sites are written against.
    //
    // No reach hint either: the dial policy is
    // `LaunchMachine::run_silent_challenge_phase`'s single copy
    // (`onboarding.md` § Reach hint's dial rule), and this refresh runs against
    // a domain that has already answered.
    adapt_verdict(fauna_launch_machine::classify_silent_challenge(
        WsAuthConnector
            .silent_challenge(node_url, None, &secret)
            .await,
    ))
}

/// The message a stopped supervisor's reason escalates as, if it is a
/// session-ending verdict — the three the background silent sign-in escalates,
/// read through the one shared classifier
/// (`NestClientError::session_ending_verdict`). Every other stop (a clean
/// close, a version skew, a mint fault) stays the connection indicator's.
fn session_ending_escalation(stop: &NestClientError) -> Option<DataMessage> {
    use fauna_client::SessionEndingVerdict as V;
    Some(match stop.session_ending_verdict()? {
        V::NestIdentityChanged => DataMessage::NestIdentityChanged,
        V::Superseded => DataMessage::IdentitySuperseded,
        V::SignInRefused => DataMessage::SignInRefused,
    })
}

/// Re-encode the shared [`SilentSignInVerdict`] as this module's
/// `Result<SilentSignIn, _>`, which the callers' log-and-drop sites are written
/// against.
///
/// Pure and separately pinned even though it is a total match: the shared
/// classifier's own tests cannot catch a regression *here*, and the specific
/// regression available — folding `IdentityChanged` into the `Err` arm — is
/// precisely the pre-2026-07-23 bug (a possible-MITM verdict that reads as a
/// network blip at the call site).
fn adapt_verdict(
    verdict: fauna_launch_machine::SilentSignInVerdict,
) -> Result<SilentSignIn, anyhow::Error> {
    use fauna_launch_machine::SilentSignInVerdict;
    match verdict {
        SilentSignInVerdict::Refreshed {
            handle,
            domain,
            tier,
        } => Ok(SilentSignIn::Refreshed {
            handle,
            domain,
            tier,
        }),
        SilentSignInVerdict::NotRegistered => Ok(SilentSignIn::NotRegistered),
        SilentSignInVerdict::IdentityChanged => Ok(SilentSignIn::IdentityChanged),
        SilentSignInVerdict::Superseded { new_actor_id_hex } => {
            Ok(SilentSignIn::Superseded { new_actor_id_hex })
        }
        // A locked account: terminal in the shared classifier, but the locked
        // surface (`devices.md` § The locked state, `ui/sessions.md` leg 2) is
        // not built on linux yet, so for now it logs through the error path —
        // every door refuses the bearer at use regardless.
        SilentSignInVerdict::Locked { locked_until_secs } => Err(anyhow::anyhow!(
            "this account is locked until {locked_until_secs} (Unix seconds)"
        )),
        SilentSignInVerdict::Failed { error } => Err(anyhow::anyhow!(error)),
    }
}

/// What a background silent sign-in learned. Variants rather than the old
/// `Option`, because the two escalating outcomes must not be reachable through
/// the error path (`security.md` § Post-auth surfacing).
///
/// The linux-local face of the shared [`SilentSignInVerdict`]: same meanings,
/// with the swallowed-failure arm carried by this module's `Err`.
enum SilentSignIn {
    /// The nest confirmed the identity; refresh the server-data cache.
    Refreshed {
        handle: String,
        domain: String,
        tier: String,
    },
    /// `fauna.auth.not_registered` — normal during onboarding, not an error.
    NotRegistered,
    /// The nest's pinned deployment identity changed mid-session. The session
    /// is already de-facto dead (its connections can no longer graduate), so
    /// the client blocks on the launch surface rather than showing a banner
    /// over a broken session.
    IdentityChanged,
    /// The identity was succeeded mid-session. The second escalating verdict,
    /// and for the same reason [`Self::IdentityChanged`] is one: the session is
    /// already de-facto dead — every connection this identity opens from here
    /// on is refused. Unlike the MITM verdict, the way out is not re-trust but
    /// importing the successor (`identity-succession.md` § Propagation → *Own
    /// device fleet*). The hex is the nest's **claimed** successor, carried for
    /// the log line only — nothing here presents it as fact.
    Superseded { new_actor_id_hex: String },
}

/// The 32-byte identity secret behind `secret_hex`, for the pre-login
/// resolver reads below. `None` on a malformed secret.
fn recovery_secret(secret_hex: &str) -> Option<[u8; 32]> {
    secret_to_keypair(secret_hex)
        .ok()
        .map(|kp| *kp.secret_bytes())
}

/// The admin's custodied deployment-seed box list, read before sign-in — the
/// source that gates the surviving-device `launch-recover-button` and fills the
/// `nest_recovery` hub (`box-recovery.md` § Recovery UI (step 4)).
///
/// One call into the shared pre-login resolver
/// (`fauna_client_account_runtime::deployment_seeds::recoverable_box_ids`;
/// `box-recovery.md` § The plane-era recovery floor → *(b) The reads*): this
/// device's own account store (under the same `StoreRoot::platform()` linux's
/// account runtime opens) **joined with** a cold read from the nest at
/// `node_url` when one is given and answers — never either-or, since a surviving
/// device's saved nest is, in the case recovery exists for, the dead box.
///
/// A *free* async fn (not a `FaunaClient` method) because it runs **before** any
/// `FaunaClient` exists. **Only the public `nest_actor_id` crosses out** — the
/// seed stays inside Rust. **Never errors**: a malformed secret or a failed
/// source collapses to what the other source answered (possibly empty).
pub async fn load_recoverable_boxes(node_url: Option<&str>, secret_hex: &str) -> Vec<String> {
    let Some(secret) = recovery_secret(secret_hex) else {
        tracing::warn!("load_recoverable_boxes: malformed secret; no box list");
        return vec![];
    };
    fauna_client_account_runtime::deployment_seeds::recoverable_box_ids(
        node_url.map(str::to_string),
        secret,
        fauna_sync_engine::root::StoreRoot::platform(),
    )
    .await
}

/// The `recover-selfhosted-command` for one custodied box — the installer `.env`
/// line carrying that box's `FAUNA_DEPLOYMENT_SEED` (`box-recovery.md`
/// § Recovery UI (step 4)), read through the same pre-login resolver as
/// [`load_recoverable_boxes`] and rendered by the one shared projection, so
/// every app emits a byte-identical line.
///
/// `None` on a malformed secret or when no source custodies that box. The page
/// then keeps its pending placeholder: showing a command carrying the **wrong**
/// box's seed would rebuild the box under a different `nest_actor_id`, which
/// every TOFU-pinned client then rejects — the exact trust break recovery exists
/// to prevent.
///
/// The seed IS surfaced here, by design — it is the installer input the admin
/// pastes (`box-recovery.md` § Trust & audience).
pub async fn load_selfhosted_recovery_command(
    node_url: Option<&str>,
    secret_hex: &str,
    nest_actor_id_hex: &str,
) -> Option<String> {
    let Some(secret) = recovery_secret(secret_hex) else {
        tracing::warn!("load_selfhosted_recovery_command: malformed secret; no command");
        return None;
    };
    fauna_client_account_runtime::deployment_seeds::selfhosted_command(
        node_url.map(str::to_string),
        secret,
        fauna_sync_engine::root::StoreRoot::platform(),
        nest_actor_id_hex,
    )
    .await
}

/// **The deployment-seed custody leg** (`box-recovery.md` § The plane-era
/// recovery floor → *(c) The writes*) over `nest` and the account-store
/// handle, mapping a run that ends with custody unconfirmed onto linux's
/// custody-warning surface — the `ActionResult::Failed` toast (app.rs), never a
/// silent drop. Called from both edges: [`FaunaClient::run_deployment_seed_custody_leg`]
/// (post-auth, when the store is already up) and `account_runtime::install`'s
/// installed arm (store-ready, when post-auth already landed).
pub async fn run_deployment_seed_custody_leg(
    nest: Arc<NestClient>,
    store: fauna_sync_engine::account_runtime::AccountStoreHandle,
    tx: UiSender,
) {
    if let Some(warning) =
        fauna_client_account_runtime::deployment_seeds::run_custody_leg(&nest, &store).await
    {
        tx.send(UiMessage::Action(ActionResult::Failed {
            context: "deployment-seed recovery custody".into(),
            error: warning,
        }));
    }
}

/// Derive the hex actor_id from a hex secret. Returns None when the
/// secret isn't 64 hex chars / 32 bytes. Used by the WS loop and the
/// FaunaClient::actor_id() accessor; replaces the previous AuthState
/// cache lookup since LaunchMachine doesn't expose actor_id.
pub(crate) fn actor_id_from_secret_hex(secret_hex: &str) -> Option<String> {
    ActorKeypair::from_secret_hex(secret_hex)
        .ok()
        .map(|kp| kp.actor_id_hex())
}

/// Verify this identity's succession against the registration chain and return
/// the successor's **public** actor_id hex — the launch `superseded` screen's
/// upgrade from the nest's *claim* to a proven fact
/// (`identity-succession.md` § Propagation → *Own device fleet*). `None` on
/// every failure, which leaves the claim-free message standing.
///
/// **Anonymous by necessity, not convenience:** the refused identity cannot
/// authenticate — that is what the refusal means — so this rides
/// `succession.lookup` / `registration.chain`, both pre-identity kinds.
///
/// **The nest's claimed successor is deliberately not an input.**
/// `resolve_successor` returns what the chain *authorizes*; a claim that
/// disagrees with the chain is a lie to log, never something to render.
///
/// The tui twin is `apps/fauna-tui/src/launch.rs::verify_superseded_successor`.
/// Only the transport binding is per-app: `RecoveryClient<R>` is generic over
/// `RpcRequester` precisely so the *ceremony* stays shared (it also serves wasm,
/// which cannot take a native connector), and each app supplies its own `R`.
pub(crate) async fn verify_succession_successor(
    nest_url: &str,
    secret_hex: &str,
) -> Option<String> {
    let old_actor_hex = actor_id_from_secret_hex(secret_hex)?;
    let old_actor_id = fauna_core::identity::ActorId::from_hex(&old_actor_hex).ok()?;
    let anon = fauna_anon_client::AnonymousNestClient::connect(nest_url)
        .await
        .ok()?;
    let client = fauna_client_recovery::RecoveryClient::new(anon);
    match fauna_client_recovery::resolve_successor(&client, old_actor_id, None).await {
        Ok(Some(verified)) => {
            let successor = verified.new_actor_id.to_hex();
            tracing::info!(
                "[launch] succession verified against the registration chain; \
                 successor {successor}"
            );
            Some(successor)
        }
        Ok(None) => {
            // Refused as superseded, yet the chain shows no succession. Nothing
            // to tell the user — but exactly the disagreement an admin wants.
            tracing::warn!("[launch] refused as superseded, yet the chain shows no succession");
            None
        }
        Err(e) => {
            tracing::warn!("[launch] could not verify the succession: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Recovery kit ceremonies — shared helpers behind the FaunaClient methods
// above (`settings.md` § Recovery kit). The tui twin is
// `apps/fauna-tui/src/settings/mod.rs`'s `recovery_identity` / `with_status` /
// `escrow_predecessor_seeds` — mirrored here rather than lifted, since each
// app supplies its own message-passing shape around the same shared
// `fauna_client_recovery` ceremonies (priority #2 already covers the ceremony
// logic itself; only the transport binding is per-app).
// `mirror_recovery_head` itself lives in
// `fauna_client_recovery::ceremony::mirror_recovery_head` — tui, linux, and
// every UniFFI app call the one shared function.
// ---------------------------------------------------------------------------

/// Decode a session's identity secret into the keypair `fauna_client_recovery`
/// ceremonies sign with. Rebuilt per op rather than cached: the ceremonies are
/// rare, and the fewer places a decoded identity key rests the better. `Err`
/// is the message the section paints — an undecodable secret is a real
/// condition (a half-restored session), not a panic site.
fn recovery_identity(secret_hex: &str) -> Result<ActorKeypair, String> {
    ActorKeypair::from_secret_hex(secret_hex)
        .map_err(|e| crate::i18n::strings::errors::recovery_identity_unreadable(&e.to_string()))
}

/// The full-chain predecessor seeds an **escrow-writing** ceremony must carry
/// (`identity-succession.md` § Seed escrow: a kit *replacement* re-puts the
/// resting blob too, and the blob it replaces may be carrying predecessor
/// seed(s) inside the corpus re-seal window; writing it without them silently
/// reopens the device-loss race the escrow backstop exists to close).
fn escrow_predecessor_seeds(secret_hex: &str) -> Vec<fauna_client_recovery::PredecessorSeed> {
    let Ok(keypair) = ActorKeypair::from_secret_hex(secret_hex) else {
        return Vec::new();
    };
    // The decode (and its deliberate skip-a-bad-row behaviour) is shared — tui
    // and the FFI apps resolve the same rows the same way.
    fauna_client_recovery::predecessor_seeds_from_rows(
        crate::account_registry().predecessor_seeds(&keypair.actor_id_hex()),
    )
}

/// The `create_kit` ceremony, shared between the create and replace gestures
/// (they differ only in `prior`) — mints/replaces the kit, mirrors the landed
/// head into the profile, then re-reads status in the SAME task so the section
/// repaints from one message with no follow-up fetch (tui's `with_status`).
async fn recovery_create_kit(
    nest: Arc<NestClient>,
    secret_hex: &str,
    prior: Option<&fauna_core::recovery::RecoveryKey>,
) -> Result<(String, fauna_client_recovery::RecoveryKitStatus), String> {
    let identity = recovery_identity(secret_hex)?;
    let client = fauna_client_recovery::RecoveryClient::new(Arc::clone(&nest));
    let predecessors = escrow_predecessor_seeds(secret_hex);
    let minted =
        match fauna_client_recovery::create_kit(&client, &identity, prior, &predecessors).await {
            Ok(kit) => {
                // The chain moved: carry it to every linked nest now, not at
                // the next full pass (`identity-succession.md` § Enforcement on
                // the home nest).
                if let Some(store) = crate::account_runtime::handle() {
                    store.registration_chain_moved();
                }
                let profile_predecessors = fauna_client_profile::predecessors_from_hex(
                    &crate::account_registry().predecessors_of(&identity.actor_id_hex()),
                );
                fauna_client_recovery::ceremony::mirror_recovery_head(
                    nest,
                    &identity,
                    &profile_predecessors,
                    &kit,
                )
                .await;
                Ok(kit.secret_hex().to_string())
            }
            Err(e) => Err(e.to_string()),
        };
    with_fresh_status(&client, &identity, minted).await
}

/// The successor's own scoped MLS store — where the retry builds an engine when
/// conversations are not up, and where the ceremony's sweep re-joins the groups.
///
/// `account_state_dir` (not the pure `actor_state_dir`) because the successor
/// is a **live** account on this device, whose scope is created on demand.
fn successor_mls_db_path(successor_actor_hex: &str) -> std::path::PathBuf {
    crate::account_scope::account_state_dir(successor_actor_hex).join("mls_state.db")
}

/// The RETIRED identity's scoped MLS store.
///
/// ⚠ **The pure `actor_state_dir`, never `account_scope::account_state_dir`**
/// — that one *creates* the scope, and the retired identity's must never be
/// conjured. The shared driver only ever `try_exists`-checks this
/// path: a device that never held conversation state for that identity has
/// nothing to sweep *from*, and conjuring an empty store there would report a
/// clean run over zero groups while the thief's leaf sits untouched in every
/// real group. `actor_state_dir` is pure and refuses only a malformed hex, which
/// a live keypair cannot produce, so the fallback names a path that does not
/// exist — which the existence check then answers as "no old state", the truth
/// on a device whose store cannot even be addressed.
fn retired_mls_db_path(old_actor_hex: &str) -> std::path::PathBuf {
    let flat = crate::account_scope::install_state_base()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/fauna"));
    fauna_sync_engine::db::actor_state_dir(&flat, old_actor_hex)
        .unwrap_or_else(|_| flat.join(old_actor_hex))
        .join("mls_state.db")
}

/// The succession ceremony's whole background half — `succeed_with_held_kit`,
/// the seed persist, and whichever of the two arms the nest's reply lands on.
///
/// Mirrors tui's `Op::RecoveryStolen` body; everything with a decision in it is
/// the shared `fauna_client_recovery::ceremony`'s, and what is left here is
/// linux's own store-path resolution and its registry handle.
async fn do_succeed_identity(
    nest: Arc<NestClient>,
    secret_hex: &str,
    phrase: &str,
    node_url: &str,
    old_engine: Option<Arc<fauna_mls::engine::MlsEngine>>,
) -> fauna_client_recovery::ceremony::StolenOutcome {
    use fauna_client_recovery::ceremony::StolenOutcome;
    // The ceremony never started — nothing moved.
    let identity = match recovery_identity(secret_hex) {
        Ok(identity) => identity,
        Err(e) => return StolenOutcome::not_landed(e),
    };
    let client = fauna_client_recovery::RecoveryClient::new(Arc::clone(&nest));
    // The old identity rides as `old_identity`: this device still holds the seed
    // (it is signed in), and `old_sig` is informational continuity — never
    // load-bearing (`identity-succession.md` § The succession statement) — so
    // supplying it changes no consumer's verdict.
    let old_actor_id = identity.actor_id();
    // A refusal before the submit is the not-landed arm: nothing moved, and
    // the seed it minted authorizes nothing.
    let attempt = match fauna_client_recovery::succeed_with_held_kit(
        &client,
        old_actor_id,
        phrase,
        Some(&identity),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(e) => return StolenOutcome::not_landed(e),
    };

    // FIRST, before anything that can fail or block: make the successor seed
    // durable. From the nest's commit until this line that seed is the only copy
    // of the key the account now belongs to, and the sweep below makes network
    // calls over every group.
    //
    // Read off the attempt rather than a matched arm: BOTH arms carry the seed,
    // and the `Unconfirmed` one is precisely the case where the nest may already
    // have committed while telling us it did not. Persisting only the confirmed
    // arm would reproduce that finding one layer up. Hoisted rather than built
    // inline because the `Unconfirmed` arm READS BACK through this same registry
    // to decide whether it may claim the seed is saved, and a second registry
    // over a second store handle could answer about a different store.
    let accounts = crate::account_registry();
    if let Err(e) = accounts.add_account(attempt.successor_secret_hex(), Some(node_url), None) {
        // Not fatal: the fold's adoption tries again and, failing that, puts the
        // seed on screen as the only way back. Logged because this is the moment
        // of maximum exposure.
        tracing::error!(
            "[settings/recovery] persisting the successor seed straight after the succession \
             landed: {e}"
        );
    }

    // Everything below is the *propagation* half — it can fail without unmaking
    // the succession, so its errors ride the outcome as a report rather than
    // turning the whole ceremony into an `Err`.
    let outcome = match attempt {
        fauna_client_recovery::SuccessionAttempt::Confirmed(handoff) => {
            let sweep = fauna_client_recovery::ceremony::sweep_after_succession(
                node_url,
                old_engine.as_deref(),
                &handoff,
                successor_mls_db_path,
            )
            .await;
            StolenOutcome::Landed(fauna_client_recovery::ceremony::LandedSuccession::new(
                handoff.successor_secret_hex().to_string(),
                handoff.new_actor_id,
                sweep,
                handoff.succeeded_at,
            ))
        }
        fauna_client_recovery::SuccessionAttempt::Unconfirmed(unconfirmed) => {
            fauna_client_recovery::ceremony::finish_unconfirmed_succession(
                node_url,
                old_engine.as_deref(),
                unconfirmed.old_actor_id,
                unconfirmed.successor_secret_hex(),
                &unconfirmed.error,
                &accounts,
                successor_mls_db_path,
            )
            .await
        }
    };
    let StolenOutcome::Landed(landed) = &outcome else {
        return outcome;
    };

    // Record which row the retired identity is, durably, while we still know —
    // and BEFORE the fold switches the account, which is load-bearing twice
    // over. (1) `record_succession` is also where a **bound** launch follows the
    // account to the successor (`account-scoping.md` § Concurrent instances →
    // *The binding follows the account*); the switch re-enters the launch
    // machine, whose bound-or-refuse gate would otherwise still see the retired
    // id. (2) Nothing after the switch names that row, yet the retry below and
    // the corpus re-seal need it at **every** later sign-in, on every device, to
    // open blobs still sealed under the predecessor's `BackupKey`
    // (`succession-aftermath.md` § Re-key scope) — and
    // `ceremony::retry_predecessor` reads exactly this link to find the store it
    // sweeps from. Without it the retry answers `NotLanded` on the one device
    // that can actually finish the job.
    //
    // Never fatal: the succession has already landed on the nest, and a failure
    // here costs the automatic re-seal and the retry, not the account.
    //
    // Parked FIRST, in the same registry: what only this ceremony knows (the
    // sweep's roster, the nest's commit stamp) is the member-item and
    // filter-mark raises' input, drained by the successor's post-store-ready
    // pass — the single durable decision point of those raises
    // (`fauna_client_recovery::aftermath::PendingCeremony`).
    fauna_client_recovery::aftermath::PendingCeremony::new(
        &old_actor_id,
        &landed.sweep.review_roster(),
        landed.succeeded_at,
    )
    .park(&accounts, &landed.new_actor_id.to_hex());
    if let Err(e) =
        accounts.record_succession(&old_actor_id.to_hex(), &landed.new_actor_id.to_hex())
    {
        tracing::warn!(
            "[settings/recovery] recording the succession link failed — the corpus re-seal will \
             not find this predecessor automatically, and the group-sweep retry will report that \
             no move was recorded: {e}"
        );
    }
    outcome
}

/// The retry's background half. `Err` carries an already-selected sentence, not
/// a bare error string: three of the four answers ARE the gesture's whole
/// product (`settings.md` § Recovery kit — the button "must answer in words on
/// every press"), so linux resolves the shared projection's key and words
/// nothing itself.
async fn do_retry_group_sweep(
    nest: Arc<NestClient>,
    secret_hex: &str,
    successor_engine: Option<Arc<fauna_mls::engine::MlsEngine>>,
    tx_for_ledger: UiSender,
) -> Result<Box<fauna_client_recovery::ceremony::SweepStatus>, String> {
    use fauna_client_recovery::ceremony::{SweepRetryAnswer, SweepStatus};
    match run_group_sweep_retry(nest, secret_hex, successor_engine, tx_for_ledger).await? {
        SweepRetryAnswer::Swept(report) => Ok(Box::new(SweepStatus::Ran(report))),
        // The three terminal answers and the transport arm, each already a
        // sentence the shared projection chose — linux only resolves the key,
        // exactly as it does for the sweep's own lines.
        answered => Err(sweep_answer_sentence(&answered).unwrap_or_default()),
    }
}

/// The shared projection's sentence for a sweep-retry answer, resolved — `None`
/// for `Swept`, whose outcome renders through the sweep's own lines.
fn sweep_answer_sentence(
    answer: &fauna_client_recovery::ceremony::SweepRetryAnswer,
) -> Option<String> {
    answer
        .message()
        .map(|line| line.resolve(crate::i18n::strings::lookup))
}

/// The owed sweep's background half — the relaunch adoption's unbidden press
/// (`succession-propagation.md` § Propagation → *Own device fleet*, the
/// relaunch-adoption clause). The same ceremony as a press; what differs is
/// what it parks: an adoption carries **no** report, so every answer parks
/// one, and which one is shared Rust's call (`SweepRetryAnswer::into_owed_status`
/// — never an empty `Ran`). `Err` is the one case where nothing ran at all (a
/// session whose secret does not parse); the caller re-arms on it.
async fn do_discharge_owed_sweep(
    nest: Arc<NestClient>,
    secret_hex: &str,
    successor_engine: Option<Arc<fauna_mls::engine::MlsEngine>>,
    tx_for_ledger: UiSender,
) -> Result<
    (
        Box<fauna_client_recovery::ceremony::SweepStatus>,
        Option<String>,
    ),
    String,
> {
    let answer = run_group_sweep_retry(nest, secret_hex, successor_engine, tx_for_ledger).await?;
    tracing::info!("[succession-sweep] owed sweep answered {}", answer.kind());
    let sentence = sweep_answer_sentence(&answer);
    Ok((Box::new(answer.into_owed_status()), sentence))
}

/// The one body behind the press and the owed discharge: run the shared retry
/// ceremony and, when it swept, re-park its roster for the aftermath ledger.
/// `Err` only when nothing could run (the session's secret does not parse).
async fn run_group_sweep_retry(
    nest: Arc<NestClient>,
    secret_hex: &str,
    successor_engine: Option<Arc<fauna_mls::engine::MlsEngine>>,
    tx_for_ledger: UiSender,
) -> Result<fauna_client_recovery::ceremony::SweepRetryAnswer, String> {
    let nest_for_ledger = Arc::clone(&nest);
    use fauna_client_recovery::ceremony::SweepRetryAnswer;

    // The retired identity this account came from, off the shared resolution —
    // the DIRECT hop only, for the reason its own doc gives.
    let (successor_hex, found) = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(keypair) => {
            let successor_hex = keypair.actor_id_hex();
            let found = fauna_client_recovery::ceremony::retry_predecessor(
                &crate::account_registry(),
                &successor_hex,
            );
            (successor_hex, found)
        }
        Err(e) => return Err(e.to_string()),
    };
    let old_hex = found.as_ref().map(|(old_hex, _)| old_hex.clone());
    let old_secret_hex = found.and_then(|(_old_hex, seed)| seed);
    let answer = fauna_client_recovery::ceremony::retry_sweep_as_successor(
        nest,
        secret_hex,
        old_secret_hex.as_deref(),
        retired_mls_db_path,
        successor_mls_db_path,
        successor_engine.as_deref(),
    )
    .await;
    if let SweepRetryAnswer::Swept(report) = &answer {
        // The retry's roster joins the registry's parked ceremony (union by
        // person) and the post-store-ready pass drains it now — the one
        // path for the ceremony's roster and a retry's.
        if let Some(old) = old_hex
            .as_deref()
            .and_then(|hex| fauna_core::hex32::decode(hex).ok())
        {
            fauna_client_recovery::aftermath::PendingCeremony::repark_retried_roster(
                &crate::account_registry(),
                &successor_hex,
                &fauna_core::identity::ActorId(old),
                &report.unattested_members(),
            );
            if let Some(handle) = crate::account_runtime::handle() {
                tokio::spawn(crate::succession_aftermath::run_ledger(
                    Arc::clone(&nest_for_ledger),
                    handle,
                    tx_for_ledger,
                ));
            }
        }
    }
    Ok(answer)
}

/// Pair a ceremony's returned secret with a fresh status read — the shared
/// `fauna_client_recovery::kit_status_after_mint`.
async fn with_fresh_status(
    client: &fauna_client_recovery::RecoveryClient<Arc<NestClient>>,
    identity: &ActorKeypair,
    minted: Result<String, String>,
) -> Result<(String, fauna_client_recovery::RecoveryKitStatus), String> {
    fauna_client_recovery::kit_status_after_mint(client, identity, minted).await
}

// ---------------------------------------------------------------------------
// Send: build the signed email payload + hand it to the home nest
// ---------------------------------------------------------------------------

/// Build a signed `email/v1` `(ContactRequest, Post)` payload and hand it to
/// our home nest over `fauna.inbox.send` for same-nest local delivery.
///
/// The payload is composed by the shared `fauna_client_core::email` writer —
/// the *same* one web consumes via wasm and apple/android/windows via the
/// UniFFI `build_signed_email` export — so the wire shape can't drift per
/// client (priority #2/#4). This leaves the function as thin glue: hex-decode
/// the keypair + recipient, compose, send.
async fn build_and_send(
    nest_rpc: Arc<NestClient>,
    node_url: &str,
    secret_hex: &str,
    recipient_actor_hex: &str,
    subject: &str,
    body: &str,
) -> Result<(), anyhow::Error> {
    // 1. Derive the keypair from the caller's secret.
    let kp = ActorKeypair::from_secret_hex(secret_hex)?;

    // 2. Parse the recipient actor id.
    let recipient_bytes = fauna_core::hex32::decode(recipient_actor_hex)?;

    // 3. Compose the canonical signed `(ContactRequest, Post)` tuple with the
    //    shared writer. Using it (rather than an inline copy) keeps the wire
    //    shape uniform across all apps and picks up the correct
    //    `Timestamp::now()` microsecond stamping — the old inline version
    //    stamped milliseconds into the microsecond `created_at` field.
    let payload = fauna_client_core::email::build_signed_email(
        &kp,
        &recipient_bytes,
        subject,
        body,
        node_url,
    )
    .map_err(|e| anyhow::anyhow!("build email payload: {e}"))?;

    // 4. Hand the tuple to our home nest over the bearer WS-RPC connection
    //    (`fauna.inbox.send`). `recipient_nest_url=None` ⇒ same-nest local
    //    delivery — faithful to the retired `POST /api/v1/inbox/{actor}` twin,
    //    which only ever reached recipients on this client's own home nest.
    //    Cross-nest (Some(peer)) awaits client-side peer discovery.
    fauna_client_inbox::InboxClient::new(nest_rpc)
        .send(recipient_actor_hex.to_string(), None, payload)
        .await
        .map_err(|e| anyhow::anyhow!("inbox send: {e}"))?;
    Ok(())
}

/// Split the guardian gate off every other bridge link / follow failure, on the
/// SHARED typed predicate (`RpcError::is_guardian_approval_required`) rather
/// than a string match — tui's `bridges::guardian_gate_or_failed`. `Some` only
/// for the typed refusal: offering the ask on a transport failure would tell an
/// unsupervised user their account is supervised (`family-safety.md`
/// § Feed-source approvals, rule (a) of the contacts half).
fn feed_source_refusal(
    e: &fauna_client::NestClientError,
    bridge_id: &str,
    operation: fauna_core::data::FeedSourceOperation,
    target: &str,
) -> Option<UiMessage> {
    match e {
        fauna_client::NestClientError::Rpc(err) if err.is_guardian_approval_required() => {
            Some(UiMessage::Data(DataMessage::FeedSourceRefused {
                bridge_id: bridge_id.to_string(),
                operation: operation.as_str().to_string(),
                target: target.to_string(),
            }))
        }
        _ => None,
    }
}

/// A landed contact ask's re-read of the ward's own asks, or the ask's error.
pub type ContactAskResult = Result<Vec<fauna_client_family::FamilyContactRequestInfo>, String>;

/// A landed feed-source ask's re-read of the ward's own asks, or the ask's error.
pub type FeedAskResult = Result<Vec<fauna_client_family::FamilyFeedRequestInfo>, String>;

/// `fauna.family.contact.request` + the status re-read. A failed re-read is not
/// a failed ask (the guardian has been rung), so it degrades to an empty list.
async fn ask_contact(nest: Arc<NestClient>, peer: String) -> ContactAskResult {
    let peer_bytes = fauna_core::hex32::decode(&peer).map_err(|e| format!("peer id: {e}"))?;
    let family = fauna_client_family::FamilyClient::new(nest);
    family
        .contact_request(peer_bytes.to_vec())
        .await
        .map_err(|e| e.to_string())?;
    Ok(family
        .status()
        .await
        .map(|s| s.contact_requests)
        .unwrap_or_default())
}

/// `fauna.family.feed_source.request` + the status re-read (same degrade rule).
async fn ask_feed_source(
    nest: Arc<NestClient>,
    bridge_id: String,
    operation: String,
    target: String,
    label: String,
) -> FeedAskResult {
    let family = fauna_client_family::FamilyClient::new(nest);
    family
        .feed_source_request(bridge_id, operation, target, label)
        .await
        .map_err(|e| e.to_string())?;
    Ok(family
        .status()
        .await
        .map(|s| s.feed_requests)
        .unwrap_or_default())
}

/// How a knock send ended — the one classification the contacts page and the
/// profile page both render from, so the guardian gate is told apart from every
/// other failure in exactly one place (tui's `contacts::KnockSend`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnockSend {
    Sent,
    /// The nest's typed guardian-approval refusal
    /// (`RpcError::is_guardian_approval_required`) — the one failure that
    /// reveals `contact-request-guardian-button`.
    RefusedByGuardian,
    Failed(String),
}

/// `build_knock_payload` → `fauna.inbox.send`, classified. The guardian gate is
/// separated from every other failure on the SHARED typed predicate rather than
/// a per-app string match — which handler refused is the nest's business.
async fn knock_classified(
    nest: Arc<NestClient>,
    node_url: &str,
    secret: [u8; 32],
    recipient: String,
    recipient_nest_url: Option<String>,
) -> KnockSend {
    let kp = ActorKeypair::from_secret(secret);
    let recipient_bytes = match fauna_core::hex32::decode(&recipient) {
        Ok(b) => b,
        Err(e) => return KnockSend::Failed(format!("recipient id: {e}")),
    };
    let payload =
        match fauna_client_core::email::build_knock_payload(&kp, &recipient_bytes, node_url) {
            Ok(p) => p,
            Err(e) => return KnockSend::Failed(format!("knock payload: {e}")),
        };
    match fauna_client_inbox::InboxClient::new(nest)
        .send(recipient, recipient_nest_url, payload)
        .await
    {
        Ok(_) => KnockSend::Sent,
        Err(fauna_client::NestClientError::Rpc(err)) if err.is_guardian_approval_required() => {
            KnockSend::RefusedByGuardian
        }
        Err(e) => KnockSend::Failed(format!("inbox send: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Contact / Knock / Notification WS-RPC reply → UI row mappers
// ---------------------------------------------------------------------------

/// Map `fauna.knocks.list` reply rows onto the UI `KnockRow` shape. `sender`
/// (hex actor id) becomes `peer_actor_id`; `summary` is wrapped (the UI row
/// carries it as `Option`); `created_at` (millis) is rendered as the
/// `timestamp` string (the legacy HTTP twin sent the timestamp as a string).
fn knock_rows_from_items(items: Vec<fauna_client_contacts::contacts::KnockItem>) -> Vec<KnockRow> {
    items
        .into_iter()
        .map(|item| KnockRow {
            peer_actor_id: item.sender,
            summary: Some(item.summary),
            timestamp: item.created_at.to_string(),
        })
        .collect()
}

/// Map `fauna.contacts.list` reply rows onto the UI `ContactRow` shape.
/// `peer_id` (hex actor id) becomes `peer_actor_id`. `handle`/`domain` are
/// carried through from the enriched reply (`Some` for a local peer with a
/// handle set, `None` for a federated peer — the roster filter matches over
/// `handle@domain@actor-id` uniformly via the shared predicate). `updated_at`
/// prefers `accepted_at`, falling back to `created_at`, rendered as a string
/// (the legacy HTTP twin sent a string).
fn contact_rows_from_items(
    items: Vec<fauna_client_contacts::contacts::ContactItem>,
) -> Vec<ContactRow> {
    items
        .into_iter()
        .map(|item| ContactRow {
            peer_actor_id: item.peer_id,
            peer_handle: item.handle,
            peer_domain: item.domain,
            status: item.status,
            updated_at: Some(item.accepted_at.unwrap_or(item.created_at).to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Feed / Post decode helpers
// ---------------------------------------------------------------------------

/// Format an epoch-**microsecond** timestamp as a relative-time display string,
/// delegating the bucket→i18n-key decision to the shared
/// [`fauna_core::format::relative_time`] via [`crate::i18n::relative_time`] — the
/// four "Nm/Nh/Nd ago" buckets are chosen once in shared Rust and resolved
/// through the linux i18n table, never hand-rolled here (priority #2/#3,
/// `docs/goal/behavior/value-formatting.md` § Relative time; web/android/windows
/// already migrated). `us == 0` is the "no timestamp" sentinel → empty string; a
/// timestamp `≥ 7 d` old renders a local date. Callers pass **microseconds**
/// (feed/search `created_at` are micros as-is; snapshots ×1e6 from secs, folders
/// ×1e3 from ms).
pub fn format_epoch_us(us: i64) -> String {
    if us == 0 {
        return String::new();
    }
    let then_ms = us / 1_000;
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    crate::i18n::relative_time(then_ms, now_ms)
}

/// The hash-less handle for a freshly picked attachment path: its `{name,
/// size}` from a `stat` — no read, so staging stays cheap. `media_type` stays
/// `None`; the seal's own answer sets it at submit, beside the hash
/// ([`seal_and_upload_attachment`]). Mirrors tui's `picked_handle`.
fn picked_handle(path: &str) -> Option<fauna_feed::AttachedFile> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    Some(fauna_feed::AttachedFile {
        name,
        size: meta.len(),
        blob_hash: None,
        media_type: None,
    })
}

/// Seal the picked file **for the composer's current audience**, upload it, and
/// return the resolved [`fauna_feed::AttachedFile`] the shared
/// `FeedManager::submit_post` stages into a `MediaItem`. The file is picked at
/// attach time and sealed + uploaded only when the user posts.
///
/// **The seal is resolved before the bytes are POSTed** (`ui/media.md`
/// § Encryption at rest). `seal_compose_attachment` reads the composer's
/// audience and seals accordingly — public composes still pass through as
/// plaintext `PublicPost` bytes, byte-identical to what
/// `fauna_client::upload_public_post_blob` produced before — so the caller must
/// have staged the gate/sale already. A tier's period key never leaves shared
/// Rust; this glue is pure transport, and rides the platform HTTP/bulk plane
/// rather than WS-RPC (`feed.md` § Where logic lives) — which is the only sense
/// in which it is "client glue", not a licence to hand-roll it per app.
async fn seal_and_upload_attachment(
    nest: &dyn crate::nest_content_api::NestContentApi,
    manager: &LinuxFeedManager,
    file_path: &str,
) -> Result<fauna_feed::AttachedFile, String> {
    let raw = std::fs::read(file_path).map_err(|e| format!("read {file_path}: {e}"))?;
    let size = raw.len() as u64;
    let name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let sealed = manager.seal_compose_attachment(raw).await?;
    let media_type = sealed.media_type.clone();
    let hash = fauna_client::upload_prepared_blob(
        nest,
        MultipartBlob {
            sidecar_cbor: sealed.primary.sidecar_cbor,
            bytes: sealed.primary.bytes,
        },
        sealed.thumbnail.map(|t| MultipartBlob {
            sidecar_cbor: t.sidecar_cbor,
            bytes: t.bytes,
        }),
    )
    .await?;

    Ok(fauna_feed::AttachedFile {
        name,
        size,
        blob_hash: Some(hash),
        // The sealed class's sidecar says `application/octet-stream`; the real
        // MIME rides inside the seal, so the `MediaItem` must take it from the
        // seal's own answer — never the sidecar, never a filename guess.
        media_type: Some(media_type),
    })
}

/// Worker-thread body for [`FaunaClient::save_snapshot_file`]: build a
/// throwaway restore engine on a fresh current-thread runtime (mirrors
/// `libs/fauna-ffi/src/snapshot_download.rs`'s worker), fetch the file's
/// plaintext bytes by manifest, then write them to `save_path`.
#[allow(clippy::too_many_arguments)]
fn download_snapshot_file_and_save(
    auth: Arc<AuthClient>,
    nest_rpc: Arc<NestClient>,
    device_id: [u8; 32],
    secret_hex: &str,
    manifest_hash: fauna_core::data::ContentHash,
    relative_path: &str,
    save_path: &str,
    context: String,
) -> UiMessage {
    // gtk-runtime-ok: this whole fn IS the worker-thread body — its only caller
    // spawns it on a dedicated `fauna-snapshot-download` OS thread (see
    // `save_snapshot_file`'s `thread::Builder::new().name(...)`). The spawn
    // lives in the caller, so the gate cannot see it from here.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return UiMessage::Action(ActionResult::Failed {
                context,
                error: format!("build snapshot-download runtime: {e}"),
            });
        }
    };
    let result = rt.block_on(async move {
        let engine = fauna_sync_engine::engine_lifecycle::build_restore_engine(
            auth, nest_rpc, device_id, secret_hex,
        );
        engine
            .download_file_bytes_by_manifest(manifest_hash, None, relative_path)
            .await
    });
    match result {
        Ok(bytes) => match std::fs::write(save_path, &bytes) {
            Ok(()) => UiMessage::Action(ActionResult::Success { context }),
            Err(e) => UiMessage::Action(ActionResult::Failed {
                context,
                error: format!("write {save_path}: {e}"),
            }),
        },
        Err(e) => UiMessage::Action(ActionResult::Failed {
            context,
            error: format!("download {relative_path:?}: {e:#}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Disk-backed nest-identity pin store (the TOFU `known_hosts` analogue)
//
// The transport-trust model (security.md § Transport trust) learns a
// `(nest host → nest_actor_id)` pin the first time it authenticates a
// self-signed / LAN nest, then refuses a *changed* identity on later connects.
// That pin must outlive the process or the user re-TOFUs every launch, so at
// startup — before the first authenticated connect — we install
// fauna-anon-client's `DiskPinStore`, replacing the in-memory default.
//
// The pin file is **non-secret** routing state (host → public actor_id, like
// `~/.ssh/known_hosts`), so it lives in the XDG *config* dir alongside the
// MLS / P2P / window-state stores — NOT in libsecret (that is for the identity
// secret) and NOT in `service_watcher::resolve_data_dir()` (that is the *nest*
// sidecar's data dir, which is `None` for a pure client, so pins would never
// persist on the common laptop deployment). Rooting it under the same
// `~/.config/fauna/` dir means the e2e driver's per-run `XDG_CONFIG_HOME`
// isolation covers it for free.
// ---------------------------------------------------------------------------

/// The fauna config dir (`$XDG_CONFIG_HOME/fauna` or `~/.config/fauna`),
/// matching the MLS / P2P / window-state resolution. `None` when neither var is
/// set (→ keep the in-memory pin store; pins won't survive restart, but nothing
/// breaks).
pub(crate) fn fauna_config_dir() -> Option<std::path::PathBuf> {
    fauna_core::platform_ids::xdg_app_config_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "fauna",
    )
}

/// Install the process-global `tracing` subscriber via the shared `fauna-log`
/// crate. Call **once**, as the very first thing in `main()` — before anything
/// emits a `tracing` event — so the whole session is captured. Installs the
/// in-memory ring buffer (read by the Settings → Logs sub-page) plus a
/// daily-rolling file under `<config>/fauna/logs/` plus stderr, filtered by
/// `RUST_LOG` (default `info`). Logs land under the same `~/.config/fauna` root
/// as the pin/MLS/window-state stores, so the e2e driver's per-run
/// `XDG_CONFIG_HOME` isolation covers them for free.
///
/// The writer guard is leaked (`mem::forget`) intentionally: it must live for
/// the whole process so the non-blocking file writer keeps flushing. A no-op
/// when the config dir can't be resolved (logs then go to stderr only, via the
/// fallback below).
pub fn install_logging() {
    let guard = match fauna_config_dir() {
        Some(dir) => fauna_log::init(&dir),
        None => {
            // No config dir → still install a stderr-only subscriber so events
            // aren't silently dropped. `init` needs a dir for the file layer,
            // so fall back to a temp dir; the ring + stderr still work.
            fauna_log::init(&std::env::temp_dir().join("fauna"))
        }
    };
    if let Some(guard) = guard {
        std::mem::forget(guard);
    }
    // First event into the ring — a lifecycle line so the Settings → Logs page
    // (observability.md § Surfaces) always has at least one entry, and each
    // launch stamps the on-disk file. No secrets / paths (redaction rule).
    tracing::info!(target: "fauna_linux", "fauna-linux client logging initialised");
}

/// Install the disk-backed nest-identity pin store at startup so TOFU pins
/// survive restarts. Call **once**, early (before the first authenticated
/// connect) — it replaces the process-global in-memory default (last write
/// wins). The canonical filename lives in `fauna_anon_client::cert_binding`
/// (via `DiskPinStore::open_in_dir`), shared with the UniFFI apps so the
/// on-disk layout is uniform.
///
/// The dir is the shared **install-scoped trust home**
/// (`cert_binding::install_scoped_trust_home` — `$XDG_CONFIG_HOME/fauna` here,
/// the same value [`fauna_config_dir`] resolves for the other stores): one
/// derivation for every writer (this app, tui) and consumer (the sync agent),
/// per `security.md` § Pin custody rule 1.
pub fn install_disk_pin_store() {
    let dir = fauna_client::cert_binding::install_scoped_trust_home();
    let _ = std::fs::create_dir_all(&dir);
    tracing::info!("nest-identity pin store under {}", dir.display());
    fauna_client::trust::install_pin_store(std::sync::Arc::new(
        fauna_client::cert_binding::DiskPinStore::open_in_dir(&dir),
    ));
}

// ---------------------------------------------------------------------------
// Credential storage via freedesktop Secret Service (D-Bus)
//
// Three separate libsecret items, matching the Apple KeychainStore layout
// (one keychain item per field with `kSecAttrAccount` = secret_key /
// device_id / node_url). Items share `application=fauna-desktop` and are
// distinguished by a per-field `account` attribute so partial writes
// don't have to read-modify-write a single blob — a missing or
// half-written field never causes the others to fail to deserialize.
//
// Migration: a legacy single-blob item written by the pre-split layout
// (matching `application=fauna-desktop` with no `account` attribute) is
// detected on load, parsed, copied into the three new items, and
// deleted. Idempotent — once migrated, subsequent loads only see the
// new layout.
// ---------------------------------------------------------------------------

// The libsecret `application` attribute that namespaces every Fauna keyring
// item. Defaults to `"fauna-desktop"` in production; the `FAUNA_KEYRING_APP`
// env var overrides it so concurrent E2E runs (which share the session's
// secret-service daemon — per-run `XDG_*` isolation does NOT cover it) get a
// private namespace and can't sweep each other's credentials. Every other
// Fauna namespace below (pending-invite, awaiting-DNS, …) derives from this
// base by suffix, so one env var isolates the whole keyring surface per run.
//
// Read fresh on each call (cheap relative to the D-Bus round-trip it precedes)
// rather than cached: prod sets the env once before launch, but the in-process
// isolation test switches it between logical "runs", which a process-wide cache
// would defeat.
// The credential primitives (attrs/file-store/keyring item ops) and the
// libsecret-backed `SecretStore` live in the shared `fauna-credential-store`
// crate since fauna-tui became the second freedesktop consumer; this module
// keeps the linux-only trio wrappers + resume slots built on them.
pub use fauna_credential_store::CredentialStore;

/// Linux's shared-store constructor: `FAUNA_KEYRING_APP` override or the
/// `fauna-desktop` namespace, file backend under `FAUNA_E2E_CREDENTIAL_DIR`.
pub fn secret_store() -> fauna_credential_store::CredentialStore {
    fauna_credential_store::CredentialStore::new("fauna-desktop")
}

fn cred_app() -> String {
    // Through the shared gated accessor, never a second `std::env::var` here:
    // a per-app copy of the read is exactly the divergence row 194 existed to
    // fix (priority #2), and an ungated copy would keep the env name in a
    // shipped `fauna-desktop` after the shared crate stopped naming it.
    fauna_credential_store::keyring_app_override().unwrap_or_else(|| "fauna-desktop".to_string())
}

// ---------------------------------------------------------------------------
// Env-gated file-backed credential store (e2e + keyring-unavailable fallback)
//
// When `FAUNA_E2E_CREDENTIAL_DIR` is set, the long-term identity trio
// (`store_credentials_partial` / `load_credentials`) routes to a 0600 JSON
// file under that directory instead of libsecret. Both motivations are
// downstream of the same fact — a headless box (or a hardened deployment) may
// have no *usable* Secret Service: its gnome-keyring default collection can be
// locked, so `create_item` / `get_secret` fail with
// `org.freedesktop.Secret.Error.IsLocked` even though `connect` succeeds.
//
//   1. E2E: the believable live-mail test drives the real onboarding UI, which
//      must persist the identity secret + nest_url + device_id and read them
//      back on the authenticated launch. The linux e2e driver sets this env
//      var to a per-run private dir so the round-trip survives a locked keyring
//      without disturbing the session-wide secret-service daemon that sibling
//      sessions share (cf. `FAUNA_KEYRING_APP`).
//   2. Robustness: a keyring-unavailable deployment gets a working
//      out-of-the-box login fallback instead of a silently dead session.
//
// Scope is deliberately the identity trio only — the believable flow never
// defers, so the awaiting-DNS / pending-encryption-mode resume slots and the
// best-effort handle/domain/tier UX cache stay on raw libsecret (no file-arm
// yet). The pending-invite slot is the
// exception: it lives on the shared `AccountRegistry` (`secret_store()`
// above → `fauna-credential-store::CredentialStore`), which honors
// `FAUNA_E2E_CREDENTIAL_DIR` independently — see the InviteSubmitted /
// LoggedIn arms of `views/onboarding/mod.rs::handle_wizard_done`. Those still
// on raw libsecret are all best-effort on the happy path (a locked-keyring
// write just logs and never blocks startup), so a locked keyring no longer
// breaks login once the trio is file-backed.
//
// Unset in production → libsecret is the sole backend; zero behavior change.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Server-data cache (handle / domain / tier)
//
// These are *not* part of the long-term identity contract — they're a UX
// cache populated from the registration response (and the silent-sign-in
// refresh) so the status bar / settings show the user's identity instantly on
// relaunch. The server is the source of truth. Since 2026-09-24 the cache IS
// the active account's index entry in the shared `AccountRegistry`
// (`AccountRegistry::update_cache` / `session_material`) — the same rows the
// account switcher lists — so `delete_credentials`' registry sweep takes it
// with the identity, and the pre-registry `handle` / `domain` / `tier`
// libsecret items are neither written nor read any more.
// ---------------------------------------------------------------------------

/// Persist the server-data cache fields (handle / domain / tier).
///
/// Each argument is `Option<&str>`; `None` means "leave the existing
/// item alone". Empty strings are also written as-is — pass `None` if
/// you want to preserve a previously-cached value rather than blank it.
pub fn store_account_cache(
    handle: Option<&str>,
    domain: Option<&str>,
    tier: Option<&str>,
) -> Result<(), anyhow::Error> {
    if handle.is_none() && domain.is_none() && tier.is_none() {
        return Ok(());
    }
    // Sync wrapper for non-async (GTK-thread) callers. The libsecret write runs
    // on a worker thread via `block_on_tokio`, so neither the round trip nor
    // `Runtime::drop` reaches the GTK main loop. Async callers (e.g.
    // `silent_sign_in`'s `self.runtime.spawn` task) should still await
    // `store_account_cache_async` directly rather than park a worker here — but
    // it is now a preference, not a panic: the runtime is no longer built on the
    // caller's thread, so calling this from inside a runtime is legal.
    let handle = handle.map(str::to_string);
    let domain = domain.map(str::to_string);
    let tier = tier.map(str::to_string);
    crate::async_helper::block_on_tokio(async move {
        store_account_cache_async(handle.as_deref(), domain.as_deref(), tier.as_deref()).await
    })
}

/// Async core of [`store_account_cache`] — write the server-data cache fields
/// to libsecret. Callable from any tokio context; the sync wrapper above is for
/// GTK-thread callers, async callers `.await` this so they never nest a runtime.
pub async fn store_account_cache_async(
    handle: Option<&str>,
    domain: Option<&str>,
    tier: Option<&str>,
) -> Result<(), anyhow::Error> {
    if handle.is_none() && domain.is_none() && tier.is_none() {
        return Ok(());
    }
    // Mirror into a process-local cache *first*, before the libsecret write.
    // libsecret is cross-restart persistence, but it can be **locked or
    // absent** (a headless/e2e session with no unlocked keyring, or a user
    // who hasn't unlocked their login keyring) — in which case `create_item`
    // below errors with `IsLocked` and nothing is persisted. The live session
    // still knows its own `<handle>@<domain>`, so keep it in-process so
    // `load_account_cache` can serve it regardless of the keyring's state.
    // Without this, a locked keyring leaves the conversations session's
    // self-address seed (and the CalDAV `self_email` read) unable to find the
    // account, so outbound mail refuses with no resolvable From. `Some("")`
    // handle is treated as "unset" (same semantics as the libsecret path below).
    {
        let mut mem = account_cache_mem().lock().unwrap();
        if let Some(h) = handle.filter(|h| !h.is_empty()) {
            mem.0 = Some(h.to_string());
        }
        if let Some(d) = domain {
            mem.1 = Some(d.to_string());
        }
        if let Some(t) = tier {
            mem.2 = Some(t.to_string());
        }
    }
    // The durable half: the ACTIVE account's index entry in the registry.
    // `update_cache` rewrites all three fields, so a `None` here is merged
    // with what the entry already holds — every caller's "leave the existing
    // value alone" contract. An empty handle means "no handle set" (verify /
    // account-fetch return `""` for an actor without one) and is never
    // cached: doing so would clobber a real handle written by another launch
    // path, and an absent cache entry already means "unknown". No active
    // account (a sign-out landed first) means nothing to write against.
    let registry = crate::account_registry();
    let Some(active) = registry.active() else {
        return Ok(());
    };
    let current = registry.session_material(&active);
    let merged = |fresh: Option<&str>, stored: Option<String>| {
        fresh
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .or(stored)
    };
    let handle = merged(handle, current.as_ref().and_then(|m| m.handle.clone()));
    let domain = merged(domain, current.as_ref().and_then(|m| m.domain.clone()));
    let tier = merged(tier, current.as_ref().and_then(|m| m.tier.clone()));
    registry.update_cache(
        &active,
        handle.as_deref(),
        domain.as_deref(),
        tier.as_deref(),
    )?;
    Ok(())
}

/// Load the server-data cache fields. Each `Option<String>` is `None`
/// when the corresponding item is absent (e.g. before first
/// registration). Caller decides what to display vs. leave blank.
/// Process-local mirror of the `(handle, domain, tier)` account cache. Written
/// by [`store_account_cache_async`] and read first by [`load_account_cache`], so
/// the live session's identity survives a locked/absent libsecret keyring (see
/// the comment in `store_account_cache_async`).
/// `(handle, domain, tier)`.
type AccountCacheFields = (Option<String>, Option<String>, Option<String>);

fn account_cache_mem() -> &'static std::sync::Mutex<AccountCacheFields> {
    static MEM: std::sync::OnceLock<std::sync::Mutex<AccountCacheFields>> =
        std::sync::OnceLock::new();
    MEM.get_or_init(|| std::sync::Mutex::new((None, None, None)))
}

/// Forget the in-process `(handle, domain, tier)` mirror — the account switch
/// calls this so the incoming account's readers fall through to ITS registry
/// entry instead of inheriting the outgoing session's `<handle>@<domain>`.
pub(crate) fn reset_account_cache_mem() {
    *account_cache_mem().lock().unwrap() = (None, None, None);
}

pub fn load_account_cache() -> (Option<String>, Option<String>, Option<String>) {
    // In-process cache first — it's the live session's source of truth and is
    // immune to a locked/absent keyring. If it already holds handle+domain
    // (everything any caller needs), skip the libsecret round-trip entirely.
    let (mh, md, mt) = { account_cache_mem().lock().unwrap().clone() };
    if mh.is_some() && md.is_some() {
        return (mh, md, mt);
    }
    // Fall back to the registry entry for any field the in-process cache lacks
    // (e.g. a freshly relaunched process before the first store_account_cache
    // call).
    let (kh, kd, kt) = load_account_cache_registry();
    (mh.or(kh), md.or(kd), mt.or(kt))
}

/// The durable half of [`load_account_cache`]: the ACTIVE account's cached
/// handle / domain / tier off its registry index entry. Synchronous — the
/// store's keyring arm already drives libsecret on its own OS thread, so this
/// is legal from a GTK handler and from a tokio worker alike.
fn load_account_cache_registry() -> (Option<String>, Option<String>, Option<String>) {
    let registry = crate::account_registry();
    let Some(active) = registry.active() else {
        return (None, None, None);
    };
    match registry.session_material(&active) {
        Some(m) => (m.handle, m.domain, m.tier),
        None => (None, None, None),
    }
}

// ---------------------------------------------------------------------------
// Multi-account SecretStore glue (Stage 1) — MOVED to the shared
// `fauna-credential-store` crate (with the credential primitives) when
// fauna-tui became the second freedesktop consumer. `client::secret_store()`
// above is linux's constructor; the logical-key mapping, backend selection,
// and sync/async threading notes live with the impl in that crate. See
// docs/goal/architecture/long-term-store.md § Multi-account evolution.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pending-encryption-mode slot — LEGACY, consume-only (2026-07-11).
//
// Two libsecret items (nest_url, handle) under a separate `application`

/// Delete all Fauna credentials from the system keyring, and report what
/// survived.
///
/// **Returns what survived rather than an error.** The erase is best-effort by
/// design — a sign-out completes either way — but a keyring that refused the
/// wipe must not look like one that took it. Until 2026-09-13 the sign-out
/// handler dropped this function's `Result` (`let _ =`) after the residue line
/// was already built, so a locked collection holding the identity seed painted a
/// clean "Signed out" (`account-scoping.md` § Erasure follows scope → *the
/// credential half is a residue class too*). The caller hands it to
/// `account_scope::record_residue`.
#[must_use = "a sign-out that drops this reports success over a keyring that may \
              still hold the signed-out identity - the defect this return exists \
              to prevent"]
pub fn delete_credentials() -> fauna_client_accounts::CredentialSweep {
    // Also clear the in-process account-cache mirror (see `account_cache_mem`):
    // logout/reset must not let the next signed-in actor inherit this session's
    // cached `<handle>@<domain>`.
    *account_cache_mem().lock().unwrap() = (None, None, None);
    // The per-actor sweep, then the `fauna-desktop` namespace wipe, then a
    // read-back — the one shared sequence (`long-term-store.md` § Cleanup
    // contract). The wipe alone cannot reach the shared `fauna-account-store`
    // namespace holding this machine's writer key and each account's principal
    // bundle, and the sweep must run first because it enumerates through the
    // registry index the wipe destroys.
    fauna_credential_store::erase_all_credentials(
        &crate::account_registry(),
        &CredentialStore::for_namespace(cred_app()),
    )
}

/// [`delete_credentials`] again for a sign-out residue retry, reading back the
/// keys the sign-out recorded as surviving — the shared
/// [`fauna_credential_store::re_erase_credentials`]. Called only when the
/// recorded credential half is not clean, and only once the retry's serving
/// gate has passed (`account_scope::retry_residue`).
pub fn re_delete_credentials(
    recorded: fauna_client_accounts::CredentialSweep,
) -> fauna_client_accounts::CredentialSweep {
    fauna_credential_store::re_erase_credentials(
        &crate::account_registry(),
        &CredentialStore::for_namespace(cred_app()),
        recorded,
    )
}

/// The namespace wipe alone, with the `application` namespace passed
/// explicitly — how the per-run isolation tests below exercise the real
/// secret-service round-trip env-free. Production goes through
/// [`delete_credentials`], whose shared sequence owns this wipe.
#[cfg(all(test, feature = "live-secret-service"))]
fn delete_credentials_in(app: &str) -> Result<(), anyhow::Error> {
    // The shared store owns both arms (file + libsecret) and the routing between
    // them: `fauna-tui`'s factory reset is the second consumer, so this is one
    // primitive, not two copies (priority #2). It also drives the libsecret sweep
    // on a dedicated OS thread, which the local `block_on` here did not — reaching
    // this from a tokio worker used to panic with "Cannot start a runtime from
    // within a runtime".
    CredentialStore::for_namespace(app).delete_namespace()
}

#[cfg(test)]
mod account_cache_runtime_tests {
    //! Regression for the keyring-read nested-runtime panic.
    //!
    //! `load_account_cache` is a sync fn reached from BOTH sync contexts
    //! (startup in `main.rs`, settings in `app.rs`) AND tokio worker threads
    //! (the CalDAV `self_email` path). Its
    //! `load_account_cache_registry` fallback used to build a current-thread
    //! runtime and `block_on` *on the caller's thread*, which panics with
    //! "Cannot start a runtime from within a runtime" whenever that thread is
    //! already driving one — observed at startup. The fix drives the libsecret
    //! read on a dedicated OS thread (never a tokio worker), so it is legal
    //! from any context.
    //!
    //! Unlike the libsecret round-trip tests below, this does NOT skip when
    //! D-Bus/secret-service is absent: the dedicated thread either connects or
    //! returns `None`, and the property under test is purely "no panic inside a
    //! runtime", which holds either way.
    use super::load_account_cache_registry;

    #[test]
    fn keyring_read_is_safe_inside_a_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build test runtime");
        // Pre-fix this panicked on the worker thread; post-fix it returns.
        rt.block_on(async {
            let _ = load_account_cache_registry();
        });
    }

    // `persistence_flag_read_is_safe_inside_a_runtime` — the same property,
    // hung on `load_persistence_flag` because it was the one member of the
    // `async_helper::block_on_tokio` family that took no arguments and wrote nothing, so the test
    // could run without touching the dev machine's real keyring — was
    // dropped along with `load_persistence_flag` itself in the
    // compat-remnant sweep (2026-09-27). No other zero-argument, non-writing
    // member of that family remains to re-host it on; the mechanism stays
    // guarded structurally by `gtk-thread-tokio-check`, which still fails
    // the merge if any site builds its own runtime again.
}

#[cfg(test)]
mod actor_id_memo_tests {
    //! The actor id is derived once per client, however often it is read.
    //!
    //! Pinned as a **call count**, not a duration: the defect this closes was
    //! an Ed25519 derivation per read, hot enough (the feed derives it per post
    //! card) to hold the GTK main loop open — but the contract that keeps it
    //! closed is "derive once", which is exact and machine-independent.
    use super::ActorIdMemo;
    use std::cell::Cell;

    #[test]
    fn the_actor_id_is_derived_once_however_often_it_is_read() {
        let memo = ActorIdMemo::default();
        let derivations = Cell::new(0);
        for _ in 0..5 {
            let got = memo.get(|| {
                derivations.set(derivations.get() + 1);
                Some("deadbeef".to_string())
            });
            assert_eq!(got.as_deref(), Some("deadbeef"), "every read answers");
        }
        assert_eq!(
            derivations.get(),
            1,
            "an accessor callers treat as a field read must not re-derive"
        );
    }

    #[test]
    fn a_secret_that_does_not_derive_is_remembered_as_absent() {
        // The `None` case must memoize too — otherwise a malformed secret
        // costs the same failed derivation on every read, which is the same
        // defect wearing a different answer.
        let memo = ActorIdMemo::default();
        let derivations = Cell::new(0);
        for _ in 0..3 {
            assert_eq!(
                memo.get(|| {
                    derivations.set(derivations.get() + 1);
                    None
                }),
                None
            );
        }
        assert_eq!(derivations.get(), 1);
    }
}

#[cfg(test)]
mod silent_sign_in_adapter_tests {
    //! `security.md` § Post-auth surfacing: of everything a background silent
    //! challenge can learn, **only the session-ending outcomes** escalate to
    //! the launch surface. That RULE now lives in shared Rust
    //! (`fauna_launch_machine::classify_silent_challenge`), and its pins moved
    //! with it — lifted from this module when tui became the second wired leg.
    //!
    //! What stays pinned HERE is the app-local adapter, because the shared
    //! tests structurally cannot see it: [`super::adapt_verdict`] is the one
    //! place linux could still fold the identity verdict into the `Err` arm and
    //! reintroduce the pre-2026-07-23 bug (a possible-MITM verdict that reads
    //! as a network blip at the call site) with every shared test still green.
    use super::{SilentSignIn, adapt_verdict};
    use fauna_launch_machine::SilentSignInVerdict;

    #[test]
    fn the_identity_verdict_survives_the_adapter_as_its_own_outcome() {
        assert!(
            matches!(
                adapt_verdict(SilentSignInVerdict::IdentityChanged),
                Ok(SilentSignIn::IdentityChanged)
            ),
            "a mid-session identity change must survive this adapter as its OWN \
             outcome — an Err here is indistinguishable from a network blip, \
             which is exactly how it used to be swallowed"
        );
    }

    #[test]
    fn a_swallowed_failure_becomes_the_error_path_callers_expect() {
        let err = adapt_verdict(SilentSignInVerdict::Failed {
            error: "connection reset".into(),
        });
        assert!(
            err.is_err(),
            "the shared swallowed-failure arm must land on this module's error \
             path — the callers log-and-drop it"
        );
    }

    #[test]
    fn success_and_not_registered_keep_their_meanings() {
        match adapt_verdict(SilentSignInVerdict::Refreshed {
            handle: "alice".into(),
            domain: "example.test".into(),
            tier: "free".into(),
        }) {
            Ok(SilentSignIn::Refreshed {
                handle,
                domain,
                tier,
            }) => assert_eq!(
                (handle.as_str(), domain.as_str(), tier.as_str()),
                ("alice", "example.test", "free")
            ),
            other => panic!("success must refresh the cache; got {:?}", other.is_ok()),
        }
        assert!(
            matches!(
                adapt_verdict(SilentSignInVerdict::NotRegistered),
                Ok(SilentSignIn::NotRegistered)
            ),
            "not-registered keeps its own verdict — the one a signed-in session escalates"
        );
    }

    /// A supervisor that stopped because the nest refused its post-4401
    /// re-mint — the user suspended mid-session — escalates to the launch
    /// surface; an ordinary stop stays the connection indicator's.
    #[test]
    fn a_stopped_supervisors_session_ending_reason_escalates_and_nothing_else_does() {
        use crate::app::DataMessage;
        use fauna_client::NestClientError as E;
        assert!(matches!(
            super::session_ending_escalation(&E::Rpc(fauna_protocol::RpcError::not_registered())),
            Some(DataMessage::SignInRefused)
        ));
        for ordinary in [
            E::RpcDisconnected {
                was_in_flight: false,
            },
            E::SubprotocolMismatch,
            E::Auth("(401) fauna.auth.signature_failed".into()),
        ] {
            assert!(super::session_ending_escalation(&ordinary).is_none());
        }
    }
}

#[cfg(test)]
mod keyring_namespace_tests {
    //! The per-run `FAUNA_KEYRING_APP` namespace that lets concurrent E2E runs
    //! share one secret-service daemon without sweeping each other's
    //! credentials. The session-service keyring is NOT covered by the bridge's
    //! per-run `XDG_*` isolation, so every Fauna keyring item is namespaced by
    //! an `application` attribute derived from this env var — one value
    //! isolates the whole keyring surface (identity) per run. Both
    //! wizard-resume slots — pending-invite and awaiting-manual-DNS — moved
    //! to the shared `AccountRegistry` (see
    //! `views/onboarding/mod.rs::handle_wizard_done`) and namespace themselves
    //! independently via `secret_store()`'s `FAUNA_KEYRING_APP`-derived base.
    use super::cred_app;

    /// The prod default namespace when `FAUNA_KEYRING_APP` is absent (the
    /// normal `cargo test` env). Env-free otherwise.
    #[test]
    fn namespaces_derive_from_base() {
        let base = cred_app();

        if std::env::var_os("FAUNA_KEYRING_APP").is_none() {
            assert_eq!(base, "fauna-desktop", "prod default namespace unchanged");
        }
    }
}

/// The real-keyring half of the namespace isolation above — opt-in, because it
/// round-trips real items through the session's Secret Service, which an
/// ordinary test build must never reach (`keyring_guard_tests` below).
#[cfg(all(test, feature = "live-secret-service"))]
mod live_keyring_namespace_tests {
    use super::{CredentialStore, delete_credentials_in};
    use fauna_client_accounts::SecretStore as _;

    /// Two concurrent runs (distinct `application` namespaces) don't clobber
    /// each other: creds stored under run A survive run B wiping ITS own
    /// namespace at launch — the per-run keyring isolation this fix exists for.
    /// Exercises the real secret-service round-trip via the `*_credentials_in`
    /// seam, so no `FAUNA_KEYRING_APP` mutation (and no env race) is needed.
    /// Skipped when libsecret / D-Bus is unavailable (e.g. CI without the
    /// secret-service daemon).
    #[test]
    fn distinct_namespaces_do_not_clobber() {
        if !fauna_credential_store::keyring_probe() {
            eprintln!(
                "[live_keyring_namespace_tests] libsecret not available; skipping \
                 isolation round-trip. Run with a session bus + \
                 gnome-keyring/seahorse to exercise the real keyring."
            );
            return;
        }
        // Namespaces unique to this process and disjoint from the default
        // `fauna-desktop` namespace the other keyring tests use, so this test
        // is safe to run in parallel with them.
        let run_a = format!("fauna-desktop-e2e-a-{}", std::process::id());
        let run_b = format!("fauna-desktop-e2e-b-{}", std::process::id());

        // Pre-clean in case a prior failed run leaked state.
        let _ = delete_credentials_in(&run_a);
        let _ = delete_credentials_in(&run_b);

        // Run A stores an identity slot — the same store seam the registry
        // writes through.
        let store_a = CredentialStore::for_namespace(run_a.clone());
        let store_b = CredentialStore::for_namespace(run_b.clone());
        store_a.set("fauna/aa/secret", "aa");

        // Run B launches and wipes ITS namespace — must not touch run A.
        delete_credentials_in(&run_b).expect("delete under run B must succeed");
        assert!(
            store_b.get("fauna/aa/secret").is_none(),
            "run B namespace must be empty"
        );

        // Run A's slot survived run B's wipe — the property under test.
        assert_eq!(
            store_a.get("fauna/aa/secret").as_deref(),
            Some("aa"),
            "run A creds must survive run B's wipe"
        );

        // Cleanup.
        delete_credentials_in(&run_a).expect("cleanup run A must succeed");
        assert!(
            store_a.get("fauna/aa/secret").is_none(),
            "run A namespace must be empty after cleanup"
        );
    }
}

/// A test build of this app never reaches the session's live keyring
/// (`fauna_credential_store::live_keyring_allowed`): a unit-test binary talking
/// to the developer's `gnome-keyring-daemon` has crashed it, and every crash
/// re-locks the login keyring for every process on the box. The opt-in
/// `live-secret-service` suite re-allows it, so the guard stands down there.
#[cfg(all(test, not(feature = "live-secret-service")))]
mod keyring_guard_tests {
    use fauna_client_accounts::SecretStore as _;

    #[test]
    fn a_test_build_never_reaches_the_live_keyring() {
        use fauna_credential_store as cs;
        assert!(
            cs::keyring_app_override().is_none(),
            "a unit test never runs under a harness keyring namespace"
        );
        assert!(
            !cs::live_keyring_allowed(),
            "the dev-dependency feature `no-live-keyring` is missing"
        );
        assert!(
            !cs::keyring_probe(),
            "a test build reports no usable keyring"
        );

        // The app's own env-routed constructor resolves the keyring arm here
        // (no e2e credential dir in a unit test) — and still touches nothing.
        let store = cs::CredentialStore::for_namespace(format!(
            "fauna-keyring-guard-{}",
            std::process::id()
        ));
        assert_eq!(
            store.file_backend_dir(),
            None,
            "the keyring arm, not the e2e file arm"
        );
        store.set("fauna/guard", "x");
        assert_eq!(store.get("fauna/guard"), None);
        store.delete("fauna/guard");
        assert_eq!(
            cs::live_keyring_calls(),
            0,
            "no call reached a native keyring arm"
        );
    }
}

// `launch_challenge_classification_tests` and `build_signed_auth_body_tests`
// removed — exercised `classify_silent_error`, `LaunchChallengeOutcome`,
// and `build_signed_auth_body`, all gone with the LaunchMachine migration.
// (Main's separate cleanup commit independently dropped the
// classification module; this commit's wider migration also drops the
// build_signed_auth_body module since the underlying function is gone.)
// Equivalent coverage now lives in:
// - `libs/fauna-launch-machine/tests/silent_challenge.rs` (challenge/verify
//   wire format, transient/decommissioned/unregistered classification)
// - `libs/fauna-launch-machine/tests/token_refresh.rs` (the refresh's state
//   transitions; the refresh is the silent challenge too since 2026-09-21)
// - `nest_content_api` module — the bearer-authed REST chokepoint that
//   replaced the per-call `ensure_token` + `bearer_auth` + status-match
//   pattern; carries the 401-reactive refresh and is wiremock-tested in
//   `nest_content_api::reqwest_impl`'s `#[cfg(test)]` module.
