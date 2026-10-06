//! HTTP and WebSocket route handlers for the Node.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use futures_util::{SinkExt, StreamExt};

use fauna_core::data::{
    ContactRequest, ContactStatus, Post, ReachVerdict, SupervisedReach, supervised_reach_verdict,
};
use fauna_core::encoding::canonical_decode;

use crate::api_error::ApiError;
use crate::db::CacheDb;
use crate::ws::{FatalCloseReason, WsState};

/// Upper bound on a single inbound WebSocket message/frame on the client-facing
/// WS-RPC endpoints. The WS-RPC protocol caps a frame at 1 MiB (`transport.md`
/// § Wire format); this is the coarse transport-layer outer bound that stops an
/// (anonymous) peer from making nest buffer axum's 64 MiB default per frame
/// before the protocol decoder runs. 2 MiB
/// leaves headroom over the 1 MiB protocol cap (enforced separately at decode),
/// so it can never reject a legitimate frame yet still cuts the amplification
/// ceiling 32×.
///
/// Single-sourced from [`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`] so the
/// native clients (`fauna-client`, `fauna-anon-client`) cap their tungstenite
/// `WebSocketConfig` at the *same* value — symmetric inbound limit, review
/// finding F-CL1.
pub(crate) const MAX_WS_MESSAGE_SIZE: usize = fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE;

/// Events sent from route handlers to the discovery polling task.
#[derive(Debug)]
pub enum FeedEvent {
    Created {
        feed_id: String,
    },
    Deleted {
        feed_id: String,
    },
    Updated {
        feed_id: String,
    },
    ContributorAdded {
        feed_id: String,
        nest_url: String,
        author_id: Option<Vec<u8>>,
    },
    ContributorRemoved {
        feed_id: String,
        nest_url: String,
        author_id: Option<Vec<u8>>,
    },
}

/// One pending `publish_spam_baseline` drain run: the holder it was poked to,
/// and the channel that holder's merged half comes back through.
pub struct SpamBaselineRun {
    /// The aggregation holder this run belongs to — the same identity
    /// `resolve_content_processor_holder_seal_target` sealed the contributor
    /// copies to, and the only enrolled service user that can serve the run.
    /// Bound here because it is *not* the only holder that can reach the submit
    /// kind: a standard box also runs an MDA, which holds no reaching
    /// `content.read{spam-model}` grant and would submit an empty half — first
    /// submit wins the oneshot, so an unbound run is one a grant-less holder can
    /// take from under the real one.
    pub holder: [u8; 32],
    /// Resolved by `fauna.capabilities.submit_spam_baseline` when `holder`
    /// submits; the publish handler awaits it with a bounded timeout.
    pub tx: tokio::sync::oneshot::Sender<SpamBaselineSubmission>,
}

/// The granted holder's merged half of one `publish_spam_baseline` drain run
/// (`mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-13), sent
/// from the `fauna.capabilities.submit_spam_baseline` handler to the awaiting
/// publish handler through the [`AppState::spam_baseline_runs`] oneshot.
pub struct SpamBaselineSubmission {
    /// Plaintext `SpamModel` bytes of the holder's off-box additive merge —
    /// empty ⇒ nothing merged this run.
    pub merged_model: Vec<u8>,
    /// Contributor copies merged into `merged_model` (clamped nest-side to
    /// the sealed-candidate population before counting toward the floor).
    pub contributors: u32,
    /// Served copies the holder could not unseal / decode (advisory).
    pub unreadable: u32,
    /// The contributors the holder names as merged (well-formed 32-byte ids
    /// only; the publish run intersects them with its sealed candidates).
    /// Empty ⇒ nothing is counted and the merged half is not folded
    /// (`mail-spam.md` § Cold start Path 2 → *A contributor's departure
    /// withdraws the baseline*).
    pub merged_contributors: Vec<[u8; 32]>,
}

/// Shared application state.
pub struct AppState {
    // ── Core ─────────────────────────────────────────────────────────
    pub db: Arc<CacheDb>,
    pub ws: Arc<WsState>,
    /// In-memory push subscription registry for IDLE/NOTIFY
    /// (`fauna.bridges.subscribe_mailbox_state`). Per-WS-connection-
    /// lifetime, not persisted; emission hooks in
    /// `bridge_imap_handlers.rs` walk this after each state-mutating
    /// SQLite commit. Per I5 Phase F.1.
    pub bridge_push_registry: Arc<crate::bridge_push_registry::BridgePushRegistry>,
    /// In-memory advisory task-delegation lease store (`fauna.delegation.*`).
    /// Per-actor, never persisted — a nest restart drops all leases and clients
    /// re-acquire (participants.md § Coordination primitive; the lease is live
    /// state, not user data).
    pub delegation_leases: Arc<crate::delegation_registry::LeaseRegistry>,
    /// Wakes the nest's own lease runner (`delegation_runner::run`) for an
    /// immediate pass — poked by the `fauna.capabilities.{mint,renew,revoke}`
    /// handlers so a fresh grant is claimed (and a revoke released) without
    /// waiting out a heartbeat period.
    pub delegation_runner_wake: Arc<tokio::sync::Notify>,
    /// Per-owner health of this nest's own segment-backup passes — the progress
    /// half of the `backup-upload` lease's sufficiency predicate, so a nest that
    /// fails every pass hands the kind back to clients instead of heartbeating
    /// forever (`crate::segment_backup::BackupPassHealth`). **Ephemeral**, like
    /// the lease blackboard it feeds: a restart forgets, re-claims, and one
    /// failed sweep re-establishes the truth.
    pub backup_pass_health: Arc<crate::segment_backup::BackupPassHealth>,
    /// Pending `publish_spam_baseline` drain runs, keyed by 16-byte `run_id`
    /// (`mail-spam.md` § Encrypted-mode interaction, ratified 2026-07-13).
    /// The publish handler resolves the box's aggregation holder, inserts a run
    /// bound to it, pokes *that* holder alone
    /// (`PushEvent::BridgeSpamBaselinePublish`), and awaits its
    /// `fauna.capabilities.submit_spam_baseline` with a bounded timeout; the
    /// submit handler removes the entry — only for the bound holder — and sends
    /// the merged half through. **Ephemeral** — never persisted; a nest restart
    /// drops pending runs (the admin's synchronous publish call died with the
    /// process anyway).
    pub spam_baseline_runs: tokio::sync::Mutex<std::collections::HashMap<Vec<u8>, SpamBaselineRun>>,
    /// Throttle for `fauna.federation.succession.push`-hint-triggered verifies
    /// (per-identity cooldown + concurrent-dial cap). The push is a *hint*: the
    /// verify it wakes dials this nest's own recorded anchor for the identity,
    /// never anything the pusher supplied (`succession_pull` module docs, the
    /// anchor rule). **Ephemeral** — an empty table after restart only means
    /// the first hint per identity is admitted again.
    pub succession_hints: crate::succession_pull::SuccessionHintState,
    /// The authorization server's runtime memory and its one outbound seam —
    /// the PAR store, the DPoP nonce minter, the replay sets, the
    /// client-metadata cache and the guarded fetcher
    /// (`crate::oauth_as_state::OAuthAsRuntime`). **Ephemeral by design**, and
    /// that is `authorization-server.md` § As built's ruling rather than an
    /// omission: a restart mid-flow fails that flow cleanly and the user
    /// retries, so there is no interior state a crash can strand.
    pub oauth_as: Arc<crate::oauth_as_state::OAuthAsRuntime>,
    /// The nest-hosted WASM plugins (`third-party.md` § Execution forms →
    /// *WASM components*): the running supervisors and the install cards
    /// awaiting approval. In memory only — the rows and the files on disk are
    /// the durable half, and the boot walk restarts from them.
    pub plugins: Arc<crate::plugin_runner::PluginRunner>,
    /// Per-(route, source) request budgets for the OAuth endpoints
    /// (`crate::oauth_as_rate_limit`). Ephemeral for the same reason every
    /// abuse bound in the nest is: an empty table after a restart only means
    /// the first request per source is admitted again.
    pub oauth_limiter: Arc<crate::oauth_as_rate_limit::EndpointLimiter>,
    pub rpc_router: Arc<crate::rpc_router::RpcRouter>,
    /// Serving table for the `fauna.federation.*` kinds a verified peer nest may
    /// invoke over the long-lived federation channel (Spec Y2 slice 4 §4.C). Its
    /// registered kinds ARE the federation kind allowlist — the structural authz
    /// boundary (`federation_router::FederationRouter`). The peer-symmetric
    /// counterpart of `rpc_router`.
    pub federation_router: Arc<crate::federation_router::FederationRouter>,
    /// Pool of live, peer-symmetric nest↔nest federation channels, keyed by peer
    /// `nest_id` (Spec Y2 slice 4 §4.D, `federation_pool::FederationChannelPool`).
    /// Nest-side originators (the KP-fetch/Welcome relay, nest-sync worker, feed
    /// poller, remote-RSVP) dial/reuse a channel here; a peer that doesn't offer
    /// the channel is a hard `PoolError::Unsupported` (the HTTP interim was
    /// retired in Spec Y2 slice 5).
    pub federation_pool: Arc<crate::federation_pool::FederationChannelPool>,
    pub config: Arc<crate::config::NestConfig>,
    pub nest_identity: Arc<crate::nest_identity::NestIdentity>,
    /// Ed25519 signing key loaded from the nest_keypair table at startup.
    pub nest_signing_key: Option<ed25519_dalek::SigningKey>,
    /// Provider of the SPKI fingerprint of the cert this nest's WS-RPC listener
    /// is currently serving — the live source for the `fauna.auth.handshake`
    /// TLS channel-binding leg (`docs/goal/architecture/security.md`
    /// § Transport trust). The real impl is the `MultiDomainCertResolver` behind
    /// the TLS listener (reporting its **default**/apex cert's SPKI — never a
    /// per-custom-domain cert); `None` on a plain-HTTP dev nest (the handshake
    /// then omits `cert_binding`).
    pub served_cert_spki: Option<Arc<dyn crate::acme::ServedCertSpki>>,
    /// Wake handle for the ACME cert-lifecycle task
    /// ([`acme_http01::cert_lifecycle_task`](crate::acme_http01::cert_lifecycle_task)).
    /// An admin's `fauna.bridges.restore_real_tls_cert` notifies it so the task
    /// re-evaluates issuance *now* instead of waiting out its steady poll — the
    /// "retry issuance now" accelerator for the self-heal (e.g. right after port
    /// 80 opens). The task still gates each attempt on the failed-validation
    /// budget, so a wake **never** fires an attempt that would blow Let's
    /// Encrypt's rate limit (`docs/goal/behavior/mail-bridge-lifecycle.md`
    /// § Self-healing). Always present; cheap to create, no-op when nothing waits.
    pub acme_retry_notify: Arc<tokio::sync::Notify>,
    /// Bumped whenever the certificate on disk changes (`acme::cert_watcher_task`,
    /// the one place every change — an ACME issue or renewal, a self-signed
    /// provision, a client-issued install — passes through). The relay sidecar's
    /// channel watches it and tells the relay to fetch its cert again
    /// (`sidecar_channel::serve_relay_channel`, `tls-certificates.md` § Keeping
    /// the cert alive). A `watch`, not a `Notify`: a channel busy answering a
    /// request when the bump lands still sees it afterwards.
    pub relay_cert_changed: tokio::sync::watch::Sender<u64>,
    /// How many relay sidecars hold an authenticated channel to this nest right
    /// now (`sidecar_channel::serve_relay_channel` counts itself in and out) —
    /// zero or one in the image, always zero where no relay binary runs beside
    /// the nest. This is the whole of "does this deployment run its relay": no
    /// flag, no file, nothing a person or an RPC can set (`p2p.md` § The relay).
    pub relay_channels: Arc<std::sync::atomic::AtomicUsize>,
    /// Cancellation scope of THIS serving generation (`box-recovery.md`
    /// § Deployment-seed rotation → *Adoption by the running process*): every
    /// long-lived task of the generation — workers, sweepers, in-flight
    /// connection tasks — is spawned through [`AppState::spawn_scoped`] /
    /// [`AppState::scope_handle`] so a generation teardown (deployment-seed
    /// rotation, admin serving-port change) cancels and awaits ALL of them
    /// before `start_server` is re-entered. A long-lived spawn that bypasses
    /// the scope is a defect: an old-generation task would keep signing with
    /// material the rotation just superseded.
    pub serve_generation: tokio_util::sync::CancellationToken,
    /// Tracker paired with `serve_generation` — teardown closes it and awaits
    /// (bounded) so no old-generation task overlaps the successor generation.
    pub serve_tasks: tokio_util::task::TaskTracker,
    /// Edge signal "tear down this serving generation and re-enter
    /// `start_server`". Fired by the deployment-seed rotate handler after its
    /// reply flushes; awaited by every serve loop (`main.rs` and
    /// `desktop_serve::run_serve_loop`).
    pub serve_restart: Arc<tokio::sync::Notify>,
    pub http_client: reqwest::Client,
    pub tls_enabled: bool,
    /// Client-set CORS allow-list — the trusted browser origins for nest's own
    /// HTTP API. Boot-resolved from the `nest_cors_origins` DB row when present,
    /// else the `config.nest.cors_origins` seed; the admin RPC
    /// (`fauna.admin.set_cors_origins` / `apply_cors_origins_change`) swaps it
    /// live. **`ArcSwap`, not the `tokio::sync::RwLock` the sibling bool/int
    /// policy knobs use**, because the only reader is the `AllowOrigin::predicate`
    /// closure in the boot-built CORS layer — a **sync** tower-middleware context
    /// that cannot `.read().await`. ArcSwap gives the predicate a lock-free
    /// per-request `.load()`; the apply path `.store()`s the new list. Empty ⇒ the
    /// built-in `DEFAULT_CORS_ORIGIN` only (`node_policy_core::origin_allowed`).
    pub cors_origins: Arc<arc_swap::ArcSwap<Vec<String>>>,
    /// The admin's web-app origin choice — what this nest's reserved `/app`
    /// answers (`web-content-hosting.md` § Same-origin security model → *The
    /// nest-served `/app/` and the central origin*). Boot-resolved from the
    /// `nest_web_app_origin` row (absent ⇒ bundled); swapped live by
    /// `fauna.admin.web_app_origin.set` (`web_app_origin::apply_web_app_origin_change`).
    /// `ArcSwap` for the same reason as `cors_origins`: the reader is the `/app`
    /// router's per-request probe.
    pub web_app_origin: Arc<arc_swap::ArcSwap<fauna_protocol::web_app_origin::WebAppOrigin>>,
    /// Sync **cache** of the deployment's identity domain — a projection of the
    /// primary `mail_domains` row, which IS the nest's identity
    /// (`docs/goal/behavior/dns-management.md`); there is no separate identity
    /// store. `None` on a fresh, un-claimed / domainless (or local/IP) box. Read at
    /// **top precedence** by the sync `handle_domain()` / `web_serving_domain()`
    /// accessors (`state.rs`) — an `ArcSwapOption` (not the sibling `RwLock` knobs)
    /// precisely because those accessors are sync and cannot `.read().await`. Loaded
    /// at boot by `identity_domain_core::resolve_identity_domain` (reads the primary)
    /// and refreshed by `identity_domain_core::apply_primary_identity` whenever a
    /// claim / add-domain sets the primary. Cannot drift from the row — it is never
    /// independently authored. Design:
    /// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Claim.
    pub identity_domain: Arc<arc_swap::ArcSwapOption<String>>,
    /// The ACME/self-signed material directory (`{data-dir}/acme` in prod). Held
    /// first-class so the claim write-path can force-re-synthesize the self-signed
    /// floor to cover a claimed-post-boot domain (`self_signed_cert::resynthesize_floor_for_domain`)
    /// without threading the path through the storage impl.
    pub acme_dir: std::path::PathBuf,
    pub backup_service: Option<Arc<crate::backup::service::BackupService>>,
    pub payload_store: Option<Arc<crate::payload_store::PayloadStore>>,
    /// Per-actor mail segment store (Plan 2). The kind-agnostic
    /// `SegmentManager` registered with `kind = "mail"`: owns the append-only
    /// segment files + `manifest.mail` under `{data-dir}/__mail/{actor}/`. The
    /// mail-specific append/read/compact coordination (including the
    /// `segment_records` SQLite mirror) lives in `crate::segments::mail` free
    /// functions that take this manager + `CacheDb`.
    pub mail_segments: Arc<fauna_segment_store::SegmentManager>,
    /// Per-channel conversation segment store (Plan 7). The kind-agnostic
    /// `SegmentManager` registered with `kind = "conv"`: owns the append-only
    /// segment files + `manifest.conv` under `{data-dir}/__conv/{channel_id}/`.
    /// Conv's audience-scope is the channel_id (the MLS group), not an actor.
    /// The conv-specific append/read/tombstone coordination (incl. the
    /// `segment_records` SQLite mirror + per-channel seq allocation) lives in
    /// `crate::segments::conv` free functions over this manager.
    pub conv_segments: Arc<fauna_segment_store::SegmentManager>,
    /// Per-author post segment store (Track C posts). The kind-agnostic
    /// `SegmentManager` registered with `kind = "post"`: owns the append-only
    /// segment files + `manifest.post` under `{data-dir}/__post/{author}/`.
    /// Posts are point-read-by-CID (the record CID's digest is the `post_id`);
    /// the append/read/tombstone/compact coordination over this manager lives
    /// in `crate::segments::post` free functions. The cross-actor feed *list*
    /// runs off the SQLite projection, never these segments (`feed.md` § The
    /// read model).
    pub post_segments: Arc<fauna_segment_store::SegmentManager>,
    /// Per-actor CONTENT segment store for CalDAV event bodies —
    /// `__calendar/<actor_hex>/` (S6.4). Distinct from `cal_placement` below:
    /// this holds the sealed bodies, that holds the placement journal. The
    /// coordination lives in `crate::segments::cal` free functions.
    pub cal_segments: Arc<fauna_segment_store::SegmentManager>,
    /// Per-actor CONTENT segment store for CardDAV vCard bodies —
    /// `__card/<actor_hex>/` (S6.5). Twin of `cal_segments`; distinct from the
    /// `card_placement` journal. Coordination in `crate::segments::card`.
    pub card_segments: Arc<fauna_segment_store::SegmentManager>,
    /// Per-actor placement-journal manager for `__mail-placement/<actor>/`.
    /// IMAP write RPCs append placement events here atomically with their
    /// SQLite mutations. Spec § D6 (ε).
    pub mail_placement: Arc<crate::segments::MailPlacementSegmentManager>,
    /// Per-actor placement-journal manager for `__calendar-placement/<actor>/`.
    /// CalDAV write RPCs append placement events here atomically with
    /// their SQLite mutations. Spec § D2 (record schemas) / § D3
    /// (manifest holds compacted current state).
    pub cal_placement: Arc<crate::segments::CalPlacementSegmentManager>,
    /// Per-process CardDAV placement-journal coordinator (the
    /// `cal_placement` twin). Card placement events live under
    /// `{data-dir}/segments/__card-placement/`. Phase-3 S6.
    pub card_placement: Arc<crate::segments::CardPlacementSegmentManager>,
    pub feed_event_tx: tokio::sync::mpsc::Sender<FeedEvent>,
    /// Local-aggregate transition signal for the federation exchange
    /// originator (`exchange_originator` module): a monotonic counter bumped
    /// (`send_modify`) whenever a LOCAL exportable aggregate may have changed —
    /// a report capture/withdrawal, a report-share opt-out sweep. The worker
    /// debounces and pushes to peers.
    /// Watch (not mpsc): only "something changed since last look" matters.
    pub exchange_transition_tx: tokio::sync::watch::Sender<u64>,
    /// Security event notifier (inbox + best-effort push/email).
    pub security_notifier: Arc<crate::security_notify::SecurityNotifier>,
    /// Watch channel for update status (None until first check completes).
    pub update_status: tokio::sync::watch::Receiver<Option<fauna_update::UpdateStatus>>,

    // ── Domain sub-structs ───────────────────────────────────────────
    pub auth: crate::state::AuthState,
    pub sync: crate::state::SyncState,
    pub email: crate::state::EmailState,
    pub bridge: crate::state::BridgeState,
    pub mls: crate::state::MlsState,
    #[cfg(feature = "bluesky")]
    pub bluesky: crate::state::BlueskyState,
    #[cfg(feature = "nostr")]
    pub nostr: crate::state::NostrState,
    #[cfg(feature = "activitypub")]
    pub activitypub: crate::state::ActivityPubState,
    /// Web content rendering service (None if web hosting is not configured).
    pub web_content_service: Option<Arc<crate::web_content::service::WebContentService>>,
    /// Host-header resolver for web content domains (None if web hosting is not configured).
    pub host_resolver: Option<Arc<crate::web_content::serve::HostResolver>>,
    /// The web-serve component's capability-holder identity + live grant
    /// registry (web paywall, Pillar 2). `None` when web hosting is off, the
    /// data dir is unavailable, or an admin revoked the holder's enrollment —
    /// paywalled serving then darkens to teasers; ungated serving is unaffected.
    pub web_serve_holder: Option<Arc<crate::web_content::holder::WebServeHolder>>,
    /// Web push notification service (None if no VAPID key is configured).
    pub push_service: Option<Arc<crate::push::PushService>>,
    pub services_json_path: std::path::PathBuf,
    /// Root directory for the custodian-nest runtime's per-owner custodied
    /// stores (`{data-dir}/custody-hosting` in prod; the custody-hosting
    /// pump's pull leg writes keyless relay rows + adopted segments under
    /// `<root>/<host_hex>/<owner_hex>/`). `None` disables the pump — the
    /// `for_test` default; test rigs point it at a tempdir.
    pub custody_hosting_root: Option<std::path::PathBuf>,
    /// Sidecar bearer tokens: token string → granted scopes.
    pub sidecar_tokens: std::collections::HashMap<String, Vec<crate::sidecar_tokens::SidecarScope>>,
    /// Sliding-window rate limiter for per-credential bridge blob-fetch RPCs.
    pub bridge_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Per-actor throttle on the post training correction
    /// (`fauna.moderation.train`), keyed by the authenticated caller (bucket
    /// key `(actor, actor, "<method>")`), so one authenticated user can't pin
    /// the post-read path. Config in
    /// `bridge_rate_limit::SPAM_TRAIN_LIMITER_CONFIG`.
    pub spam_train_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Per-(actor, channel) throttle on a non-claimant rostered member's MLS
    /// Commit into a claimed folder channel (`federation.md` § Cross-nest
    /// shared folders + channel append, residual (a); bucket key `(actor,
    /// channel, "commit")`). Checked in `conversations_handlers::
    /// channel_send_core`, which the federation `channel.append` relay shares
    /// — the claimant is exempt and every other channel shape is untouched.
    /// Config in `bridge_rate_limit::CHANNEL_COMMIT_LIMITER_CONFIG`.
    pub channel_commit_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Per-principal throttle on the HTTP record door
    /// (`crate::records_door`). Config in
    /// `bridge_rate_limit::RECORDS_DOOR_LIMITER_CONFIG`.
    pub records_door_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// The live per-source-IP TLS-connection cap `serve_tls` enforces. Shared
    /// between the accept loop (constructed by [`crate::start_server`], which
    /// hands a clone to `serve_tls`) and the `fauna.transport.put_policy` Admin
    /// handler, which calls `set_max` so a cap change binds the live listener
    /// without a nest restart (the hot-reload that brings the nest-TLS cap level
    /// with the already-hot-reloaded mail per-IP cap — `transport.md` § Abuse
    /// posture item (2)). Always present (default `256`); on a plain-HTTP nest
    /// nothing enforces it (harmless — there is no TLS accept loop).
    pub per_ip_conn_limit: Arc<fauna_conn_limit::PerIpConnLimit>,

    /// Spec Y2 federation: per-originating-nest throttle on the cross-nest
    /// data plane (key-package fetch + welcome delivery). Reuses the bridge
    /// `Limiter` type; the bucket key is `(originating_nest_id, target_actor,
    /// "<op>")`. Rate limiting is the primary DoS defense since a nest
    /// signature is attribution, not authorization
    /// (`docs/goal/architecture/federation.md` § Security).
    pub federation_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Global cap on concurrently-executing RPC handlers (F5). Every dispatch
    /// (`dispatch_core::spawn_dispatch`, both the per-actor and federation paths)
    /// acquires a permit before spawning its handler and holds it for the
    /// handler's lifetime; when the cap is reached the caller's read loop blocks
    /// on `acquire` — backpressure — so a single socket pipelining tens of
    /// thousands of requests cannot grow tasks/memory without bound. Sized at
    /// [`crate::dispatch_core::MAX_INFLIGHT_HANDLERS`].
    pub handler_semaphore: Arc<tokio::sync::Semaphore>,

    /// Per-source throttle on the anonymous (pre-identity) discovery surface
    /// (`nest.info` / `handle.available` / `nest.resolve` / `actor.by_handle`).
    /// Reuses the bridge `Limiter` type; the bucket key is `(source-IP, kind)`
    /// where the source IP is `RpcConnection.peer_addr` (the real client IP on
    /// the public TLS path — `WithConnectInfo`). Rate limiting is the primary
    /// DoS / directory-enumeration defense on this signature-less surface
    /// (`docs/goal/architecture/federation.md` § Security). See
    /// `crate::anonymous_rate_limit`.
    pub anonymous_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// The failed-credential throttle on the bearer WS upgrades and
    /// `fauna.auth.device_handshake`: counts refusals per
    /// `(source IP × claimed identity)` and, over budget, answers `429` +
    /// `Retry-After` (upgrades) or `fauna.protocol.rate_limited` (the mint).
    /// Carries its own shed reporters. See `crate::failed_credential_throttle`.
    pub failed_credential_throttle:
        Arc<crate::failed_credential_throttle::FailedCredentialThrottle>,

    /// Rate-limited reporter for `anonymous_rate_limit`'s trips — one
    /// `warn!` per `fauna_conn_limit::SHED_LOG_INTERVAL`, never one per
    /// refusal. Own counter per gate (this and the four siblings below), so a
    /// burst on one gate can never suppress another's line. See
    /// `crate::anonymous_rate_limit::report_shed`.
    pub anonymous_rate_limit_shed: Arc<fauna_conn_limit::ShedCounter>,

    /// Per-source throttle on the one-time admin-claim attempt surface
    /// (`fauna.auth.claim_admin`). The claim code is a short deploy-time secret,
    /// so an unthrottled `claim_admin` is brute-forceable on a fresh unclaimed
    /// nest (admin takeover); a tight per-source limit (`claim_config`) defeats
    /// the realistic single-source automated attack. Keyed on
    /// `RpcConnection.peer_addr` like `anonymous_rate_limit`. See
    /// `crate::anonymous_rate_limit::claim_config`.
    pub claim_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// **Global** (all-sources-combined) cap on the admin-claim surface
    /// (`fauna.auth.claim_admin`) — defense in depth behind `claim_rate_limit`.
    /// The per-source limiter is evaded by a *distributed* brute-force (each IP
    /// gets the full per-source budget); a single source-independent bucket caps
    /// the *total* attempt rate so a botnet cannot accelerate guessing the short
    /// claim code. Must be a separate `Limiter` instance from `claim_rate_limit`
    /// (keyed source-independently via `anonymous_rate_limit::check_global`). The
    /// availability tradeoff (a flood can delay, never take over, the admin's
    /// claim) is accepted — see `crate::anonymous_rate_limit::global_claim_config`.
    pub global_claim_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Rate-limited reporter for a `claim_admin` trip on EITHER
    /// `claim_rate_limit` or `global_claim_rate_limit` — one counter, since
    /// both gates already collapse to the same single warning today. See
    /// `crate::anonymous_rate_limit::report_shed`.
    pub claim_rate_limit_shed: Arc<fauna_conn_limit::ShedCounter>,

    /// Per-source throttle on the in-band invite-code verify surface
    /// (`fauna.account.invite_code.verify`). Same surface class as the admin
    /// claim — guessing a secret over an anonymous signature-less kind — but
    /// lower severity (yields an unauthorized account, not admin) and a stronger
    /// code, so a per-source bound (no global cap) suffices. Restores the per-IP
    /// limit the retired HTTP `/api/v1/invite/verify` twin carried. Keyed on
    /// `RpcConnection.peer_addr`. See
    /// `crate::anonymous_rate_limit::invite_verify_config`.
    pub invite_verify_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Rate-limited reporter for `invite_verify_rate_limit`'s trips. See
    /// `crate::anonymous_rate_limit::report_shed`.
    pub invite_verify_rate_limit_shed: Arc<fauna_conn_limit::ShedCounter>,

    /// Per-source throttle on the self-service registration surface
    /// (`fauna.account.register`). Register is an anonymous signature-bound write
    /// that creates an account (handle reservation + Ed25519 verify); unthrottled
    /// it permits registration floods / handle exhaustion / spam actors on a
    /// `public`+open nest. Restores the per-IP limit the retired HTTP
    /// `POST /api/v1/register` twin carried (the dead `registration_limiter`
    /// governor field never re-wired it), now that the anonymous WS path exposes
    /// the real client IP. Keyed on `RpcConnection.peer_addr`. See
    /// `crate::anonymous_rate_limit::register_config`.
    pub register_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Rate-limited reporter for `register_rate_limit`'s trips. See
    /// `crate::anonymous_rate_limit::report_shed`.
    pub register_rate_limit_shed: Arc<fauna_conn_limit::ShedCounter>,

    /// Per-source throttle on the in-band invite-**request** submit surface
    /// (`fauna.account.invite_request.submit`). Submit is an anonymous
    /// signature-bound write that inserts a `pending` row (dedup only by
    /// `actor_id`, defeated by rotating keypairs); unthrottled it floods SQLite
    /// with pending rows. Pairs with the global `MAX_PENDING_INVITE_REQUESTS`
    /// row-count cap in `invite_core` (this gate slows one flooder; the cap is the
    /// hard disk backstop). Keyed on `RpcConnection.peer_addr`. See
    /// `crate::anonymous_rate_limit::invite_request_config`.
    pub invite_request_rate_limit: Arc<crate::bridge_rate_limit::Limiter>,

    /// Rate-limited reporter for `invite_request_rate_limit`'s trips. See
    /// `crate::anonymous_rate_limit::report_shed`.
    pub invite_request_rate_limit_shed: Arc<fauna_conn_limit::ShedCounter>,

    /// The deployment's client-set NAT axis (`public` / `private`), resolved at
    /// startup from the `nest_nat_mode` DB row when present, falling back to the
    /// `config.nest.mode` seed (`FAUNA_MODE`) when absent. This is the single
    /// DB-backed source every runtime reader of the NAT axis consults — never
    /// `config.nest.mode` directly (that is only the pre-claim boot seed). The
    /// MDA bind (`whoami`), the MTA supervisor gate (`mta_should_run`), the
    /// private-only LAN-cert sync, and the admin/factory-reset paths all read
    /// this. Interior-mutable (`RwLock`) because the admin-panel toggle
    /// (`fauna.setup.nat_mode` / `apply_node_mode_change`) flips it live;
    /// ACME/STUN are restart-applied (the boot reconcile enforces them —
    /// `2026-06-15-nest-nat-mode-client-set-design.md` §§ 3,6).
    pub node_mode: std::sync::Arc<tokio::sync::RwLock<crate::config::NodeMode>>,

    /// Whether this deployment enforces the actor's **tier quota** — the inbox
    /// byte cap (`routes::deliver_to_inbox`), the per-tier feed ceiling
    /// (`feed_routes`), and the federation Welcome inbox cap
    /// (`federation_handlers`). Set by the deployment **artifact**, not by any
    /// human: the standalone server (`main.rs`) passes `true`, the embedded
    /// single-user desktop nest (`desktop_serve.rs`) passes `false` — a tier
    /// ceiling on the owner's own machine is nonsense. `AppState::for_test`
    /// defaults it `false`.
    ///
    /// This is **not** a registration gate. It was carved out of the old
    /// `require_registration` boolean, which conflated two unrelated questions:
    /// "may an unknown actor self-provision on handshake?" (now permanently
    /// **no** — the auto-provision branch is deleted; `login.md` § Errors) and
    /// "does this actor's tier cap apply?" (this field). Keeping them fused meant
    /// the desktop nest silently disabled *both*, and the CLI flag that fed them
    /// was configuration-file theatre (`principles.md` § One configuration surface).
    pub enforce_tier_quotas: std::sync::Arc<tokio::sync::RwLock<bool>>,

    /// Client-set `subhandles` policy — whether the nest advertises the
    /// `handle@domain` / `@handle.domain` subhandle address forms (`account_core`,
    /// `discovery_core` readers + `nest.info`/`setup.status`). Resolved at startup
    /// from the `nest_subhandles` DB row when present, else the
    /// `config.nest.subhandles` seed. Interior-mutable (`RwLock`) because the
    /// admin RPC (`fauna.admin.set_subhandles` / `apply_subhandles_change`) flips
    /// it live. Sibling of `node_mode` — the deployment-policy cluster.
    pub subhandles: std::sync::Arc<tokio::sync::RwLock<bool>>,

    /// The deployment's registration posture + the orthogonal free-tier ceiling —
    /// the **one** knob deciding whether a new account can be created
    /// (`account_core` reader). Resolved at startup from the `nest_registration_mode`
    /// DB row when present, else the `config.nest.registration_mode` seed;
    /// interior-mutable because `fauna.admin.set_registration_mode` flips it live.
    ///
    /// Replaces the old `registration.open` + `registration.invite_required` +
    /// `max_free_users` triple, whose 8 boolean combinations expressed only 3
    /// meaningful postures. Owner: `public-mode.md` § Registration Modes.
    pub registration_mode: std::sync::Arc<
        tokio::sync::RwLock<(fauna_protocol::node_policy::RegistrationMode, Option<u64>)>,
    >,

    /// The "accept only signups carrying app age verification" gate
    /// (`family-safety.md` § The account age band D5+D6; gating scope
    /// `public-mode.md` § Age at registration — `account_core` reader).
    /// Resolved at startup from the `nest_age_verification_required` DB row
    /// when present, else the **hard-coded default off** — deliberately no
    /// config seed. Interior-mutable because
    /// `fauna.admin.set_age_verification_required` flips it live.
    pub age_verification_required: std::sync::Arc<tokio::sync::RwLock<bool>>,

    /// Client-set node-wide storage cap (`router_status` reader + `setup.status`).
    /// `Some(v)` caps the nest at `v` bytes; `None` is no cap. The runtime reader
    /// consults this, never `config.nest.max_storage_bytes` directly (that is only
    /// the pre-claim boot seed). Resolved at startup from the
    /// `nest_max_storage_bytes` DB row when present (including a present row that
    /// cleared the cap), else the config seed. Interior-mutable (`RwLock`) because
    /// the admin RPC (`fauna.admin.set_max_storage_bytes` /
    /// `apply_max_storage_bytes_change`) sets it live. Sibling of `node_mode` —
    /// the deployment-policy cluster.
    pub max_storage_bytes: std::sync::Arc<tokio::sync::RwLock<Option<u64>>>,

    /// Storage-boundary operations: seal-shape verification at ingest,
    /// floor-derived server-side search, ACME-TLS handling.
    ///
    /// One implementation ([`crate::storage::SealedStorage`]), live from the
    /// first instruction — the storage-mode axis (and the swap-on-commit that
    /// needed interior mutability here) was retired in Phase 4
    /// (`docs/goal/architecture/nest/storage-modes.md` § Boot story).
    pub storage: crate::storage::SharedStorage,

    /// Test-only override of the registry the app relay's two demand doors —
    /// `region_relay::artifact_get_handler` and
    /// `region_tier::demand_situs_content_policies` — verify enrollment
    /// against (read via [`crate::region_tier::demand_door_registry`]).
    /// `None` (production, and every `for_test` caller that doesn't opt in)
    /// falls back to the real compiled-in `region_tier::relay_registry()`,
    /// same as before. Set via [`Self::install_region_registry_for_test`]
    /// before the state is wrapped in `Arc` — the `storage`/
    /// `install_storage_for_test` shape, not a process-global: a global
    /// fixture would enroll a region for every test sharing the binary,
    /// including `the_compiled_in_registry_binds_nobody`, which pins that the
    /// real registry enrols nobody. Does **not** change what the refill a
    /// first ask schedules (`region_relay::schedule_refill`) verifies
    /// against — that always reads the real `relay_registry()` — so an
    /// enrolled override never causes a real fetch to `REGION_LOG_BASE_URL`
    /// under test.
    pub region_registry_override: Option<fauna_core::region_authority::RegionRegistry>,

    /// Test-only outbound clock override. `0` = unset → real wall-clock;
    /// any other value freezes [`AppState::outbound_now`] there so the e2e
    /// can fast-forward the 4 h delay-warning / 5 d give-up boundaries
    /// deterministically. Driven by `POST /api/v1/test/outbound/clock`.
    /// Never compiled into production.
    #[cfg(feature = "test-hooks")]
    pub outbound_clock_override: std::sync::Arc<std::sync::atomic::AtomicI64>,

    /// Test-only re-score worklist serve counters — the drain's decision-point
    /// observable, so an e2e can prove "the nest decided after my plant landed"
    /// instead of sleeping a settle window. Bumped by
    /// `rescore_worklist_handler` once its unit list is built; read via
    /// `GET /api/v1/test/capabilities/rescore_worklist`. Never compiled into
    /// production; see [`crate::rescore_drain_test_hook`] for why one counter
    /// suffices here where the alert sweep needs a pair.
    #[cfg(feature = "test-hooks")]
    pub rescore_worklist_serves:
        std::sync::Arc<crate::rescore_drain_test_hook::RescoreWorklistServes>,

    /// Test-only counter of post bridge fan-outs **initiated** — the decision
    /// point behind "a re-delivered forward must not fan out again", so an e2e
    /// can assert that absence against state rather than a settle window.
    /// Bumped synchronously by [`spawn_post_bridge_fanout`], read via
    /// `GET /api/v1/test/posts/fanouts`. Never compiled into production; see
    /// [`crate::post_fanout_test_hook`] for why the *initiation* point is the
    /// only sound place to count it.
    #[cfg(feature = "test-hooks")]
    pub post_fanout_initiations:
        std::sync::Arc<crate::post_fanout_test_hook::PostFanoutInitiations>,

    /// Test-only forced-on override for the content-sealing-epochs mail
    /// write gate (`fauna_mls::wrapped_blob::MAIL_EPOCH_SEALING_WRITE_DEFAULT`,
    /// `true` since the 2026-07-19 flip — this force-on override is now
    /// redundant-but-harmless and kept so pre-flip e2e drives stay valid).
    /// `false` (default) = use the production constant; `true` = force
    /// [`AppState::epoch_sealing_enabled`] on. Driven by `POST
    /// /api/v1/test/content/epoch_sealing`. Never compiled into production.
    #[cfg(feature = "test-hooks")]
    pub epoch_sealing_test_override: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// Test-only hold on the custody-hosting pump's PERIODIC loop
    /// ([`crate::custody_hosting_worker::CustodyHostingWorker::spawn`]):
    /// `true` = every scheduled tick skips its pass, so the only passes are
    /// explicit `POST /api/v1/test/custody_hosting/run-now` pokes. Lets a
    /// journey assert the before-any-pass state without racing the 15-minute
    /// cadence (convention 14). Driven by `POST
    /// /api/v1/test/custody_hosting/hold`. Never compiled into production.
    #[cfg(feature = "test-hooks")]
    pub custody_hosting_periodic_held: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// MTA-STS policy fetcher for `fauna.bridges.fetch_mta_sts_policy`
    /// (T2.1a). nest owns the `_mta-sts.<domain>` TXT + `.well-known/
    /// mta-sts.txt` GET and the per-`max_age` cache so the Go MTA bridge
    /// can apply the per-host enforce/testing decision locally before the
    /// TLS handshake (`docs/goal/behavior/smtp-server.md` § MX resolution,
    /// item 3). Production wraps a `LiveMtaStsFetcher` in
    /// `CachingMtaStsFetcher` (falling back to `NullMtaStsFetcher` if the
    /// live fetcher can't be built); `for_test` installs `NullMtaStsFetcher`.
    pub mta_sts_fetcher: std::sync::Arc<dyn fauna_mail::outbound::mta_sts::MtaStsFetcher>,

    /// Public-recursive DNS self-verifier for `fauna.dns.verify_records` (and
    /// the web-content `_fauna-verify` domain check). Resolves each expected
    /// record against a recursive resolver and caches observed values with a
    /// short TTL. Production wraps a `LiveRecordResolver`; `for_test` (and a
    /// resolver-build failure) installs a `NullRecordResolver` so verification
    /// answers "checking" rather than a false "missing".
    pub dns_verifier: std::sync::Arc<crate::dns_verifier::DnsVerifier>,

    /// Test-only scripted MTA-STS lookups, keyed by domain. Consulted by
    /// `fetch_mta_sts_policy_handler` *before* `mta_sts_fetcher` so a
    /// tier_3 e2e can script a policy (or a not-published / fetch-error /
    /// invalid outcome) for a test domain without any real DNS/HTTPS.
    /// Driven by `POST /api/v1/test/outbound/mta-sts`. Never compiled into
    /// production; gated on `test-hooks` alone so it works in the plain
    /// `--features test-hooks` e2e build (same gating as the clock hook).
    #[cfg(feature = "test-hooks")]
    pub mta_sts_override: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, fauna_mail::outbound::mta_sts::MtaStsLookup>,
        >,
    >,

    /// DANE/TLSA resolver for `fauna.bridges.fetch_tlsa` (T2.1b). nest owns
    /// the `_25._tcp.<mx_host>` DNSSEC-validating lookup (the Go stdlib can't
    /// do DNSSEC) so the Go MTA bridge can pin the outbound TLS handshake to
    /// the published TLSA records (`docs/goal/behavior/smtp-server.md`
    /// § Architectural rules).
    /// Production installs `LiveTlsaResolver`; `for_test` installs
    /// `NullTlsaResolver` (always empty → no DANE pinning).
    pub tlsa_resolver: std::sync::Arc<dyn fauna_mail::outbound::dane::TlsaResolver>,

    /// DNSSEC-validating MX resolver for `fauna.bridges.resolve_mx`. nest
    /// owns the recipient domain's MX lookup for the same reason it owns the
    /// TLSA one — the Go stdlib resolver can't do DNSSEC — and the reply
    /// carries the RRset's `secure` provenance so the bridge can gate DANE
    /// pinning on it (RFC 7672 §2.2; `docs/goal/behavior/smtp-server.md`
    /// § Architectural rules, outbound DANE).
    /// Production installs `LiveMxRrsetResolver`; `for_test` installs
    /// `NullMxRrsetResolver` (no hosts, not secure).
    pub mx_resolver: std::sync::Arc<dyn fauna_mail::outbound::mx::MxRrsetResolver>,

    /// Test-only scripted TLSA lookups, keyed by MX host. Consulted by
    /// `fetch_tlsa_handler` *before* `tlsa_resolver` so a tier_3 e2e can
    /// script published records (or an empty no-DANE result) for a test MX
    /// host without any real DNSSEC lookup. Driven by
    /// `POST /api/v1/test/outbound/tlsa`. Never compiled into production;
    /// gated on `test-hooks` alone (same as the MTA-STS override).
    #[cfg(feature = "test-hooks")]
    pub tlsa_override: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, Vec<fauna_mail::outbound::dane::TlsaRecord>>,
        >,
    >,

    /// Outbound-STARTTLS posture prober for the deliverability diagnostic's
    /// "Outbound TLS to gmail.com" check (`mail-deliverability.md` § Symptom
    /// diagnostics). Production installs `LiveStarttlsProber`; `for_test`
    /// installs `NullStarttlsProber` (reports the probe unavailable → a Warn
    /// row, no network). Mirrors the `mta_sts_fetcher` / `tlsa_resolver` seam.
    pub starttls_prober: std::sync::Arc<dyn crate::mail_deliverability::StarttlsProber>,

    /// Nest-side link-preview resolver state for `fauna.linkpreview.resolve`
    /// (`render-model.md` § D4): the SSRF-safe outbound fetcher, the by-url TTL
    /// cache, and the per-actor rate limiter. Production wires the real
    /// `SsrfSafeFetcher`; the `test-hooks` fixture override rides on the
    /// separate `link_preview_override` map below so production SSRF is never
    /// loosened.
    pub link_preview: crate::link_preview::LinkPreviewState,

    /// Test-only scripted link-preview fetch fixtures, keyed by url -> (body
    /// bytes, content-type). Consulted by the `fauna.linkpreview.resolve`
    /// handler *ahead of* `link_preview.fetcher`, but **only for urls present in
    /// the map** -- an unmapped url (e.g. a private-IP literal) still falls
    /// through to the real SSRF-guarded fetcher, so the e2e exercises both the
    /// served-OG success path and the real SSRF rejection without loosening
    /// production. Driven by `POST /api/v1/test/linkpreview/*`. Never compiled
    /// into production; gated on `test-hooks` alone (same as `mta_sts_override`).
    #[cfg(feature = "test-hooks")]
    pub link_preview_override:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (bytes::Bytes, String)>>>,

    /// Test-only fault injection for `fauna.bridges.list`, keyed by
    /// `BridgeProvider::id()`. Every currently-registered provider's
    /// `status()` only errors on a genuine DB fault, and every declared
    /// `BridgeLinkMode` today has `platform: None` (so a client's
    /// `applicable_modes` count never falls to zero on its own) — with no
    /// override, the un-linkable-mode degraded shape
    /// (`fauna_client_bridges::link_block`, `bridges.md` § Errors & edge
    /// cases) is unreachable from a real running nest. Consulted by
    /// `bridges_ui_handlers::list_handler` ahead of the real
    /// `provider.status()` call, same shape as `link_preview_override`.
    /// Driven by `POST /api/v1/test/bridges/:id/status-override`. Never
    /// compiled into production; gated on `test-hooks` alone.
    #[cfg(feature = "test-hooks")]
    pub bridge_status_override: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, crate::bridge_management::BridgeStatusOverride>,
        >,
    >,

    /// Actors whose hosted ATProto identity
    /// `fauna.bridges.atproto.get_integration_status` stops NAMING — the reply's
    /// `identity` reads `None` while the row, the DID, the bridge's serving and
    /// the public log are all untouched: the box answering "nothing" about an
    /// identity it minted, which is the lie the client's audit floor defends
    /// against (`atproto-identity-custody.md` § The audit floor and the
    /// departed-DID alarm). A retirement is deliberately NOT the lever — it is
    /// the attributable, silent arm of that rule. Why the seam is cut at the
    /// reply is `atproto_identity_test_hook`'s module doc. Driven by
    /// `POST /api/v1/test/atproto/withhold-identity`. Never compiled into
    /// production; gated on `test-hooks` alone.
    #[cfg(feature = "test-hooks")]
    pub atproto_identity_withheld:
        std::sync::Arc<std::sync::Mutex<std::collections::HashSet<[u8; 32]>>>,

    /// Per-channel refusal of one class of `fauna.conversations.channel.send`
    /// envelope — the seam that lets a tier_3 journey stage a succession sweep
    /// which is genuinely *partial*, and so render and press
    /// `recovery-kit-sweep-retry-button` at all
    /// (`settings.md` § Recovery kit → *Finishing an unfinished group sweep*).
    /// Consulted at the top of `conversations_handlers::channel_send_core`,
    /// before the envelope is ingested, so a refused send consumes no seq —
    /// the fault is a flake, not a hole. Driven by
    /// `POST /api/v1/test/conversations/channel/{id}/refuse` and its `clear`
    /// twin; which class to refuse, and why it must be a class rather than a
    /// count, is `channel_refusal_test_hook`'s own module doc. Never compiled
    /// into production; gated on `test-hooks` alone, same as the two overrides
    /// above.
    #[cfg(feature = "test-hooks")]
    pub channel_send_refusal: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<[u8; 32], crate::channel_refusal_test_hook::ChannelRefusal>,
        >,
    >,

    /// Per-RPC-kind dispatch hold — the seam that makes a client's *pre-fetch*
    /// window a state a test can stand in rather than an interval it races
    /// (`settings.md` § Privacy sub-page: "Until `inbox_mode_get` has answered,
    /// the mode is *unknown*"). Consulted by `dispatch_core::spawn_dispatch` at
    /// the one chokepoint every WS-RPC request crosses, before the per-kind
    /// deadline timeout starts, so an armed hold spends none of the handler's
    /// own budget. Driven by `POST /api/v1/test/rpc-hold/{kind}`, its `GET`
    /// arrival observable, and the `release` twin; why a hold rather than a
    /// process freeze or a shortened debounce is `rpc_hold_test_hook`'s own
    /// module doc. Never compiled into production; gated on `test-hooks` alone,
    /// same as the overrides above.
    #[cfg(feature = "test-hooks")]
    pub rpc_hold: std::sync::Arc<crate::rpc_hold_test_hook::RpcHoldRegistry>,
}

impl AppState {
    /// Clone the storage impl's `Arc` out so callers own it across `.await`s.
    pub fn storage(&self) -> crate::storage::SharedStorage {
        self.storage.clone()
    }

    /// Spawn a long-lived task scoped to THIS serving generation: tracked by
    /// `serve_tasks`, killed at its next await point when `serve_generation`
    /// is cancelled (generation teardown — deployment-seed rotation, admin
    /// serving-port change). Every long-lived spawn in the nest's serve graph
    /// goes through here or [`Self::scope_handle`]; see `box-recovery.md`
    /// § Deployment-seed rotation → *Adoption by the running process*.
    pub fn spawn_scoped<F>(&self, fut: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send,
    {
        let token = self.serve_generation.clone();
        self.serve_tasks.spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = fut => {}
            }
        })
    }

    /// Adopt an already-spawned task's `JoinHandle` into this serving
    /// generation — for the `spawn_*` helpers that spawn internally and
    /// return their handle. The supervisor aborts the inner task on
    /// generation cancel and is itself tracked, so teardown's
    /// `serve_tasks.wait()` covers the adopted task too.
    pub fn scope_handle(&self, handle: tokio::task::JoinHandle<()>) {
        let token = self.serve_generation.clone();
        self.serve_tasks.spawn(async move {
            let mut handle = handle;
            tokio::select! {
                _ = token.cancelled() => {
                    handle.abort();
                    let _ = handle.await;
                }
                _ = &mut handle => {}
            }
        });
    }

    /// Tear down this serving generation so `start_server` can be re-entered:
    /// abort the server task (the accept loop), 1001-drain the live WS
    /// connections (their tasks are detached — an abort of the accept loop
    /// alone would leave them serving the old graph indefinitely; the 1001
    /// makes clients reconnect promptly, and the reconnect is where a rotated
    /// identity + chain are met), then cancel the generation scope and await
    /// the tracker bounded. After this returns, no task of this generation
    /// signs anything — the re-entered `start_server` rebuilds the whole
    /// graph from the (possibly rotated) DB.
    pub async fn teardown_serving_generation(&self, server_handle: tokio::task::JoinHandle<()>) {
        server_handle.abort();
        let _ = server_handle.await;
        self.ws.begin_shutdown();
        let drain_deadline = std::time::Instant::now() + crate::ws::GRACEFUL_SHUTDOWN_TIMEOUT;
        while self.ws.connection_count() > 0 && std::time::Instant::now() < drain_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let remaining = self.ws.connection_count();
        if remaining > 0 {
            tracing::warn!(
                "serving-generation teardown: {remaining} WS connection(s) did not drain; \
                 proceeding — their tasks hold the old graph only until they close"
            );
        }
        self.serve_generation.cancel();
        self.serve_tasks.close();
        if tokio::time::timeout(std::time::Duration::from_secs(5), self.serve_tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                "serving-generation teardown: tasks did not drain within 5s; re-entering anyway"
            );
        }
    }

    /// Signal the federation exchange originator that a LOCAL exportable
    /// aggregate may have changed (report capture/withdrawal, report-share
    /// opt-out sweep). Cheap and debounced —
    /// call after the mutation commits; a spurious bump is harmless.
    pub fn notify_exchange_transition(&self) {
        self.exchange_transition_tx
            .send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Revoke an actor's live authority: kill its bearer tokens, close the
    /// WebSockets it already holds (**4401**, no drain), and purge its
    /// mailbox-state push subscriptions.
    ///
    /// Per `transport.md` § Connection lifecycle → *Revocation teardown*,
    /// every path that strips an actor's authority calls this (or performs
    /// the same halves inline where the site needs a specific interleaving —
    /// the pre-identity lockout, suspend, the eviction ladder). Tokens stop the *next*
    /// connection, `disconnect_actor` closes the ones already open; neither
    /// half alone suffices. The registry purge is the bridge-specific third
    /// piece (a no-op for every non-MDA actor): a revoked bridge keeps its
    /// `users` row, so the mint gate does not stop it re-opening a socket —
    /// and a Push is not an RPC, so only the purge stops stale mailbox-state
    /// subscriptions re-arming on that socket
    /// ([`crate::bridge_push_registry::BridgePushRegistry::remove_mda`]).
    ///
    /// Call it *after* the authority-stripping DB write commits, so a
    /// connection racing the teardown still resolves to no caller class at
    /// dispatch.
    pub async fn revoke_actor_authority(&self, actor_id: &[u8; 32]) {
        self.auth
            .token_store
            .revoke_actor(&fauna_core::identity::ActorId(*actor_id))
            .await;
        self.close_actor_sockets(actor_id);
        self.bridge_push_registry.remove_mda(actor_id);
    }

    /// The **socket half** of an actor-wide revocation: every WS-RPC
    /// connection of `actor_id` closes **4401** (no drain), and with it every
    /// relay-serving announce it carried. The call a site makes when it does
    /// the halves inline for its own interleaving (the pre-identity lockout, suspend, the
    /// eviction ladder) — the one name every actor-wide socket close goes
    /// through (`transport-connection.md` § *Revocation teardown*). Call it
    /// after the token rows are revoked, as every sweep is.
    pub fn close_actor_sockets(&self, actor_id: &[u8; 32]) {
        self.ws.disconnect_actor(actor_id);
    }

    /// [`Self::revoke_actor_authority`], for the handlers that revoke the
    /// actor **they are themselves being called by**: the identity-succession
    /// ceremony (`fauna.recovery.succession.submit`) and, since 2026-10-04, the
    /// signed-in lock (`fauna.sessions.lockout`).
    ///
    /// Same three halves, same instant, one difference — the connection this
    /// request arrived on is closed only *after* its Reply is on the wire
    /// ([`crate::ws::WsState::disconnect_actor_sparing`]). It stops dispatching
    /// with all the others; it just gets to finish the sentence.
    ///
    /// The signed-in lock has the same shape for the same reason: the actor
    /// locks its own account over a socket the lock closes, and its Reply
    /// (`ok` + `locked_until`) is how the app learns to leave its shell for the
    /// locked surface (`ui/sessions.md` § User actions).
    ///
    /// **Why only these.** Every other member of the teardown enumeration strips
    /// the authority of somebody who is not asking — an admin suspends a user,
    /// the ladder evicts an account, the executor deletes one — and for those
    /// the 4401 is the whole message, which is why the unsparing form drops
    /// whatever was queued behind it. The succession ceremony retires the
    /// identity whose socket carries it, and its Reply (`new_actor_id` +
    /// `succeeded_at`) is the ceremony's entire product: dropped, the client
    /// cannot tell "the account moved" from "nothing happened", takes
    /// `succeed_with_held_kit`'s `Unconfirmed` arm, and reconciles a ceremony
    /// that in fact committed. `identity-succession.md` § Enforcement on the
    /// home nest; `transport-connection.md` § Connection lifecycle →
    /// *Revocation teardown*.
    ///
    /// "Only here" is about the **per-actor** enumeration. The per-token
    /// teardown ([`Self::revoke_session_authority`] and its `revoke_all` twin,
    /// 2026-09-20) spares the caller too, and unconditionally: those kinds are
    /// self-directed by construction, so they have no unsparing form to be the
    /// default.
    ///
    /// Outside a handler — a background sweep, the eviction ladder — there is
    /// no caller, so this degrades to exactly [`Self::revoke_actor_authority`].
    pub async fn revoke_actor_authority_sparing_caller(&self, actor_id: &[u8; 32]) {
        self.auth
            .token_store
            .revoke_actor(&fauna_core::identity::ActorId(*actor_id))
            .await;
        self.ws.disconnect_actor_sparing(
            actor_id,
            crate::dispatch_core::current_caller().map(|c| (c.conn_id, c.correlation_id)),
        );
        self.bridge_push_registry.remove_mda(actor_id);
    }

    /// Revoke **one session's** live authority: drop that bearer's row and
    /// close the WebSockets it already opened (**4401**, no drain). The
    /// per-token twin of [`Self::revoke_actor_authority`], and the socket half
    /// `fauna.sessions.revoke` lacked until 2026-09-20.
    ///
    /// Same two-halves rule, same reason: the bearer is validated once, at the
    /// upgrade, and `dispatch_core` never re-reads the token store, so dropping
    /// the row governs only that session's *next* connection while the one it
    /// already holds keeps dispatching as `User`. The difference from the
    /// per-actor form is what it leaves alone — the actor's **other** sessions
    /// keep their tokens and their sockets, because ending one session the user
    /// does not recognize is not signing out (`devices.md` § What a session is,
    /// and what revoking one does).
    ///
    /// **The caller's own Reply survives.** This kind is always self-directed —
    /// the handler's ownership check refuses any other actor's `token_id` — so
    /// the session being ended is regularly the connection asking, and that
    /// Reply is the app's only evidence the revoke committed; a bare 4401 is
    /// indistinguishable from an unrelated eviction. Unlike
    /// [`Self::revoke_actor_authority_sparing_caller`], which is a separate
    /// method because its unsparing sibling is the common case, there is no
    /// unsparing form here: nothing ever wants one.
    ///
    /// No `BridgePushRegistry` purge, which the per-actor form does need: that
    /// purge is for an actor that has lost its authority entirely, and this
    /// actor has not — it keeps every other session it holds.
    ///
    /// ⚠ **The row delete stays before the sweep** (both helpers): it is what
    /// makes [`Self::register_upgraded_connection`]'s one re-read close the
    /// upgrade window totally. Sweep first, and a connection registering
    /// between the sweep and the delete would find its row still present and
    /// never be closed.
    pub async fn revoke_session_authority(&self, actor_id: &[u8; 32], token_id: &str) {
        self.auth.token_store.revoke_by_token_id(token_id).await;
        self.ws.disconnect_token_id(
            actor_id,
            token_id,
            crate::dispatch_core::current_caller().map(|c| (c.conn_id, c.correlation_id)),
        );
        #[cfg(test)]
        revoke_race::after_sweep(actor_id).await;
    }

    /// [`Self::revoke_session_authority`] for `fauna.sessions.revoke_all`:
    /// revoke every session of `actor_id` **but** `keep_token_id`, and close
    /// the sockets of each. Returns how many token rows were revoked (the
    /// reply's `revoked` count).
    ///
    /// A `keep_token_id` matching none of the actor's sessions ends them all,
    /// the caller's included — the renewal race `devices.md` § The client's own
    /// session documents and accepts. That acceptance rests on the next request
    /// re-minting, so the caller's Reply has to survive its own teardown; it
    /// does, through the same one-frame grace.
    pub async fn revoke_other_sessions_authority(
        &self,
        actor_id: &[u8; 32],
        keep_token_id: &str,
    ) -> usize {
        let revoked = self
            .auth
            .token_store
            .revoke_all_except_token_id(&fauna_core::identity::ActorId(*actor_id), keep_token_id)
            .await;
        self.ws.disconnect_actor_except_token_id(
            actor_id,
            keep_token_id,
            crate::dispatch_core::current_caller().map(|c| (c.conn_id, c.correlation_id)),
        );
        #[cfg(test)]
        revoke_race::after_sweep(actor_id).await;
        revoked
    }

    /// Revoke **one device's** live authority over `actor_id`: drop every token
    /// row `device_key` minted and close the WebSockets those bearers opened
    /// (**4401**, no drain). The per-device twin of
    /// [`Self::revoke_session_authority`] — the one call each device-removal
    /// door makes (`fauna.sync.devices.delete`, `fauna.sync.device_grant.revoke`,
    /// graduation's severance of a marked guardian device). Returns how many
    /// token rows were revoked (the grant revoke's `sessions_revoked`).
    ///
    /// Same two-halves rule: dropping the rows governs only the device's *next*
    /// connection, and the socket it already holds would keep dispatching as
    /// `User`, because the dispatch gate reads actor state and a device removal
    /// leaves the actor healthy. The actor's other sessions — direct sign-ins,
    /// other devices — keep their tokens and sockets.
    ///
    /// **The caller's own Reply survives**, for the reason the per-token form
    /// gives: both kinds are presentable by the very device being removed, over
    /// its own device-minted session (an agent retiring itself, a user deleting
    /// the device they hold), and the Reply is that caller's only evidence the
    /// removal committed. For graduation the caller is the guardian, whose
    /// connection is not in the ward's entry, so the spare matches nothing.
    ///
    /// ⚠ **The row delete stays before the sweep**, exactly as in the per-token
    /// helpers: it is what lets [`Self::register_upgraded_connection`]'s
    /// re-read close a device-minted upgrade racing the removal.
    pub async fn revoke_device_authority(
        &self,
        actor_id: &[u8; 32],
        device_key: &[u8; 32],
    ) -> usize {
        let revoked = self
            .auth
            .token_store
            .revoke_minted_by(&fauna_core::identity::ActorId(*actor_id), device_key)
            .await;
        self.ws.disconnect_device_key(
            actor_id,
            device_key,
            crate::dispatch_core::current_caller().map(|c| (c.conn_id, c.correlation_id)),
        );
        #[cfg(test)]
        revoke_race::after_sweep(actor_id).await;
        revoked
    }

    /// Register a freshly upgraded connection in `WsState.subs`, remembering
    /// which session (`token_id`) its bearer was — and close it at once if that
    /// session was revoked while the upgrade was in flight.
    ///
    /// **The window.** `ws_handler` validates the bearer, the 101 goes out, and
    /// only then does `handle_ws` register the connection. A
    /// `fauna.sessions.{revoke,revoke_all}` landing in between sweeps a `subs`
    /// this connection has not joined yet, and the connection then registers
    /// carrying the revoked `token_id`. Nothing downstream would ever deny it:
    /// `dispatch_core` never re-reads the token store, and the dispatch gate's
    /// authority read (`caller_class_for_actor`) sees only the *actor*, which a
    /// per-token revoke leaves perfectly healthy. It would dispatch as `User`
    /// for ever — no TTL ends it, since the row it would expire from is gone.
    ///
    /// **Why one re-read after the subscribe closes it completely.** Every
    /// sub-actor revoke helper — [`Self::revoke_session_authority`],
    /// [`Self::revoke_other_sessions_authority`] and
    /// [`Self::revoke_device_authority`] — deletes the token rows strictly
    /// before it sweeps the sockets. So at the instant of this read
    /// exactly one of two things is true: the row is already gone, and we
    /// revoke the connection ourselves; or it is still there, so the delete
    /// has not happened, the sweep has not happened either, and when it runs
    /// it will find this connection in `subs`.
    ///
    /// ⚠ **Ordering is what makes this total, on both sides.** The re-read
    /// must stay AFTER the subscribe: before it, a revoke could delete and
    /// sweep between the read and the registration. And the helpers' delete
    /// must stay before their sweep. Either reorder reopens the window.
    ///
    /// ⚠ **One read per upgrade — never move it into dispatch.** Teaching the
    /// dispatch gate to read the token store would undo "validated exactly
    /// once, at the upgrade" on the hot path of every RPC of every connection
    /// (`transport-connection.md` § Connection lifecycle → *Revocation
    /// teardown* → *The per-token twin* → *The upgrade window*).
    ///
    /// Revoking a connection before `run_connection` starts is safe:
    /// [`crate::ws::RpcConnection::revoke`] stores the flag with
    /// `send_replace`, and `run_connection` checks it before it reads a frame,
    /// so the socket closes 4401 having dispatched nothing. A connection with
    /// **no** `token_id` has no row to re-read and is never closed here — it
    /// keeps the conservative resolution each teardown gives it.
    ///
    /// A connection **bound to a device** (`bound_device_key` — the bearer was
    /// minted by that device's granted key, `ws::RpcConnection::bound_device_key`)
    /// also marks the device's roster row `last_seen`: the row's meaning is
    /// *the last time a connection bound to this device registered with the
    /// nest* (`devices.md` § Listing Devices → *`last_seen_at`*), and this is
    /// that moment for the WS-RPC kind, as the data-plane handler's `register`
    /// is for the other. Best-effort: a DB fault here is logged and never
    /// refuses the connection, which was authorized before the 101.
    pub async fn register_upgraded_connection(
        &self,
        actor_id: [u8; 32],
        token_id: Option<String>,
        bound_device_key: Option<[u8; 32]>,
    ) -> (
        Arc<crate::ws::RpcConnection>,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        // Test-only rendezvous inside the upgrade window: the bearer was
        // validated in `ws_handler` and the 101 is out, but the connection is
        // not yet in `WsState.subs`. A test arms this to hold the upgrade here
        // while it runs an entire revoke, which turns a timing race into a
        // causal barrier (e2e-conventions.md § convention 14). It sits
        // immediately above the subscribe, so a re-read hoisted to the top of
        // this function lands above the park too and reads a row the revoke
        // has not yet deleted; a read moved only to between this park and the
        // subscribe is caught by the park inside the read itself
        // (`token_store::read_race`). `#[cfg(test)]` alone, so no artifact and no
        // integration test can reach it (§ convention 15, by construction) —
        // the same shape as `auth_core::mint_race`.
        #[cfg(test)]
        let barrier = match token_id.as_deref().and_then(upgrade_race::take) {
            Some(barrier) => {
                barrier.validated.notify_one();
                barrier.may_register.notified().await;
                Some(barrier)
            }
            None => None,
        };
        let (conn, rx) = self
            .ws
            .subscribe_with_session(actor_id, token_id, bound_device_key);
        if let Some(token_id) = conn.token_id.as_deref()
            && !self
                .auth
                .token_store
                .has_session(&fauna_core::identity::ActorId(actor_id), token_id)
                .await
        {
            tracing::info!(
                target: "auth",
                "a WS upgrade raced its own session's revocation; the connection was closed"
            );
            conn.revoke();
        }
        if let Some(key) = conn.bound_device_key.as_ref()
            && let Err(e) = self
                .db
                .touch_device_last_seen_by_principal(&actor_id, key)
                .await
        {
            tracing::warn!(
                actor = %hex::encode(actor_id),
                "a device-bound WS upgrade could not mark its row last_seen: {e:#}"
            );
        }
        #[cfg(test)]
        if let Some(barrier) = barrier {
            barrier.registered.notify_one();
        }
        (conn, rx)
    }

    /// Wall-clock seconds for the outbound queue's scheduling + retry math
    /// (`fetch_outbound_due` / `mark_outbound_failed` / `mark_outbound_
    /// bounced` / `enqueue_outbound_mail`). Real clock in production; under
    /// `test-hooks` an e2e-settable override (`0` = real) so the 4 h delay-
    /// warning and 5 d give-up boundaries are reachable without waiting.
    /// Every outbound time read goes through here so a row's `created_at`
    /// and the `now - created_at` elapsed math stay on one consistent clock.
    pub fn outbound_now(&self) -> i64 {
        #[cfg(feature = "test-hooks")]
        {
            let v = self
                .outbound_clock_override
                .load(std::sync::atomic::Ordering::Relaxed);
            if v != 0 {
                return v;
            }
        }
        crate::db::now_epoch_secs()
    }

    /// Wall-clock seconds for the inbound greylist (`check_greylist`). Real
    /// clock in production; under `test-hooks` it shares the single e2e clock
    /// override (`outbound_clock_override`, driven by `POST /api/v1/test/
    /// outbound/clock`) so a tier_3 e2e can fast-forward the 60 s / 4 h / 30 d
    /// greylist boundaries deterministically. Named separately from
    /// `outbound_now` so the inbound call site reads honestly; the shared
    /// override is fine because the e2e drives one clock at a time.
    pub fn greylist_now(&self) -> i64 {
        self.outbound_now()
    }

    /// Whether a genuine mail new-ingest seal site should resolve through the
    /// content-sealing-epochs schedule
    /// (`db::CacheDb::get_recipient_mail_seal_key`'s `epoch_sealing_enabled`
    /// param) — `fauna_mls::wrapped_blob::MAIL_EPOCH_SEALING_WRITE_DEFAULT`
    /// in production (`true` since the 2026-07-19 flip); the `test-hooks`
    /// force-on override (`POST /api/v1/test/content/epoch_sealing`) predates
    /// the flip and is now redundant-but-harmless. B4,
    /// content-sealing-epochs design § 6.
    pub fn epoch_sealing_enabled(&self) -> bool {
        #[cfg(feature = "test-hooks")]
        {
            if self
                .epoch_sealing_test_override
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return true;
            }
        }
        fauna_mls::wrapped_blob::MAIL_EPOCH_SEALING_WRITE_DEFAULT
    }

    /// Test-only: return the scripted MTA-STS lookup for `domain`, if one
    /// was installed via `POST /api/v1/test/outbound/mta-sts`. Consulted by
    /// `fetch_mta_sts_policy_handler` before the real `mta_sts_fetcher`.
    #[cfg(feature = "test-hooks")]
    pub fn test_mta_sts_override(
        &self,
        domain: &str,
    ) -> Option<fauna_mail::outbound::mta_sts::MtaStsLookup> {
        self.mta_sts_override
            .lock()
            .expect("mta_sts_override mutex poisoned")
            .get(domain)
            .cloned()
    }

    /// Test-only: return the scripted TLSA records for `mx_host`, if any were
    /// installed via `POST /api/v1/test/outbound/tlsa`. Consulted by
    /// `fetch_tlsa_handler` before the real `tlsa_resolver`.
    #[cfg(feature = "test-hooks")]
    pub fn test_tlsa_override(
        &self,
        mx_host: &str,
    ) -> Option<Vec<fauna_mail::outbound::dane::TlsaRecord>> {
        self.tlsa_override
            .lock()
            .expect("tlsa_override mutex poisoned")
            .get(mx_host)
            .cloned()
    }

    /// Test-only helper: swap the `storage` impl on a freshly-built `AppState`
    /// before it is wrapped in `Arc` + handed to `build_router`. Used by
    /// tests that need a `Storage` double (e.g. one whose seal-on-read declines)
    /// instead of the `SealedStorage` `AppState::for_test` installs.
    ///
    /// Uses `RwLock::get_mut` so no async lock acquisition is needed — the
    /// caller proves uniqueness via `&mut self` (state hasn't been shared yet).
    /// Naming matches `for_test` to flag it as test-only — integration tests
    /// (compiled as external consumers of the lib, `bins/fauna-nest/tests/*.rs`)
    /// need plain `pub` access under a `cargo test` (debug-profile) build, so
    /// this carries `debug_assertions` rather than bare `#[cfg(test)]`
    /// (convention 15 rule (a)).
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub fn install_storage_for_test(&mut self, storage: crate::storage::SharedStorage) {
        self.storage = storage;
    }

    /// Test-only helper: install the registry the app relay's two demand
    /// doors verify enrollment against (see `region_registry_override`'s own
    /// doc), before a freshly-built `AppState` is wrapped in `Arc`. Same
    /// `debug_assertions` gating and rationale as `install_storage_for_test`
    /// — integration tests need plain `pub` access under a `cargo test`
    /// (debug-profile) build.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub fn install_region_registry_for_test(
        &mut self,
        registry: fauna_core::region_authority::RegionRegistry,
    ) {
        self.region_registry_override = Some(registry);
    }
}

/// Static registration inputs that are **not** an admin policy choice.
///
/// The posture itself (open / invite-required / closed, plus the orthogonal
/// free-tier ceiling) lives in the client-set `AppState.registration_mode`
/// singleton — owner: `public-mode.md` § Registration Modes. What is left here
/// is the handle domain (derived from the nest's own domain) and the reserved-handle
/// deny-list (a constant), neither of which an admin picks from a client.
#[derive(Clone)]
pub struct RegistrationConfig {
    pub handle_domain: Option<String>,
    pub reserved_handles: Vec<String>,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            handle_domain: None,
            // The one hard-coded list (shared with fauna-router's pre-flight
            // checks; subset-pinned against the reserved web subdomain labels). A correctness constant, not a policy: no CLI flag or
            // config key feeds this field — the deleted `--reserved-handle`
            // flag's plumbing used to replace this list with the flag's empty
            // default on every production boot. The field itself stays only so
            // tests can inject a custom list.
            reserved_handles: fauna_protocol::handle::RESERVED_HANDLES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Try to extract an authenticated actor ID from the request headers.
/// Returns `None` if no valid Bearer token is present (does NOT reject the request).
pub(crate) async fn optional_bearer_auth(
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> Option<fauna_core::identity::ActorId> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))?;
    crate::auth::validate_bearer(state, token).await
}

/// Transport-independent outcome of delivering a signed `(ContactRequest, Post)`
/// inbox payload. The deleted `POST /api/v1/inbox/{actor}` HTTP twin and the
/// federation channel handler (`fauna.federation.inbox.deliver`,
/// `federation_handlers`) both call [`deliver_inbox_payload_core`] and map this
/// onto their own reply shape — only the federation mapping survives now that the
/// HTTP twin is gone (clients ride `fauna.inbox.send` → the federation leg).
#[derive(Debug)]
pub(crate) enum InboxDeliveryOutcome {
    /// Delivered to the recipient inbox; the new inbox row id (HTTP 201).
    Delivered(i64),
    /// A knock was stored — `allow_knock` mode, no prior contact (HTTP 202).
    KnockStored,
    /// Rejected by payload validation or inbox-mode policy (HTTP 4xx).
    Rejected(InboxRejection),
}

/// Why an inbox delivery was rejected. Variants map 1:1 onto the HTTP twin's
/// status codes (behaviour preserved across the core extraction).
#[derive(Debug)]
pub(crate) enum InboxRejection {
    /// Malformed payload or bad signature (HTTP 400).
    BadPayload(String),
    /// The sender is blocked by the recipient (HTTP 403).
    Blocked,
    /// The recipient's inbox is closed (HTTP 403).
    InboxClosed,
    /// The recipient accepts contacts only, sender is not a contact (HTTP 403).
    ContactsOnly,
    /// A knock from this sender is already pending (HTTP 409).
    KnockPending,
    /// Unrecognised inbox mode (HTTP 403).
    UnknownMode,
    /// Quota: recipient not registered or suspended (HTTP 403).
    QuotaForbidden(String),
    /// No account holds the (terminal) recipient id — deleted, or never
    /// registered here (HTTP 403, the same `forbidden` the quota leg gives an
    /// unregistered recipient; judged after the succession re-point).
    RecipientUnknown,
    /// Quota: the payload exceeds the recipient's quota (HTTP 413).
    QuotaTooLarge(String),
    /// A knocker-written field past its hard-coded cap
    /// (`MAX_KNOCK_SUMMARY_BYTES`, `MAX_KNOCK_SENDER_NODE_BYTES`) — refused
    /// whole, never truncated.
    KnockTooLarge(String),
    /// The recipient already holds `MAX_PENDING_KNOCKS_PER_RECIPIENT` knocks —
    /// a transient capacity refusal that clears as they act on the queue.
    KnockQueueFull,
    /// An internal storage error (HTTP 500).
    Storage,
}

/// The **disposition bucket** for an [`InboxRejection`] — which refusal is the
/// sender's fault, which is a permission answer, which is ours — stated once
/// here (next to the type it buckets, the one place that knows what each
/// variant means) so `inbox_handlers::rejection_to_error` and
/// `federation_handlers::map_inbox_rejection` can never disagree on WHICH
/// bucket a variant falls into when a variant is added.
///
/// The two legs are not simply namespace variants of one shared code per
/// bucket: `Forbidden` is a pure namespace split (`fauna.inbox.forbidden` vs
/// `fauna.federation.forbidden`) and `Internal` is byte-identical on both legs
/// (`fauna.protocol.internal`), but `MalformedClass` rides the client leg's
/// shared, un-namespaced `fauna.protocol.malformed` and the federation leg's
/// namespaced `fauna.federation.invalid_params` — a different code FAMILY, not
/// only a different namespace. Each leg still owns picking its own wire code
/// for a bucket; only the bucketing itself is shared.
pub(crate) enum InboxRejectionDisposition {
    /// A permission/policy refusal.
    Forbidden(String),
    /// The sender's own fault — bad payload, duplicate knock, oversize.
    MalformedClass(String),
    /// A transient capacity refusal — retry later. Byte-identical on both legs
    /// (`fauna.protocol.rate_limited`, the code the throttles already emit), so
    /// it needs no new client-facing error code.
    Capacity,
    /// An internal storage failure — never the sender's fault.
    Internal(String),
}

impl InboxRejection {
    /// Bucket this rejection for a wire mapper — see [`InboxRejectionDisposition`].
    pub(crate) fn disposition(self) -> InboxRejectionDisposition {
        use InboxRejectionDisposition::{Capacity, Forbidden, Internal, MalformedClass};
        match self {
            InboxRejection::BadPayload(m) => MalformedClass(m),
            InboxRejection::Blocked => Forbidden("blocked".to_string()),
            InboxRejection::InboxClosed => Forbidden("inbox closed".to_string()),
            InboxRejection::ContactsOnly => Forbidden("contacts only".to_string()),
            InboxRejection::KnockPending => MalformedClass("knock already pending".to_string()),
            InboxRejection::UnknownMode => Forbidden("unknown inbox mode".to_string()),
            InboxRejection::QuotaForbidden(m) => Forbidden(m),
            InboxRejection::RecipientUnknown => Forbidden("recipient not registered".to_string()),
            InboxRejection::QuotaTooLarge(m) => MalformedClass(m),
            InboxRejection::KnockTooLarge(m) => MalformedClass(m),
            InboxRejection::KnockQueueFull => Capacity,
            InboxRejection::Storage => Internal("inbox storage error".to_string()),
        }
    }
}

/// Where an arrival entered the nest. Re-exported from `fauna-core` so every
/// gate — inbox, Welcome plane, federation — names the ingress class identically
/// (priority #3). `federation_contact` gates federation-origin *initiation* only.
pub(crate) use fauna_core::data::ArrivalOrigin;

/// The nest-side composition of the shared routing floor: read the two facts the
/// **recipient's own nest** owns — the stored contact edge and the stored
/// guardian policy — and hand them to `fauna_core::data::supervised_reach_verdict`.
///
/// This is the choke point `family-safety.md` § Guardian policy pillar 1 demands.
/// **Every path that writes a recipient's inbox on behalf of a named sender must
/// pass through here**, or the pillar's "cannot be bypassed by using a different
/// client" is a claim the code does not keep. It reads nothing the sender
/// declares — not the post's `schema`, not the Welcome's `kind`.
///
/// Fails **closed**: a storage error is `Err(())`, which every caller maps onto a
/// rejection. Also returns the contact status so callers can distinguish a
/// blocked sender from a policy suppression.
pub(crate) async fn reach_floor(
    state: &AppState,
    recipient: &[u8; 32],
    sender: &[u8; 32],
    origin: ArrivalOrigin,
) -> Result<(ReachVerdict, Option<ContactStatus>), ()> {
    let status = match state.db.get_contact_status(recipient, sender).await {
        Ok(s) => s.as_deref().and_then(ContactStatus::from_wire),
        Err(e) => {
            tracing::error!("reach_floor get_contact_status error: {e}");
            return Err(());
        }
    };
    let supervised = match state.db.get_guardian_policy(recipient).await {
        Ok(p) => p.map(|p| SupervisedReach {
            contact_approval: p.contact_approval,
            federation_contact: p.federation_contact,
        }),
        Err(e) => {
            tracing::error!("reach_floor get_guardian_policy error: {e}");
            return Err(());
        }
    };
    Ok((supervised_reach_verdict(status, supervised, origin), status))
}

/// Decode + verify a signed `(ContactRequest, Post)` inbox payload and route it
/// to the recipient's inbox per their `InboxMode` — the shared core behind both
/// the HTTP `POST /api/v1/inbox/{actor}` twin and the
/// `fauna.federation.inbox.deliver` channel kind. Returns a transport-independent
/// [`InboxDeliveryOutcome`]; the caller maps it onto its own response. The two
/// payloads ride the embed-as-bytes wire shape under sign-over-CID per
/// `docs/goal/architecture/serialization.md` (Tasks 2.5–2.6).
pub(crate) async fn deliver_inbox_payload_core(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    body: &Bytes,
    origin: ArrivalOrigin,
) -> InboxDeliveryOutcome {
    use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes};
    let (cr_wire, post_wire): (EmbedAsBytes, EmbedAsBytes) = match canonical_decode(body) {
        Ok(pair) => pair,
        Err(e) => {
            return InboxDeliveryOutcome::Rejected(InboxRejection::BadPayload(format!(
                "invalid payload: {e}"
            )));
        }
    };
    let cr: ContactRequest = match decode_signed_bytes(&cr_wire.bytes) {
        Ok(c) => c,
        Err(e) => {
            return InboxDeliveryOutcome::Rejected(InboxRejection::BadPayload(format!(
                "decode CR: {e}"
            )));
        }
    };
    if let Err(e) = decode_signed_bytes::<Post>(&post_wire.bytes) {
        return InboxDeliveryOutcome::Rejected(InboxRejection::BadPayload(format!(
            "decode post: {e}"
        )));
    }

    // Verify payload signatures (CR + Post signatures, sender match, post_id match).
    if let Err(reason) = verify_inbox_payload(body) {
        return InboxDeliveryOutcome::Rejected(InboxRejection::BadPayload(reason));
    }

    let sender_id = cr.sender.0;

    // Supersession refusal for **new** content authored by a superseded key
    // (`identity-succession.md:71`). This path is the one that needs it most:
    // it has no authenticated caller at all — a federation peer relays bytes,
    // and `verify_inbox_payload` above establishes only that the *signature* is
    // good. Self-describing verification would therefore keep accepting a
    // stolen key's fresh posts forever; this table consult is the whole
    // difference.
    //
    // Historical content is untouched: this refuses an *arrival*, and nothing
    // rewrites what already landed (`identity-succession.md:100` — old-key
    // content is genuinely old-key content).
    //
    // Fails closed on a storage error, like every other gate on this path.
    match state.db.succession_for(&sender_id[..]).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            return InboxDeliveryOutcome::Rejected(InboxRejection::BadPayload(
                "sender identity has been superseded".into(),
            ));
        }
        Err(e) => {
            tracing::error!("inbox succession consult failed: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    }

    // The RECIPIENT's own supersession — the other half of the consult above,
    // resolved BEFORE the routing floor (ruled 2026-09-26,
    // `succession-repoint-axis.md` § Re-key scope, the delivery-plane
    // blockquote). A peer that has not propagated the statement goes on
    // addressing the retired id, and refusing it would black-hole mail in
    // flight during propagation, so the arrival is ACCEPTED — and landed where
    // the account now lives: the terminal hop of `succession_path`. Everything
    // below (`reach_floor`, the inbox mode, the contact status, quota, push,
    // the knock) then runs against the successor, which is what makes the
    // ordering load-bearing: a sender the successor has blocked must not reach
    // them by addressing the predecessor. Without this the retired id's
    // `inbox_modes` and `contacts` rows — both moved by the ceremony — fell
    // back to `allow_knock` and no edge, so the arrival became a knock plus a
    // notification on an identity nobody can sign in as. Not a wire change:
    // only the `inbox_id` the reply carries is the successor's row.
    //
    // Fails closed, like the sender consult.
    let recipient: [u8; 32] = match state.db.succession_path(&actor_id[..]).await {
        Ok(path) => match path.last() {
            None => *actor_id,
            Some(terminal) => match <[u8; 32]>::try_from(terminal.new_actor_id.as_slice()) {
                Ok(successor) => {
                    tracing::info!(
                        target: "recovery",
                        successor = %hex::encode(successor),
                        "re-pointed an inbox arrival addressed to a superseded recipient"
                    );
                    successor
                }
                Err(_) => {
                    tracing::error!("inbox recipient succession consult: malformed successor id");
                    return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
                }
            },
        },
        Err(e) => {
            tracing::error!("inbox recipient succession consult failed: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    };
    let actor_id = &recipient;

    // Existence, judged at the terminal id — for every recipient, re-pointed or
    // not. A successor since deleted took its predecessors with it
    // (`account-data-plane.md` § Nest-side requirements item 1) while
    // `actor_successions` is retained, so the walk above still lands here; and
    // a plain deleted (or never registered) recipient used to be stored as a
    // knock plus a notification on a ghost id — `store_knock` checks no quota
    // and `knocks` carries no FK. Same refusal the quota leg already gives an
    // unregistered recipient, so no new wire code.
    match state.db.is_actor_registered(actor_id).await {
        Ok(true) => {}
        Ok(false) => return InboxDeliveryOutcome::Rejected(InboxRejection::RecipientUnknown),
        Err(e) => {
            tracing::error!("inbox recipient existence check failed: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    }

    // The routing floor, ahead of every other routing decision on this path.
    //
    // There used to be a `schema == "group/v1"` exemption here that returned
    // straight to `deliver_to_inbox`, ahead of both this gate and the inbox-mode
    // routing below. `schema` is a free-form `String` the **sender** signs over
    // their **own** post — `verify_inbox_payload` constrains the signatures, the
    // sender↔author match, and the post id, and nothing else — so relabelling any
    // post `group/v1` defeated `contact_approval`, `federation_contact`, *and*
    // every recipient's `InboxMode`. A sender's self-declaration is not an
    // authorization decision. It is gone; nothing replaces it. What travels this
    // path is initiation, and initiation is exactly what the recipient's mode
    // and reach policy exist to mediate.
    let (verdict, status) = match reach_floor(state, actor_id, &sender_id, origin).await {
        Ok(v) => v,
        Err(()) => return InboxDeliveryOutcome::Rejected(InboxRejection::Storage),
    };
    match verdict {
        ReachVerdict::Suppress => {
            return if status == Some(ContactStatus::Blocked) {
                InboxDeliveryOutcome::Rejected(InboxRejection::Blocked)
            } else {
                // `federation_contact = off` — a cross-nest stranger is turned
                // away before any knock is stored, so they never surface in the
                // guardian's queue at all.
                InboxDeliveryOutcome::Rejected(InboxRejection::ContactsOnly)
            };
        }
        ReachVerdict::Knock => {
            // `contact_approval = on`: acceptance authority is the guardian's —
            // a stranger's arrival lands on the knock path even in `open` mode,
            // and the guardian reviews it via `fauna.family.approvals.*`.
            if status == Some(ContactStatus::Pending) {
                return InboxDeliveryOutcome::Rejected(InboxRejection::KnockPending);
            }
            return match state.db.has_pending_knock(actor_id, &sender_id).await {
                Ok(true) => InboxDeliveryOutcome::Rejected(InboxRejection::KnockPending),
                Ok(false) => store_knock(state, actor_id, &sender_id, &cr, body).await,
                Err(e) => {
                    tracing::error!("has_pending_knock error: {e}");
                    InboxDeliveryOutcome::Rejected(InboxRejection::Storage)
                }
            };
        }
        // The floor imposes nothing; the recipient's own inbox mode decides.
        ReachVerdict::Proceed => {}
    }

    // Look up recipient's inbox mode.
    let mode = match state.db.get_inbox_mode(actor_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("get_inbox_mode error: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    };

    match mode.as_str() {
        "open" => {
            if let Ok(Some(status)) = state.db.get_contact_status(actor_id, &sender_id).await
                && status == "blocked"
            {
                return InboxDeliveryOutcome::Rejected(InboxRejection::Blocked);
            }
            deliver_to_inbox(state, actor_id, &sender_id, body, true).await
        }
        "allow_knock" => match state.db.get_contact_status(actor_id, &sender_id).await {
            Ok(Some(status)) => match status.as_str() {
                "confirmed" | "accepted" => {
                    deliver_to_inbox(state, actor_id, &sender_id, body, true).await
                }
                "blocked" => InboxDeliveryOutcome::Rejected(InboxRejection::Blocked),
                "pending" => InboxDeliveryOutcome::Rejected(InboxRejection::KnockPending),
                // Unknown status, treat as no contact.
                _ => store_knock(state, actor_id, &sender_id, &cr, body).await,
            },
            Ok(None) => match state.db.has_pending_knock(actor_id, &sender_id).await {
                Ok(true) => InboxDeliveryOutcome::Rejected(InboxRejection::KnockPending),
                Ok(false) => store_knock(state, actor_id, &sender_id, &cr, body).await,
                Err(e) => {
                    tracing::error!("has_pending_knock error: {e}");
                    InboxDeliveryOutcome::Rejected(InboxRejection::Storage)
                }
            },
            Err(e) => {
                tracing::error!("get_contact_status error: {e}");
                InboxDeliveryOutcome::Rejected(InboxRejection::Storage)
            }
        },
        "contacts_only" => match state.db.get_contact_status(actor_id, &sender_id).await {
            Ok(Some(status)) if status == "confirmed" || status == "accepted" => {
                deliver_to_inbox(state, actor_id, &sender_id, body, true).await
            }
            Ok(_) => InboxDeliveryOutcome::Rejected(InboxRejection::ContactsOnly),
            Err(e) => {
                tracing::error!("get_contact_status error: {e}");
                InboxDeliveryOutcome::Rejected(InboxRejection::Storage)
            }
        },
        "closed" => InboxDeliveryOutcome::Rejected(InboxRejection::InboxClosed),
        // Unknown mode — reject rather than guess.
        _ => InboxDeliveryOutcome::Rejected(InboxRejection::UnknownMode),
    }
}

/// The largest arrival held inside a knock row for release on accept. A signed
/// `(ContactRequest, Post)` tuple is a control frame — text plus content-address
/// *references*, never media bytes — so this is far above any legitimate
/// payload. Past it the knock is stored bare and the held copy of the arrival is
/// dropped, exactly as every knocked arrival was before the payload was held.
///
/// This bounds the `payload` column only. The row's other knocker-written
/// columns are bounded by the refusals below, and the row count by
/// `MAX_PENDING_KNOCKS_PER_RECIPIENT` — together they are what keeps a stranger
/// from parking unbounded bytes on a recipient who has not accepted them.
const MAX_HELD_KNOCK_PAYLOAD_BYTES: usize = 16 * 1024;

/// The largest knock summary (`ContactRequest.summary`, bytes) the nest stores.
/// The summary is copied into `knocks.summary` and, through the knock's
/// notification row, into the permanent `notifications.summary` and
/// `notifications.body_args` columns, so it needs its own bound — the held
/// payload cap above never reaches it. The shared senders trim it to 80 bytes,
/// ellipsis included (`fauna_client_core::{group,email}`), and the anonymous
/// invite request's free-text cap is the same 500. An over-cap summary is
/// **refused**, never truncated: a silently trimmed sentence is a change the
/// sender cannot see.
pub(crate) const MAX_KNOCK_SUMMARY_BYTES: usize = 500;

/// The largest `ContactRequest.sender_node` (bytes) a knock stores — a URL,
/// written to `knocks.sender_node` for the knock's 90-day life. Refused past
/// it, like the summary.
pub(crate) const MAX_KNOCK_SENDER_NODE_BYTES: usize = 2048;

/// The most knocks one recipient holds at once. The per-(recipient, sender)
/// dedup is defeated by a fresh keypair per knock, and the federation throttle
/// by spreading the knocks over many sources, so this is the backstop a
/// distributed flood cannot evade. It is per recipient, not nest-wide, so a
/// flood aimed at one account fills that account's queue and nobody else's.
/// With the caps above a knock row is at most ~19 KiB, so a full queue is
/// ~19 MB — and nothing in it outlives `PENDING_KNOCK_TTL_SECS`. Past it a
/// knock is refused with `KnockQueueFull` until the recipient acts on the queue
/// or old knocks expire. 1 000 pending strangers is far past any real backlog.
pub(crate) const MAX_PENDING_KNOCKS_PER_RECIPIENT: usize = 1_000;

/// A knock's sentence as a catalog key plus data — the body both the knock push
/// and the knock's notification row carry. `message` is the knocker's own text:
/// it exists nowhere else on the row, so it rides as data
/// (`behavior/notifications.md` § Localized body).
pub(crate) fn knock_body(sender_id: &[u8; 32], message: &str) -> fauna_protocol::LocalizedText {
    fauna_protocol::LocalizedText::new("notifications.row_knock")
        .with_arg("sender", &hex::encode(sender_id)[..8])
        .with_arg("message", message)
}

/// Store a knock and notify via WebSocket. Returns 202.
///
/// `body` is the arrival that provoked the knock; it rides along in the knock
/// row so that accepting the sender delivers the very payload that knocked,
/// rather than dropping it and making the sender re-send
/// (`deliver_held_knock_payload`). This is what lets a supervised account's
/// guardian approve an arrival and have the ward actually receive it.
async fn store_knock(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    sender_id: &[u8; 32],
    cr: &ContactRequest,
    body: &Bytes,
) -> InboxDeliveryOutcome {
    // The knocker-written columns, bounded before anything is written — the
    // knock row, the pending edge and the notification row all or nothing.
    if cr.summary.len() > MAX_KNOCK_SUMMARY_BYTES {
        return InboxDeliveryOutcome::Rejected(InboxRejection::KnockTooLarge(format!(
            "knock summary exceeds {MAX_KNOCK_SUMMARY_BYTES} bytes"
        )));
    }
    if cr.sender_node.len() > MAX_KNOCK_SENDER_NODE_BYTES {
        return InboxDeliveryOutcome::Rejected(InboxRejection::KnockTooLarge(format!(
            "knock sender_node exceeds {MAX_KNOCK_SENDER_NODE_BYTES} bytes"
        )));
    }
    let held: &[u8] = if body.len() <= MAX_HELD_KNOCK_PAYLOAD_BYTES {
        body
    } else {
        tracing::warn!(
            len = body.len(),
            "knocked arrival exceeds the held-payload cap; storing a bare knock"
        );
        &[]
    };
    match state
        .db
        .push_knock_within_cap(
            actor_id,
            sender_id,
            &cr.sender_node,
            &cr.summary,
            held,
            MAX_PENDING_KNOCKS_PER_RECIPIENT,
        )
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::warn!("recipient's knock queue is full; refusing the knock");
            return InboxDeliveryOutcome::Rejected(InboxRejection::KnockQueueFull);
        }
        Err(e) => {
            tracing::error!("push_knock error: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    }
    if let Err(e) = state
        .db
        .upsert_contact(actor_id, sender_id, "pending")
        .await
    {
        tracing::error!("upsert_contact error: {e}");
        // Non-fatal: knock was stored, contact status is secondary
    }
    // One body for the knock push and the knock's notification row, so an
    // OS-level knock notification says exactly what the row says
    // (`behavior/notifications.md` § Localized body).
    let body = knock_body(sender_id, &cr.summary);
    state.ws.notify_push(
        actor_id,
        fauna_protocol::PushEvent::Knock(fauna_protocol::push_events::KnockPayload {
            sender_id: hex::encode(sender_id),
            summary: cr.summary.clone(),
            body: Some(body.clone()),
            ..Default::default()
        }),
    );

    // Best-effort push for offline recipients
    let preview = format!("New knock from {}", &hex::encode(sender_id)[..8]);
    crate::push::dispatch_offline_push(state, actor_id, "New knock", &preview, "/app/contacts");

    // Insert unified notification for the knock
    let now = fauna_core::data::Timestamp::now().as_i64();
    let notif_text = crate::db::notifications::NotificationText::localized(body);
    // A re-knock supersedes its own doorbell (`behavior/notifications.md`
    // § Retention, rule 3): without this, a standing `knock` row from the same
    // sender — an accepted request's, or one its dismiss missed — would make
    // `insert_notification`'s dedup swallow the new knock, and it would never
    // ring.
    if let Err(e) = state
        .db
        .delete_knock_notification(actor_id, sender_id)
        .await
    {
        tracing::warn!("delete_knock_notification before re-knock: {e}");
    }
    if let Ok(Some(notif_id)) = state
        .db
        .insert_notification(
            actor_id,
            &fauna_protocol::notifications::NotifType::Knock,
            "fauna",
            Some(sender_id),
            None,
            None,
            &notif_text,
            now,
        )
        .await
    {
        state.ws.notify_push(
            actor_id,
            fauna_protocol::PushEvent::Notification(
                fauna_protocol::push_events::NotificationPayload {
                    notification_id: notif_id,
                    notif_type: fauna_protocol::notifications::NotifType::Knock,
                    source: "fauna".into(),
                    sender_id: Some(hex::encode(sender_id)),
                    content_id: None,
                    summary: notif_text.summary().to_string(),
                    body: notif_text.body().cloned(),
                    timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                    extra: std::collections::BTreeMap::new(),
                },
            ),
        );
    }

    InboxDeliveryOutcome::KnockStored
}

/// Deliver a payload to the inbox with quota enforcement.
///
/// `promote_contact` promotes the edge to `confirmed` — "a message was exchanged"
/// — which is right for a live arrival. It is **false** when releasing a payload a
/// knock had been holding (`deliver_held_knock_payload`): that arrival predates
/// the acceptance, so counting it as an exchange would confirm every accepted
/// knock on the spot and quietly retire the `accepted` TTL.
async fn deliver_to_inbox(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    sender_id: &[u8; 32],
    body: &Bytes,
    promote_contact: bool,
) -> InboxDeliveryOutcome {
    // Wrap the signed (ContactRequest, Post) tuple in the canonical inbox
    // envelope (layer 1) so the shared drain dispatches by `kind`. The tuple
    // bytes ride through as the envelope payload verbatim; the client decodes
    // them after unwrapping. Everything stored / replicated / size-counted below
    // is the envelope, keeping the blob, inline payload, and replica identical.
    let envelope_bytes = match fauna_protocol::inbox::InboxEnvelope::contact_request(body.to_vec())
        .to_canonical_bytes()
    {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::error!("inbox envelope encode error: {e}");
            return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
        }
    };
    let blob_hash = if let Some(ps) = &state.payload_store {
        match ps.store(&envelope_bytes).await {
            Ok((_, hash)) => hash.map(|h| h.digest()),
            Err(e) => {
                tracing::error!("payload_store error: {e}");
                return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
            }
        }
    } else {
        None
    };

    // Tier-quota enforcement (off on the single-user desktop nest).
    if *state.enforce_tier_quotas.read().await {
        match state.db.check_quota(actor_id, envelope_bytes.len()).await {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not registered") || msg.contains("suspended") {
                    return InboxDeliveryOutcome::Rejected(InboxRejection::QuotaForbidden(msg));
                }
                return InboxDeliveryOutcome::Rejected(InboxRejection::QuotaTooLarge(msg));
            }
        }

        match state
            .db
            .push_inbox_with_quota(actor_id, &envelope_bytes, blob_hash.as_ref())
            .await
        {
            Ok(row_id) => {
                state.ws.notify_push(
                    actor_id,
                    fauna_protocol::PushEvent::InboxItem(
                        fauna_protocol::push_events::InboxItemPayload {
                            content_id: crate::ws::inbox_content_id_hex(&envelope_bytes),
                            kind_hint: None,
                            extra: std::collections::BTreeMap::new(),
                        },
                    ),
                );
                spawn_replicate_inbox(state.clone(), *actor_id, envelope_bytes.to_vec(), row_id);
                // Promote contact to confirmed (idempotent)
                if promote_contact
                    && let Err(e) = state.db.promote_to_confirmed(actor_id, sender_id).await
                {
                    tracing::warn!("promote_to_confirmed error: {e}");
                }
                // Best-effort push for offline recipients
                let preview = format!("New message from {}", &hex::encode(sender_id)[..8]);
                crate::push::dispatch_offline_push(
                    state,
                    actor_id,
                    "New message",
                    &preview,
                    "/app/conversations",
                );
                return InboxDeliveryOutcome::Delivered(row_id);
            }
            Err(e) => {
                tracing::error!("push_inbox error: {e}");
                return InboxDeliveryOutcome::Rejected(InboxRejection::Storage);
            }
        }
    }

    match state
        .db
        .push_inbox(actor_id, &envelope_bytes, blob_hash.as_ref())
        .await
    {
        Ok(row_id) => {
            state.ws.notify_push(
                actor_id,
                fauna_protocol::PushEvent::InboxItem(
                    fauna_protocol::push_events::InboxItemPayload {
                        content_id: crate::ws::inbox_content_id_hex(&envelope_bytes),
                        kind_hint: None,
                        extra: std::collections::BTreeMap::new(),
                    },
                ),
            );
            spawn_replicate_inbox(state.clone(), *actor_id, envelope_bytes.to_vec(), row_id);
            // Promote contact to confirmed (idempotent)
            if promote_contact
                && let Err(e) = state.db.promote_to_confirmed(actor_id, sender_id).await
            {
                tracing::warn!("promote_to_confirmed error: {e}");
            }
            // Best-effort push for offline recipients
            let preview = format!("New message from {}", &hex::encode(sender_id)[..8]);
            crate::push::dispatch_offline_push(
                state,
                actor_id,
                "New message",
                &preview,
                "/app/conversations",
            );
            InboxDeliveryOutcome::Delivered(row_id)
        }
        Err(e) => {
            tracing::error!("push_inbox error: {e}");
            InboxDeliveryOutcome::Rejected(InboxRejection::Storage)
        }
    }
}

/// Release the arrival a knock has been holding into the recipient's inbox,
/// now that `sender_id` has been accepted — by the recipient themselves, or by
/// the guardian of a supervised recipient (`fauna.family.approvals.decide`).
///
/// Called from `contacts_handlers::accept_contact_core`, the single accept core
/// every surface routes through. The stored bytes are re-verified from scratch
/// (`verify_inbox_payload`) rather than trusted because they sat in the DB: the
/// sender binding must hold at release exactly as it held at arrival, and a
/// release must never become a way to deliver a payload whose signatures no
/// longer check out. `sender_id` is the accepted peer, so a stored payload whose
/// `cr.sender` disagrees is dropped rather than delivered.
///
/// Best-effort by construction: a failure here leaves the contact accepted (the
/// user's decision stands) and merely means the held arrival was not delivered.
pub(crate) async fn deliver_held_knock_payload(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    sender_id: &[u8; 32],
    body: Bytes,
) {
    use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes};

    if let Err(reason) = verify_inbox_payload(&body) {
        tracing::warn!("held knock payload failed re-verification: {reason}");
        return;
    }
    let Ok((_cr_wire, post_wire)) = canonical_decode::<(EmbedAsBytes, EmbedAsBytes)>(&body) else {
        tracing::warn!("held knock payload no longer decodes");
        return;
    };
    let Ok(post) = decode_signed_bytes::<Post>(&post_wire.bytes) else {
        tracing::warn!("held knock payload post no longer decodes");
        return;
    };
    // `verify_inbox_payload` already bound `cr.sender == post.author`, so the
    // post's author IS the payload's authenticated sender.
    if post.author.0 != *sender_id {
        tracing::warn!("held knock payload sender does not match the accepted peer");
        return;
    }
    match deliver_to_inbox(state, actor_id, sender_id, &body, false).await {
        InboxDeliveryOutcome::Delivered(_) => {}
        outcome => tracing::warn!("held knock payload release failed: {outcome:?}"),
    }
}

/// Parse `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>`. Returns
/// `(subprotocol, token)` on success.
///
/// Per spec § 5.2: subprotocol is the canonical version tag; the bearer
/// token is supplied as a *second* protocol token in the same header.
/// Browsers can't set custom WebSocket headers, so the bearer is encoded
/// into the subprotocol negotiation.
///
/// Handles both wire forms:
///   - One comma-joined value:  `fauna.v1, bearer.<token>`  (tungstenite/browser)
///   - Two separate header values: `fauna.v1` + `bearer.<token>` (websocket-client/Python)
fn parse_subprotocol(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
    // Collect all Sec-WebSocket-Protocol header values and flatten any
    // comma-separated tokens within each value into a single list.
    let values: Vec<&str> = headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if values.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = values
        .iter()
        .flat_map(|v| v.split(','))
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if tokens.len() < 2 {
        return None;
    }
    let subprotocol = tokens[0].to_string();
    let token = tokens[1].strip_prefix("bearer.")?.to_string();
    if token.is_empty() {
        return None;
    }
    Some((subprotocol, token))
}

/// GET /api/v1/ws/{actor_id} — WebSocket upgrade. Per spec § 5.2.
///
/// Validates `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` before
/// accepting. The legacy `?token=` query parameter has been removed.
pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    Path(actor_id_hex): Path<String>,
    headers: axum::http::HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    ws: WebSocketUpgrade,
) -> Response {
    let actor_id = match parse_actor_id(&actor_id_hex) {
        Some(id) => id,
        None => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };

    let (subprotocol, token) = match parse_subprotocol(&headers) {
        Some(parts) => parts,
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "subprotocol header required",
            )
                .into_response();
        }
    };

    if subprotocol != "fauna.v1" {
        // 4426 = subprotocol mismatch. WebSocketUpgrade can't set custom
        // close codes pre-upgrade, so we surface as HTTP 426 + reject.
        return (
            axum::http::StatusCode::UPGRADE_REQUIRED,
            format!("subprotocol mismatch: {subprotocol}"),
        )
            .into_response();
    }

    // `validate_with_session`, not `validate`: the connection has to remember
    // WHICH SESSION its bearer was, or a per-token revocation cannot find the
    // sockets that bearer opened (`ws::RpcConnection::token_id`) — and WHICH
    // DEVICE minted it, or `fauna.sync.devices.list` cannot tell that the
    // device is online (`ws::RpcConnection::bound_device_key`). This is the
    // only place the raw bearer and its row are ever both in hand — the token
    // is dropped after the handshake, and the row is gone by the time a revoke
    // runs (or expired while the socket lives on).
    //
    // Through the one bearer validator, so the actor's standing is asked HERE,
    // before `register_upgraded_connection` subscribes the socket: a suspended
    // or locked-out actor's surviving bearer is refused 401 and never enters
    // `WsState.subs`, so no Push frame can reach it. The dispatch gate alone
    // refused its calls but let the socket register and receive the Push
    // stream the revocation teardown exists to end. The
    // registration's own `has_session` re-read stays after its subscribe
    // (`transport-connection.md` § *The per-token twin* → *The upgrade window*);
    // this consult does not replace it.
    //
    // A refused bearer spends the failed-credential throttle's
    // `(source × path actor)` bucket; past its budget the refusal becomes
    // `429` + `Retry-After`, which a native client holds every dial to this
    // nest on (`transport-connection.md` § Abuse posture → *The
    // failed-credential throttle*). Only refusals spend, so a valid bearer is
    // never answered by it.
    let session = match crate::auth::check_bearer_session(&state, &token).await {
        Ok(session) => session,
        Err(_) => {
            if state.failed_credential_throttle.note_refusal(
                crate::failed_credential_throttle::Surface::WsRpcUpgrade,
                peer_addr.map(|a| a.ip()),
                &actor_id,
            ) {
                return crate::failed_credential_throttle::too_many_requests();
            }
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };
    let token_actor = session.actor_id;
    let token_id = session.token_id;
    let bound_device_key = crate::ws::bound_device_key_for(&actor_id, session.minted_by_device);
    if token_actor.0 != actor_id {
        // Name BOTH halves. This refusal is the cross-connection binding gate
        // (`security.md` § Cross-connection binding), and a client that trips
        // it is always presenting a bearer minted for one identity on another
        // identity's connection — the signature of an app whose account
        // pointer and whose keypair disagree after an identity change. The
        // client sees only `403 Forbidden` and the reconnect supervisor backs
        // off (measured: five refusals climbing to a ~15 s ceiling), during
        // which every WS-RPC fails `rpc disconnected (was_in_flight=false)`
        // and each page paints empty-with-an-error — so the symptom surfaces
        // arbitrarily far from the cause, in whatever feature was reading.
        // Until now neither side recorded which two actors disagreed, which is
        // exactly the datum the diagnosis needs. Both are public actor ids,
        // never secret material. Reachable only with a VALID bearer, so this
        // is not an unauthenticated log-amplification surface.
        tracing::warn!(
            bearer_actor = %hex::encode(token_actor.0),
            path_actor = %hex::encode(actor_id),
            "WS upgrade refused: the bearer names a different actor than the connection",
        );
        return ApiError::forbidden("token does not match actor_id").into_response();
    }

    ws.max_message_size(MAX_WS_MESSAGE_SIZE)
        .max_frame_size(MAX_WS_MESSAGE_SIZE)
        .protocols(["fauna.v1"])
        .on_upgrade(move |socket| handle_ws(state, actor_id, token_id, bound_device_key, socket))
}

/// GET /api/v1/ws — anonymous (pre-identity) WebSocket upgrade. No `{actor_id}`
/// path segment and no `bearer.<token>` element required in
/// `Sec-WebSocket-Protocol`; the connection binds no actor and routes only the
/// `pre_identity_allowlist` kinds (auth bootstrap / discovery / admin claim).
/// Per transport.md § Pre-identity (anonymous) connection.
pub async fn ws_anonymous_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    ws: WebSocketUpgrade,
) -> Response {
    if !subprotocol_offers(&headers, "fauna.v1") {
        // No `fauna.v1` offered — same 426 shape as the authenticated endpoint.
        return (
            axum::http::StatusCode::UPGRADE_REQUIRED,
            "subprotocol fauna.v1 required",
        )
            .into_response();
    }
    // `peer_addr` is populated on both the plain-HTTP listener (the in-container
    // bridge↔nest hop) and the TLS path (`serve_tls` injects it via
    // `WithConnectInfo` — the real client IP when the SNI router conveys it with
    // a PROXY-v2 header, else the direct loopback peer). The dispatcher uses it
    // for per-source throttling and to gate loopback-only pre-identity kinds
    // (bridge self-enrollment).
    ws.max_message_size(MAX_WS_MESSAGE_SIZE)
        .max_frame_size(MAX_WS_MESSAGE_SIZE)
        .protocols(["fauna.v1"])
        .on_upgrade(move |socket| handle_ws_anonymous(state, socket, peer_addr))
}

/// True if `Sec-WebSocket-Protocol` offers `token`. Unlike `parse_subprotocol`,
/// no `bearer.<token>` element is required — shared by the anonymous
/// (`fauna.v1`) and federation (`FEDERATION_SUBPROTOCOL`) upgrade handlers,
/// neither of which binds a bearer-authenticated actor at upgrade time.
pub(crate) fn subprotocol_offers(headers: &axum::http::HeaderMap, token: &str) -> bool {
    use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|tok| tok.trim() == token)
}

async fn handle_ws(
    state: Arc<AppState>,
    actor_id: [u8; 32],
    token_id: String,
    bound_device_key: Option<[u8; 32]>,
    socket: axum::extract::ws::WebSocket,
) {
    use std::sync::Arc;
    let (conn, rx) = state
        .register_upgraded_connection(actor_id, Some(token_id), bound_device_key)
        .await;
    run_connection(Arc::clone(&state), Arc::clone(&conn), rx, socket).await;
    state.ws.remove(&actor_id, conn.conn_id);
}

/// The test-only rendezvous [`AppState::register_upgraded_connection`] offers,
/// so the window between the upgrade's bearer validation and the connection's
/// registration can be entered deterministically.
///
/// **Keyed by `token_id`, not task-local** like `auth_core::mint_race`: axum
/// runs `on_upgrade` on a task of its own, which a task-local set on the test's
/// task never reaches. The key gives the same isolation — every test mints its
/// own random session id, so an armed barrier can only ever catch the upgrade
/// it was armed for — and [`take`](upgrade_race::take) disarms it, so it fires
/// once.
///
/// Two one-shot signals rather than one, for `mint_race`'s reason: the upgrade
/// must both *announce* that it is parked and *wait* to be let through, and
/// `Notify::notify_one` stores a permit when nobody is waiting, so neither side
/// can miss the other by arriving first.
#[cfg(test)]
pub(crate) mod upgrade_race {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    pub(crate) struct Barrier {
        /// Raised by the upgrade once it is parked: validated, not registered.
        pub(crate) validated: Notify,
        /// Raised by the test once it is done inside the window.
        pub(crate) may_register: Notify,
        /// Raised by the upgrade once registration — subscribe and re-read —
        /// has finished, so a test can order a revoke's remainder after it.
        pub(crate) registered: Notify,
    }

    static ARMED: LazyLock<Mutex<HashMap<String, Arc<Barrier>>>> = LazyLock::new(Default::default);

    /// Hold the next upgrade of session `token_id` inside the window.
    pub(crate) fn arm(token_id: &str) -> Arc<Barrier> {
        let barrier = Arc::new(Barrier::default());
        ARMED
            .lock()
            .unwrap()
            .insert(token_id.to_string(), Arc::clone(&barrier));
        barrier
    }

    pub(super) fn take(token_id: &str) -> Option<Arc<Barrier>> {
        ARMED.lock().unwrap().remove(token_id)
    }
}

/// The test-only rendezvous each sub-actor revoke helper offers right after
/// its WS-RPC sweep ([`AppState::revoke_session_authority`],
/// [`AppState::revoke_other_sessions_authority`],
/// [`AppState::revoke_device_authority`]), so a test can register an upgrade
/// *after the sweep and before anything else the helper does*. That is the one
/// interleaving the helpers' delete-before-sweep ordering exists for: with the
/// delete already done, the registration's re-read finds the row gone; with a
/// reorder that put the delete after the sweep, the row is still there, the
/// re-read passes it, and the sweep has already missed it.
///
/// The call sits on the line right after the WS-RPC sweep, with nothing
/// between: glued to the sweep, a reorder that moves the delete below it
/// leaves the delete on the far side of the park.
///
/// Keyed by actor id, for [`upgrade_race`]'s reason (the helper runs on a task
/// the test spawns, which a task-local never reaches); every test generates
/// its own actor, and [`after_sweep`] disarms on first use.
#[cfg(test)]
pub(crate) mod revoke_race {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    pub(crate) struct Barrier {
        /// Raised by the helper once its sweep has run.
        pub(crate) swept: Notify,
        /// Raised by the test once it is done after the sweep.
        pub(crate) may_finish: Notify,
    }

    static ARMED: LazyLock<Mutex<HashMap<[u8; 32], Arc<Barrier>>>> =
        LazyLock::new(Default::default);

    /// Hold the next sub-actor revoke of `actor_id` just after its sweep.
    pub(crate) fn arm(actor_id: [u8; 32]) -> Arc<Barrier> {
        let barrier = Arc::new(Barrier::default());
        ARMED.lock().unwrap().insert(actor_id, Arc::clone(&barrier));
        barrier
    }

    pub(super) async fn after_sweep(actor_id: &[u8; 32]) {
        let armed = ARMED.lock().unwrap().remove(actor_id);
        if let Some(barrier) = armed {
            barrier.swept.notify_one();
            barrier.may_finish.notified().await;
        }
    }
}

/// Drive the anonymous (pre-identity) connection. It is not in `WsState.subs`
/// (no Push routing), so there is nothing to `remove` on teardown — the
/// dispatcher gates it against `pre_identity_allowlist`. Per transport.md
/// § Pre-identity (anonymous) connection.
async fn handle_ws_anonymous(
    state: Arc<AppState>,
    socket: axum::extract::ws::WebSocket,
    peer_addr: Option<std::net::SocketAddr>,
) {
    let (conn, rx) = state.ws.subscribe_anonymous(peer_addr);
    run_connection(state, conn, rx, socket).await;
}

/// The per-connection driver shared by the authenticated and anonymous
/// endpoints: outbound forwarder + ResyncRequired coalesce timer (a no-op for
/// anonymous connections, which never receive Push frames) + inbound dispatch
/// loop. Returns when the inbound side closes — or, on a graceful shutdown
/// (`WsState::begin_shutdown`), after the sender drains in-flight replies and
/// closes the socket with WS 1001. Per `transport.md` § Graceful shutdown.
pub(crate) async fn run_connection(
    state: Arc<AppState>,
    conn: Arc<crate::ws::RpcConnection>,
    mut rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    socket: axum::extract::ws::WebSocket,
) {
    use axum::extract::ws::Message;
    use std::sync::Arc;

    // Server half of the spec heartbeat (`transport.md` § Connection lifecycle).
    // The send task pings; the dispatch task holds the deadline that any inbound
    // frame re-arms. Both halves read one policy so they cannot drift apart.
    let heartbeat = state.ws.heartbeat();

    let (sink, ws_rx) = socket.split();

    // Outbound forwarder: rx → WebSocket writer. It is the sole owner of the
    // socket sink (so the 1001 close below can't race another writer). On a
    // graceful shutdown it drains the in-flight replies still landing in `rx`,
    // then closes with WS 1001 (Going Away) — never 1000, which would stop the
    // client's reconnect loop on every redeploy (`transport.md` § Close codes).
    let send_conn = Arc::clone(&conn);
    let mut send_shutdown = state.ws.subscribe_shutdown();
    let mut send_revoked = conn.subscribe_revoked();
    let send_spared = conn.subscribe_spared();
    let mut send_fatal = conn.subscribe_fatal();
    // spawn-ok(connection-scoped): the drain mechanism *is* this task —
    // `begin_shutdown` flips `send_shutdown`, which the select below answers by
    // draining in-flight replies and closing WS 1001. Named per the drain-reach
    // rule; `teardown_serving_generation` waits on `connection_count()` for it.
    let send_task = tokio::spawn(async move {
        let mut sink = sink;
        // A connection that arrives *after* shutdown began still gets a clean
        // 1001 rather than hanging (the `changed()` arm below would otherwise
        // wait for a flip that already happened). Same for a revocation that
        // raced the upgrade — the bearer was validated microseconds before
        // `revoke_actor` ran, so this socket must not survive it.
        if *send_shutdown.borrow() {
            drain_and_close(&mut sink, &mut rx, &send_conn).await;
            return;
        }
        if *send_revoked.borrow() {
            close_revoked(&mut sink).await;
            return;
        }
        // Same race as the two above: a violation raised between `subscribe_fatal`
        // and this loop would never fire `changed()`, because `subscribe` marks
        // the value seen.
        // Copied out of the guard before any `.await`: a `watch::Ref` held
        // across a suspend point makes the whole task future `!Send`.
        let pre_fatal = *send_fatal.borrow();
        if let Some(reason) = pre_fatal {
            close_fatal(&mut sink, reason).await;
            return;
        }
        // Nest→client Ping cadence. `Delay` rather than the default `Burst`
        // because a runtime stall must not produce a rapid volley of Pings once
        // the task is scheduled again — one Ping per elapsed window is the whole
        // signal (the client adapter coalesces its own ticks for the same
        // reason). The first tick fires immediately, so skip it: the first Ping
        // should land one interval into the connection, not at handshake time.
        let mut ping_tick = tokio::time::interval(heartbeat.ping_interval);
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping_tick.tick().await;
        loop {
            tokio::select! {
                biased;
                // Revocation outranks a pending reply: an emergency lockout must
                // cut the socket now, not after the outbound queue flushes.
                changed = send_revoked.changed() => {
                    if changed.is_ok() && *send_revoked.borrow() {
                        close_revoked(&mut sink).await;
                        return;
                    }
                }
                // A fatal fault outranks a pending reply for the same reason
                // revocation does — and for `ReplyOverflow` specifically,
                // draining first would be incoherent: the queue overflowed
                // precisely because the peer stopped reading it.
                changed = send_fatal.changed() => {
                    let reason = if changed.is_ok() { *send_fatal.borrow() } else { None };
                    if let Some(reason) = reason {
                        close_fatal(&mut sink, reason).await;
                        return;
                    }
                }
                maybe = rx.recv() => match maybe {
                    Some(payload) => {
                        // The one-frame grace (`RpcConnection::revoke_after_reply`):
                        // while a spared correlation id is set, every frame is
                        // checked for being *the* answer. The decode is paid only
                        // inside that window, which is at most a handful of frames
                        // at the very end of one connection's life.
                        let closes_after = match *send_spared.borrow() {
                            Some(corr) => frame_is_reply_for(&payload, corr),
                            None => false,
                        };
                        if sink.send(Message::Binary(payload)).await.is_err() {
                            return;
                        }
                        if closes_after {
                            // Flip the ordinary revocation flag now that the debt
                            // is paid, so the inbound loop tears down and
                            // `run_connection` takes its `is_revoked()` branch —
                            // the teardown from here on is the unsparing one,
                            // byte for byte.
                            send_conn.revoke();
                            close_revoked(&mut sink).await;
                            return;
                        }
                    }
                    None => return, // outbound channel closed: connection torn down
                },
                // The heartbeat itself. Ranked below outbound replies (a Ping is
                // never more urgent than the payload the peer is waiting for) but
                // above the shutdown watch, which only ever ends the loop.
                _ = ping_tick.tick() => {
                    if sink.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                        return;
                    }
                }
                changed = send_shutdown.changed() => {
                    if changed.is_ok() && *send_shutdown.borrow() {
                        drain_and_close(&mut sink, &mut rx, &send_conn).await;
                        return;
                    }
                    // `changed` Err = AppState dropped (process teardown); the
                    // `rx` arm observes the close. Otherwise loop.
                }
            }
        }
    });

    // Per-connection ResyncRequired coalesce timer.
    let resync_conn = Arc::clone(&conn);
    // spawn-ok(connection-scoped): reached transitively — the send task above
    // owns `rx`, so its 1001 close drops the receiver and this loop's
    // `ws_tx.is_closed()` check breaks within one 5s tick. It holds no
    // `AppState` and no key material, only the connection.
    let resync_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        // First tick fires immediately; skip it.
        tick.tick().await;
        loop {
            tick.tick().await;
            crate::ws::WsState::flush_resync_if_needed(&resync_conn);
            if resync_conn.ws_tx.is_closed() {
                break;
            }
        }
    });

    // Inbound dispatch loop. Ends on client close, protocol violation, or the
    // shutdown signal — once shutting down it stops reading so the drain above
    // isn't fed new requests and converges promptly.
    let dispatch_state = Arc::clone(&state);
    let dispatch_conn = Arc::clone(&conn);
    let mut dispatch_shutdown = state.ws.subscribe_shutdown();
    let mut dispatch_revoked = conn.subscribe_revoked();
    let mut dispatch_fatal = conn.subscribe_fatal();
    // spawn-ok(connection-scoped): subscribes `dispatch_shutdown` and stops
    // reading the moment `begin_shutdown` flips, which is what lets the send
    // task's drain converge. Same named mechanism as the send task above.
    let dispatch_task = tokio::spawn(async move {
        let mut ws_rx = ws_rx;
        if *dispatch_shutdown.borrow()
            || *dispatch_revoked.borrow()
            || dispatch_fatal.borrow().is_some()
        {
            return;
        }
        // Dead-link detection. Absolute (`sleep_until`, not `sleep`) so the
        // window measures silence from the peer rather than restarting on every
        // pass of this loop; every inbound frame — the Pong to our Ping, or any
        // other traffic — re-arms it just below.
        let mut liveness_deadline = tokio::time::Instant::now() + heartbeat.liveness_timeout;
        loop {
            let msg = tokio::select! {
                biased;
                changed = dispatch_revoked.changed() => {
                    if changed.is_err() || *dispatch_revoked.borrow() {
                        break; // revoked: dispatch nothing further for this actor
                    }
                    continue;
                }
                // A fatal fault raised by a *handler* (a Reply that overflowed
                // the outbound queue) has no other way to reach this loop, which
                // would otherwise sit in `ws_rx.next()` waiting on a peer that
                // has stopped reading — and `run_connection` below is waiting on
                // this task before it lets the close frame go out.
                changed = dispatch_fatal.changed() => {
                    if changed.is_err() || dispatch_fatal.borrow().is_some() {
                        break;
                    }
                    continue;
                }
                msg = ws_rx.next() => msg,
                // No frame of any kind for a full liveness window: the peer is
                // gone. Treated as the disconnect it is — `break` tears the
                // connection down exactly like a client close, so no new close
                // code reaches the wire (a peer this silent would never read one
                // anyway). Logged at `warn` deliberately: a connection reaped in
                // silence is indistinguishable from a quiet night, which is how
                // the leak this closes stayed invisible in production for weeks.
                _ = tokio::time::sleep_until(liveness_deadline) => {
                    tracing::warn!(
                        actor = %hex::encode(dispatch_conn.actor_id),
                        timeout_ms = heartbeat.liveness_timeout.as_millis() as u64,
                        "WS peer answered no heartbeat within the liveness window; closing dead link",
                    );
                    break;
                }
                changed = dispatch_shutdown.changed() => {
                    if changed.is_err() || *dispatch_shutdown.borrow() {
                        break; // shutting down: stop accepting new requests
                    }
                    continue;
                }
            };
            // Any inbound frame proves the peer is alive, so re-arm before the
            // frame is even inspected — a Pong or a stray Ping counts exactly as
            // much as a request here.
            liveness_deadline = tokio::time::Instant::now() + heartbeat.liveness_timeout;
            let Some(Ok(msg)) = msg else { break };
            match msg {
                Message::Binary(bytes) => {
                    match fauna_protocol::decode_frame(&bytes) {
                        // A connection under the one-frame grace has already lost
                        // its authority — the grace buys the answer to the request
                        // that took it away, never the right to ask another. Same
                        // refusal the `revoked` arm above performs, at the one
                        // place a *spared* connection can still reach.
                        Ok(fauna_protocol::Frame::Request(_))
                            if dispatch_conn.spared_until().is_some() => {}
                        Ok(fauna_protocol::Frame::Request(req)) => {
                            dispatch_request(
                                Arc::clone(&dispatch_state),
                                Arc::clone(&dispatch_conn),
                                req,
                            )
                            .await;
                        }
                        Ok(fauna_protocol::Frame::Cancel(c)) => {
                            dispatch_cancel(&dispatch_conn, c).await;
                        }
                        Ok(_) => {
                            // Reply/Push from client = protocol violation.
                            tracing::warn!(
                                actor = %hex::encode(dispatch_conn.actor_id),
                                "protocol violation: client sent Reply or Push frame; closing"
                            );
                            dispatch_conn.signal_fatal(FatalCloseReason::ProtocolViolation);
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                actor = %hex::encode(dispatch_conn.actor_id),
                                error = %e,
                                "malformed WS frame; closing"
                            );
                            dispatch_conn.signal_fatal(FatalCloseReason::ProtocolViolation);
                            break;
                        }
                    }
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {
                    // axum handles ping/pong at framing layer; ignore here.
                }
                Message::Text(_) => {
                    // No text frames in Spec Y; treat as protocol violation.
                    tracing::warn!(
                        actor = %hex::encode(dispatch_conn.actor_id),
                        "protocol violation: client sent text frame; closing"
                    );
                    dispatch_conn.signal_fatal(FatalCloseReason::ProtocolViolation);
                    break;
                }
            }
        }
    });

    // Wait for inbound to end (client close, protocol violation, shutdown, or
    // revocation).
    let _ = dispatch_task.await;
    if state.ws.is_shutting_down() {
        // Planned shutdown: let the sender finish its in-flight drain + 1001
        // close (bounded by the grace) instead of aborting it mid-close.
        let _ = tokio::time::timeout(
            crate::ws::SHUTDOWN_DRAIN_GRACE + std::time::Duration::from_secs(1),
            send_task,
        )
        .await;
    } else if conn.is_revoked() {
        // Revoked: no drain, but the sender still needs a moment to put the 4401
        // close frame on the wire. Aborting here instead would drop it, and the
        // client would see an ordinary transport drop — reconnecting with the
        // bearer it should have discarded.
        let _ = tokio::time::timeout(REVOKED_CLOSE_GRACE, send_task).await;
    } else if conn.fatal_close().is_some() {
        // Same reasoning as the revoked branch: no drain, but the sender needs a
        // moment to put the 4400/1011 frame on the wire. Aborting here instead
        // would drop it and the client would see an ordinary transport drop —
        // which is exactly the observability gap this path exists to close.
        let _ = tokio::time::timeout(REVOKED_CLOSE_GRACE, send_task).await;
    } else {
        send_task.abort();
    }
    resync_task.abort();
}

/// Is this outbound frame the `Reply` for `correlation_id`?
///
/// Used by the outbound task to recognise the one frame a spared connection is
/// being held open for (`RpcConnection::revoke_after_reply`). A frame that does
/// not decode is not that Reply — an undecodable frame is the nest's own bug and
/// not something to hold a revoked socket open over.
fn frame_is_reply_for(payload: &bytes::Bytes, correlation_id: u64) -> bool {
    matches!(
        fauna_protocol::decode_frame(payload),
        Ok(fauna_protocol::Frame::Reply(r)) if r.correlation_id == correlation_id
    )
}

/// How long `run_connection` waits for the outbound task to emit its 4401 close
/// frame after a revocation. The frame is a single small write on an already-open
/// socket, so this only has to cover a peer that has stopped reading.
const REVOKED_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Close a revoked actor's socket with WS **4401** (auth expired / invalid).
///
/// Every app maps 4401 to `clear_token()` → `ensure_auth()` → reconnect
/// (`libs/fauna-ws-substrate/src/adapter.rs`, `libs/fauna-rpc-wasm/src/adapter.rs`),
/// which is exactly right for a revoked actor: the bearer it holds is now
/// worthless, and the re-mint that follows is refused by `auth_core` (suspended /
/// locked out / no `users` row), so the supervisor backs off rather than
/// spinning. Emitting `1001` here would instead tell the client to reconnect
/// with the same dead bearer. Per `transport.md` § Connection lifecycle.
///
/// Unlike [`drain_and_close`] this does not flush pending replies: revocation is
/// an emergency control, and the actor has just lost the authority under which
/// those replies were computed.
async fn close_revoked(
    sink: &mut futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
) {
    use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code: 4401,
            reason: Utf8Bytes::from_static("authority revoked"),
        })))
        .await;
}

/// Close a faulted socket with the WS code its [`FatalCloseReason`] names —
/// **4400** for a protocol violation, **1011** for a Reply-channel overflow.
///
/// Both rows have been in `transport.md` § Close codes since Spec Y with no
/// nest-side producer: the dispatch loop just `break`ed and `run_connection`
/// aborted the send task, so the client saw an abrupt drop. That was never a
/// *functional* break — `ReconnectSignal::from_close`
/// (`libs/fauna-ws-substrate/src/adapter.rs`, and its wasm twin) buckets 4400,
/// 1011, 1006 and a missing close frame identically into `Retry` — but it left
/// an admin reading nest logs or a wire capture unable to tell "this client
/// is sending garbage" from "the network blipped". Same shape as the 4401 fix
/// this table's history section records as prior art.
///
/// Like [`close_revoked`] and unlike [`drain_and_close`], this does not flush
/// pending replies. For `ReplyOverflow` draining is not merely skipped but
/// incoherent — the queue overflowed because the peer stopped reading it.
async fn close_fatal(
    sink: &mut futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
    reason: crate::ws::FatalCloseReason,
) {
    use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code: reason.code(),
            reason: Utf8Bytes::from_static(reason.text()),
        })))
        .await;
}

/// On a graceful shutdown, flush the replies still landing in `rx` from
/// in-flight handlers, then close the socket with WS 1001 (Going Away). Bounded
/// by [`crate::ws::SHUTDOWN_DRAIN_GRACE`] so one stuck handler can't exceed the
/// container stop-grace. Per `transport.md` § Graceful shutdown.
async fn drain_and_close(
    sink: &mut futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
    rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>,
    conn: &std::sync::Arc<crate::ws::RpcConnection>,
) {
    use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};

    const POLL: std::time::Duration = std::time::Duration::from_millis(50);
    let deadline = tokio::time::Instant::now() + crate::ws::SHUTDOWN_DRAIN_GRACE;
    loop {
        match tokio::time::timeout(POLL, rx.recv()).await {
            Ok(Some(payload)) => {
                if sink.send(Message::Binary(payload)).await.is_err() {
                    return; // socket gone; nothing left to close
                }
            }
            Ok(None) => break, // outbound channel closed
            Err(_) => {
                // Idle for a poll interval — done once no handler is in flight.
                if conn.pending_handlers.lock().await.is_empty() {
                    break;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    // Final non-blocking sweep: a reply that landed in the narrow window between
    // a handler clearing `pending_handlers` and pushing its reply must not be
    // raced by the close frame.
    while let Ok(payload) = rx.try_recv() {
        if sink.send(Message::Binary(payload)).await.is_err() {
            return;
        }
    }
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code: 1001,
            reason: Utf8Bytes::from_static("server shutdown"),
        })))
        .await;
}

// ── Dispatch helpers — full implementations in Tasks 13–14. ─────────

async fn dispatch_request(
    state: Arc<AppState>,
    conn: Arc<crate::ws::RpcConnection>,
    req: fauna_protocol::Request,
) {
    use fauna_protocol::RpcError;

    let correlation_id = req.correlation_id;
    let kind = req.kind.clone();
    let idempotency_key = req.idempotency_key;

    // (1) Idempotency cache check (shared dispatch core). A Hit replays the
    // cached Reply frame; TooLarge emits `replay_too_large`; either way we stop.
    if crate::dispatch_core::check_idempotent(&conn, correlation_id, idempotency_key)
        .await
        .is_replayed()
    {
        return;
    }

    // (1a) A principal session (`GET /api/v1/principal/ws`) dispatches nothing
    // through the gates below or the actor handlers: every request it carries
    // runs the principal gate — the binding re-resolved, the `ThirdParty`
    // ceiling, `scope_covers` — and the principal handler table
    // (`principal_handlers::dispatch_principal`). Its actor slot is the zero
    // placeholder, so were it ever to fall through, (1d) would resolve no
    // class and refuse: the failure is a refusal, never the account's reach.
    // The kind's wire metadata (deadline, unknown_kind) stays the router's.
    if let Some(binding) = conn.principal.clone() {
        crate::dispatch_core::spawn_dispatch(
            state,
            conn,
            rpc_router_meta,
            crate::dispatch_core::DispatchSubject::Principal(binding),
            req,
        )
        .await;
        return;
    }

    // (1b) Pre-identity gate. An anonymous (no-bearer) connection may invoke
    // only the fixed pre-identity allowlist; any other kind — including a
    // registered authenticated kind — gets `unauthenticated` and the
    // connection stays open (same shape as `unknown_kind`, so one disallowed
    // request can't kill an in-flight bootstrap). Per transport.md
    // § Pre-identity (anonymous) connection. Authenticated connections skip
    // this gate entirely (per-handler `require_permission` does class checks).
    if conn.anonymous && !crate::pre_identity_allowlist::is_pre_identity_kind(&kind) {
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new(
                "fauna.protocol.unauthenticated",
                "error.protocol.unauthenticated",
            ),
        )
        .await;
        return;
    }

    // (1b′) Generic anonymous-surface throttle. Two classes ride it, for two
    // reasons (`pre_identity_allowlist::is_throttled_anonymous_kind` owns the
    // set): the world-readable oracles (`by_handle` / `handle.available` /
    // `nest.resolve` / `nest.info` / the two recovery directory reads) carry no
    // signature, so a per-source sliding-window limit is the primary DoS /
    // directory-enumeration defense (`federation.md` § Security); the
    // signature-bound recovery ceremonies (escrow / veto / succession / the
    // emergency `account.lockout`) ride it because *being* signature-gated is
    // what makes the unmetered Ed25519 verify conscriptable. Keyed on
    // `conn.peer_addr` (the real client IP: the SNI router fronting :443 conveys
    // it via a PROXY-v2 header, parsed by `serve_tls` into `WithConnectInfo`) +
    // the kind class. A trip returns a `rate_limited`
    // RpcError and leaves the connection open (same shape as the gates above).
    //
    // ⚠ This comment used to end "Authenticated connections never reach an
    // anonymous kind, so this is anonymous-only by construction" — FALSE, and
    // refuted 180 lines below by gate (1d), which exempts pre-identity kinds
    // from the class allowlist in its own words: "an authed connection may still
    // call them". Every kind this gate guards is pre-identity (mechanically:
    // `is_throttled_anonymous_kind`'s 14 members are a strict subset of
    // `is_pre_identity_kind`'s 30), so while this gate was also scoped
    // `conn.anonymous &&`, one account converted all of them to unlimited. The
    // gate now binds the KIND on every connection class; `check_conn` picks the
    // bucket (IP for anonymous, `actor_id` for authenticated) rather than
    // skipping the check. Fixed 2026-08-24.
    if crate::pre_identity_allowlist::is_throttled_anonymous_kind(&kind)
        && !crate::anonymous_rate_limit::check_conn(&state.anonymous_rate_limit, &conn, &kind)
    {
        tracing::debug!(
            kind = %kind,
            peer = ?conn.peer_addr,
            "anonymous discovery surface rate limit exceeded"
        );
        crate::anonymous_rate_limit::report_shed(
            &state.anonymous_rate_limit_shed,
            "anonymous_discovery",
            crate::anonymous_rate_limit::ShedKey::of(&conn),
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited"),
        )
        .await;
        return;
    }

    // (1b″) Admin-claim attempt throttle. `fauna.auth.claim_admin` is gated by a
    // short deploy-time claim code; unthrottled, a fresh unclaimed nest is
    // brute-forceable into an admin takeover (spec § 8.5). Two limiters, both
    // separate from the discovery limiter (this surface wants a far smaller
    // budget): a tight **per-source** limit (`claim_rate_limit`) defeats the
    // single-source automated guess, and a **global** source-independent cap
    // (`global_claim_rate_limit`) bounds a *distributed* (botnet) brute-force
    // that the per-source limit alone cannot. The legitimate admin's one
    // successful claim passes both (the global cap is generous; a flood can only
    // delay it). The `||` short-circuits so a per-source trip does NOT consume
    // global budget. Keyed per class by `check_conn`; a trip returns
    // `rate_limited`, connection stays open.
    //
    // ⚠ Binds on EVERY connection class, and must. `federation.md` § Security
    // ties the deliberate 128→40-bit claim-code reduction to these two caps —
    // "this makes the `claim_admin` throttles the primary bound again, not
    // defense-in-depth", with the binding consequence that "neither throttle may
    // be loosened or removed without first restoring the code length". A cap
    // skipped for a whole connection class is not loosened, it is absent, and
    // (1d) below lets an authenticated connection call `claim_admin`. Latent
    // rather than exploitable while `DEFAULT_REGISTRATION_MODE` is `Closed` (no
    // authenticated connection exists pre-claim) — but that is a default in
    // another crate that nothing tied to this gate. Fixed 2026-08-24.
    if kind == "fauna.auth.claim_admin"
        && (!crate::anonymous_rate_limit::check_conn(&state.claim_rate_limit, &conn, &kind)
            || !crate::anonymous_rate_limit::check_global(&state.global_claim_rate_limit, &kind))
    {
        tracing::debug!(
            peer = ?conn.peer_addr,
            "admin-claim attempt rate limit exceeded"
        );
        crate::anonymous_rate_limit::report_shed(
            &state.claim_rate_limit_shed,
            "claim_admin",
            crate::anonymous_rate_limit::ShedKey::of(&conn),
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited"),
        )
        .await;
        return;
    }

    // (1b‴) Invite-code verify throttle. `fauna.account.invite_code.verify` is
    // the same anonymous secret-guess surface class as the admin claim (lower
    // severity — a valid guess yields an account, not admin). A per-source limit
    // (`invite_verify_rate_limit`, separate budget) bounds enumeration; no global
    // cap is warranted because the invite code is far stronger than the claim
    // code (~49 bits). Keyed per class by `check_conn`; a trip returns
    // `rate_limited`, connection stays open. Binds on every connection class —
    // this kind is pre-identity, so (1d) lets an authenticated caller reach it,
    // and an account is exactly the thing invite-code guessing wants to mint.
    if kind == "fauna.account.invite_code.verify"
        && !crate::anonymous_rate_limit::check_conn(&state.invite_verify_rate_limit, &conn, &kind)
    {
        tracing::debug!(
            peer = ?conn.peer_addr,
            "invite-code verify rate limit exceeded"
        );
        crate::anonymous_rate_limit::report_shed(
            &state.invite_verify_rate_limit_shed,
            "invite_code_verify",
            crate::anonymous_rate_limit::ShedKey::of(&conn),
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited"),
        )
        .await;
        return;
    }

    // (1b⁗) Registration throttle. `fauna.account.register` is an anonymous,
    // signature-bound *write* that creates an account; unthrottled it permits
    // registration floods / handle exhaustion / spam actors on a `public`+open
    // nest, plus an unmetered Ed25519 verify per attempt (security review § D10).
    // A per-source limit (`register_rate_limit`, separate budget) restores the
    // per-IP bound the retired HTTP twin carried — `registration_limiter` was
    // dead code that never re-wired it. Keyed per class by `check_conn` (for an
    // anonymous caller the real client IP, now that the SNI router conveys it);
    // a trip returns `rate_limited` and leaves the connection open. Binds on
    // every connection class — pre-identity, so (1d) admits an authenticated
    // caller, who would otherwise mint accounts without any bound at all.
    if kind == "fauna.account.register"
        && !crate::anonymous_rate_limit::check_conn(&state.register_rate_limit, &conn, &kind)
    {
        tracing::debug!(
            peer = ?conn.peer_addr,
            "registration rate limit exceeded"
        );
        crate::anonymous_rate_limit::report_shed(
            &state.register_rate_limit_shed,
            "register",
            crate::anonymous_rate_limit::ShedKey::of(&conn),
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited"),
        )
        .await;
        return;
    }

    // (1b⁗⁗) Invite-request submit throttle. `fauna.account.invite_request.submit`
    // is an anonymous, signature-bound *write* that inserts a `pending` row
    // (deduped only by `actor_id`, defeated by rotating keypairs); unthrottled it
    // floods SQLite with pending rows on a box already wedged once by disk
    // exhaustion (security review § D6). A per-source limit
    // (`invite_request_rate_limit`, separate budget) slows a single flooder; the
    // global `MAX_PENDING_INVITE_REQUESTS` row-count cap in
    // `invite_core::submit_invite_request_core` is the hard disk backstop a
    // distributed flood cannot evade. Keyed per class by `check_conn`; a trip
    // returns `rate_limited`, connection stays open. Binds on every connection
    // class — pre-identity, so (1d) admits an authenticated caller, and this
    // gate's own reason ("unthrottled it floods SQLite") is class-blind.
    if kind == "fauna.account.invite_request.submit"
        && !crate::anonymous_rate_limit::check_conn(&state.invite_request_rate_limit, &conn, &kind)
    {
        tracing::debug!(
            peer = ?conn.peer_addr,
            "invite-request submit rate limit exceeded"
        );
        crate::anonymous_rate_limit::report_shed(
            &state.invite_request_rate_limit_shed,
            "invite_request_submit",
            crate::anonymous_rate_limit::ShedKey::of(&conn),
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited"),
        )
        .await;
        return;
    }

    // (1c) Loopback gate. A loopback-only kind (bridge self-enrollment) is
    // trusted only from a same-host peer — a process that can reach nest's
    // loopback is already inside the deployment trust boundary. A remote source
    // IP is refused (cross-IP bridge auth is not yet designed). This is sound on
    // the SNI-fronted deploy because the in-container bridge dials nest's
    // loopback *directly* (no PROXY header → `peer_addr` is loopback → gate
    // passes), whereas every external client arrives via the SNI router, which
    // prepends a PROXY-v2 header carrying the real (non-loopback) client IP that
    // `serve_tls` resolves into `peer_addr` → gate refuses. `None` is treated as
    // non-loopback — i.e. this gate fails CLOSED, unlike the rate-limit gates
    // above, which is why the authenticated class (whose `peer_addr` is always
    // `None`) is refused here rather than admitted. This comment used to say
    // `None` happens "only in tests", the same false premise the throttle gates
    // carried; here the premise being wrong costs nothing, and the sentence is
    // corrected rather than the behaviour. Deliberately NOT "fixed" by capturing
    // `ConnectInfo` on the authenticated upgrade: that would populate
    // `peer_addr` for authenticated connections and turn this refusal into a
    // PASS for an authenticated loopback caller of `bridges.request_enrollment`.
    // Per `mail-bridge-lifecycle.md` § Cold boot.
    if crate::pre_identity_allowlist::requires_loopback_peer(&kind)
        && !conn
            .peer_addr
            .map(|a| a.ip().is_loopback())
            .unwrap_or(false)
    {
        tracing::warn!(
            kind = %kind,
            peer = ?conn.peer_addr,
            "refused loopback-only kind from non-loopback peer"
        );
        send_error_reply(
            &conn,
            correlation_id,
            RpcError::new(
                "fauna.bridges.remote_enrollment_unsupported",
                "error.bridges.remote_enrollment_unsupported",
            ),
        )
        .await;
        return;
    }

    // (1d) Central capability gate — the audited chokepoint of
    // `docs/goal/architecture/apps/bridges.md` § Why this shape: "every method
    // a bridge can call is on nest's per-role allowlist … it can't be reached by
    // a bridge until the role's allowlist contains the method." For an
    // authenticated connection, a *registered, non-pre-identity* kind dispatches
    // only when the caller's `CallerClass` is permitted by
    // `bridge_method_allowlist::is_permitted`. This makes the allowlist the
    // single enforcement point: a kind absent from it (the `_ => false` arm) is
    // bridge-unreachable *by default*, so a future handler that forgets its own
    // `require_class` can no longer be silently reached by a compromised bridge
    // actor. The per-handler `require_class` / `require_permission` calls stay as
    // defense in depth (they also carry the finer User-vs-Admin + caller-scope
    // logic this coarse gate does not). Scope:
    //   • anonymous connections were already confined to the pre-identity
    //     allowlist at (1b), so this runs only for authenticated ones;
    //   • pre-identity kinds (discovery / bootstrap / claim / enrollment) are
    //     exempt — an authed connection may still call them, and by design they
    //     are not in the class allowlist;
    //   • unregistered kinds are left to fall through to the `unknown_kind` reply
    //     in `spawn_dispatch` (we gate only kinds the router serves), so a typo
    //     still yields `unknown_kind`, not a misleading `permission_denied`.
    if !conn.anonymous
        && state.rpc_router.contains(&kind)
        && !crate::pre_identity_allowlist::is_pre_identity_kind(&kind)
    {
        match crate::bridge_method_allowlist::caller_class_for_actor(&state.db, &conn.actor_id)
            .await
        {
            Ok(Some(class)) => {
                if !crate::bridge_method_allowlist::is_permitted(class, &kind) {
                    tracing::warn!(
                        kind = %kind,
                        ?class,
                        "central capability gate: kind not permitted for caller class"
                    );
                    // Refusal-code contract (api-layers.md § Caller-class
                    // authorization): a LISTED kind refused on class answers
                    // with its own family's `fauna.<ns>.permission_denied` —
                    // the code apps and `tests/api/` branch on — while an
                    // UNLISTED kind keeps the central bridges code (full
                    // ruling on `class_refusal_namespace`).
                    let err = match crate::bridge_method_allowlist::class_refusal_namespace(&kind) {
                        Some(ns) => crate::rpc_errors::permission_denied_ns(
                            ns,
                            "caller class not permitted for this kind",
                        ),
                        None => crate::rpc_errors::central_permission_denied(),
                    };
                    send_error_reply(&conn, correlation_id, err).await;
                    return;
                }
            }
            // Unknown / revoked actor (incl. the zero-actor guard). An
            // authenticated connection's actor normally resolves; a
            // mid-connection revocation lands here — refuse rather than dispatch.
            Ok(None) => {
                tracing::warn!(
                    kind = %kind,
                    "central capability gate: unknown or revoked actor"
                );
                send_error_reply(
                    &conn,
                    correlation_id,
                    crate::rpc_errors::central_permission_denied(),
                )
                .await;
                return;
            }
            Err(e) => {
                tracing::error!(
                    kind = %kind,
                    error = %e,
                    "central capability gate: caller-class lookup failed"
                );
                send_error_reply(
                    &conn,
                    correlation_id,
                    RpcError::new("fauna.protocol.internal", "error.protocol.internal"),
                )
                .await;
                return;
            }
        }
    }

    // (2)–(7) Kind lookup → deadline → encode → spawn handler (AbortHandle for
    // Cancel) → cache + emit Reply. The mechanical core is shared with the
    // federation serving path (`crate::dispatch_core`); the per-actor path
    // supplies the `rpc_router` lookup + the authenticated actor as the subject.
    let actor = conn.actor_id;
    crate::dispatch_core::spawn_dispatch(state, conn, rpc_router_meta, actor, req).await;
}

/// Per-actor router lookup, passed to [`crate::dispatch_core::spawn_dispatch`].
/// (The federation path passes its own `federation_router` lookup; both yield the
/// same `&RpcKindMeta` since the handler shape is identical.)
fn rpc_router_meta<'a>(
    state: &'a AppState,
    kind: &str,
) -> Option<&'a crate::rpc_router::RpcKindMeta> {
    state.rpc_router.kind_meta(kind)
}

async fn send_error_reply(
    conn: &Arc<crate::ws::RpcConnection>,
    correlation_id: u64,
    err: fauna_protocol::RpcError,
) {
    use fauna_protocol::{Frame, Reply, Value, encode_canonical, encode_frame};
    let err_bytes = encode_canonical(&err).unwrap_or_default();
    let payload: Value = fauna_cbor::decode_strict(&err_bytes).unwrap_or(Value::Null);
    let frame = Frame::Reply(Reply {
        ty: Reply::TYPE,
        correlation_id,
        payload,
        ok: false,
    });
    if let Ok(bytes) = encode_frame(&frame) {
        let _ = conn.ws_tx.try_send(bytes);
    }
}

async fn dispatch_cancel(conn: &Arc<crate::ws::RpcConnection>, cancel: fauna_protocol::Cancel) {
    if let Some(handle) = conn
        .pending_handlers
        .lock()
        .await
        .remove(&cancel.correlation_id)
    {
        handle.abort();
        tracing::debug!(
            actor = %hex::encode(conn.actor_id),
            correlation_id = cancel.correlation_id,
            "cancel: aborted in-flight handler"
        );
    } else {
        tracing::trace!(
            actor = %hex::encode(conn.actor_id),
            correlation_id = cancel.correlation_id,
            "cancel: no in-flight handler (already completed or never seen)"
        );
    }
}

/// Failure modes of the shared post-ingest core (`ingest_post_core`). The
/// WS-RPC handler `posts_handlers::posts_create_handler` maps these onto
/// `RpcError` shapes; the ingest/classify pipeline lives in exactly one place.
/// (The HTTP twin `post_post` / `POST /api/v1/posts` was deleted in
/// `ws-rpc-everywhere-2` T4 — the per-variant HTTP status notes below are the
/// historical mapping the WS-RPC plane preserves.)
pub(crate) enum PostCreateError {
    /// `Storage::ingest_post` rejected — carries the pre-built `ApiError`
    /// (already status + message-shaped).
    Ingest(ApiError),
    /// payload-store / `put_post` failure (the HTTP twin's `500`).
    Internal(String),
}

/// Shared post-ingest core — the full pipeline behind `fauna.posts.create`
/// (the only caller since the `POST /api/v1/posts` HTTP twin was removed in
/// `ws-rpc-everywhere-2` T4): `ingest_post` seal-shape verify → `store_post`
/// (segment body + floor projection) → engagement counters →
/// `spawn_replicate_post` + bluesky write-through. Returns the
/// content-addressed `post_id` (`blake3(body)`) on success. See
/// `posts_handlers::posts_create_handler` for the `RpcError` mapping.
///
/// There is no pre-mode gate: a nest is content-ready from first boot
/// (`nest/storage-modes.md` § Boot story).
pub(crate) async fn ingest_post_core(
    state: &Arc<AppState>,
    author: [u8; 32],
    body: &Bytes,
) -> Result<[u8; 32], PostCreateError> {
    let hash = blake3::hash(body);
    let post_id: [u8; 32] = *hash.as_bytes();

    // Post bodies now rest in the `__post/<author_hex>` segment store, not
    // `content.payload` / the blob store — `store_post` (below) appends the
    // body and writes the `content` row + projection with an empty payload.
    // The legacy `payload_store` inline/blob path is retired for posts (media
    // *attachments* referenced inside the body are separate blobs, unaffected).

    // --- Pre-store envelope verification (Storage::ingest_post) ---
    //
    // BARE-decode + signature-verify + (if gated) key-access consistency.
    // Rejection → Err(StorageUnavailable::ingest_rejected(reason)) maps via
    // into_api_error() to 400 with `{"error":"ingest rejected: <reason>"}`.
    // Verdict drives nest_post_ingest_total{verdict,reason}; the `reason` label
    // is populated only on verdict=rejected.
    let storage = state.storage();
    let post_outcome = match storage
        .ingest_post(&crate::storage::PostIngestItem {
            uploader: author,
            body: body.as_ref(),
        })
        .await
    {
        Ok(o) => o,
        Err(e) => {
            let reason_label = match e.kind {
                crate::storage::StorageUnavailableKind::IngestRejected(r) => r.as_snake_case(),
                _ => "error",
            };
            tracing::warn!(
                target: "nest_metrics",
                metric = "nest_post_ingest_total",
                verdict = "rejected",
                reason = reason_label,
                "post ingest rejected: {e}"
            );
            return Err(PostCreateError::Ingest(e.into_api_error()));
        }
    };
    match post_outcome.verdict {
        crate::storage::PostIngestVerdict::Accepted => {
            tracing::debug!(
                target: "nest_metrics",
                metric = "nest_post_ingest_total",
                verdict = "accepted",
                "post ingest"
            );
        }
    }
    drop(storage);

    // No nest-side classification runs here: a content scorer runs only at a
    // capability position — the perimeter bridge pre-seal, the user's client
    // post-decrypt, or a holder of a user-minted grant (the re-score drain).
    // `architecture/content-scoring.md` § The placement matrix.

    // Store the post: body → `__post` segment store, `content` row + projection
    // with an empty payload (the post-cutover authoritative body store).
    match crate::segments::post::store_post(&state.post_segments, &state.db, &post_id, body, None)
        .await
    {
        Ok(()) => {}
        Err(e) => {
            tracing::error!("store_post error: {e}");
            return Err(PostCreateError::Internal("storage error".into()));
        }
    }

    // Interaction-bar counters: a reply / repost / quote post bumps its target's
    // counter when the referencing post lands (`feed.md` § Interaction bar; the
    // `like` half rides `fauna.posts.interact`). Idempotent (deduped by this
    // content-addressed post's id) + non-fatal — a counter hiccup must not fail
    // the create, so log and proceed.
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    if let Err(e) = state
        .db
        .record_reference_engagements(&post_id, &author, body, now_us)
        .await
    {
        tracing::warn!("record_reference_engagements: {e}");
    }

    // The post's floor-derived FTS row is written by `store_post`'s projection
    // (`put_post_index_only` → `insert_and_index`, body = `Post::body_text()`),
    // which is what `Storage::search` serves. Nothing to do here.

    // The room arm of the reception pass: when this is a room-restricted post
    // addressed to a community room this nest homes and holds the tip's wrap
    // for, open it in this same act and index it into the room's view
    // (`conversation-rooms.md` § The three classes → *What the home nest does
    // with its read*, purpose 3). Every other post — and every failure — is a
    // no-op; the create never depends on it.
    crate::room_post_view::index_room_post(state, &post_id, body).await;

    spawn_replicate_post(state.clone(), post_id, body.to_vec());
    spawn_post_bridge_fanout(state, author, post_id, body);
    // Post forwarding (private → paired public nest). Awaited rather than
    // spawned: it is one INSERT, and a dropped task would silently lose the
    // relay. Sits with the other outbound paths above and, like them, is not
    // gated on the quarantine/suppress decision.
    maybe_enqueue_outbox(state, &author, body).await;

    // ATProto PDS projection nudge (S3, `atproto-pds-bridge.md` § Where logic
    // lives): a freshly-landed post may need projecting to the author's
    // ATProto repo. Fired for every create — over-nudging is fine (the bridge
    // pulls the cursor-paged public stream and filters); best-effort (a
    // disconnected bridge catches up on its poll).
    crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(state, Some(author))
        .await;

    Ok(post_id)
}

/// Outcome of the shared post-read core (`get_post_core`). Mirrors the HTTP
/// twin `get_post`'s three terminal shapes (resolved bytes / not-found /
/// storage error), plumbed plain so the WS-RPC handler can map them too.
pub(crate) enum GetPostOutcome {
    /// Resolved post bytes (the HTTP twin's `application/octet-stream` body).
    Found(Vec<u8>),
    /// Taken down under a legal obligation (`moderation.md` § Categories &
    /// enforcement item 1). The body is **withheld from every viewer** — never
    /// read/resolved — and the client renders a visible tombstone ("removed
    /// under legal obligation [reference]") in its place. `reference` is the
    /// legal-obligation reference the tombstone cites.
    LegalTakedown { reference: String },
    /// Quarantine-gated-or-absent (the HTTP twin's `404`).
    NotFound,
    /// Storage error (the HTTP twin's `500`).
    Error,
}

/// Shared post-read core — the body behind `GET /api/v1/posts/{id}` and
/// `fauna.posts.get`: DB read → quarantine visibility gate (quarantined ⇒
/// author/admin only) → payload-store resolve → worker read-fallback.
/// `caller` is the authenticated actor (the HTTP twin reads it from an
/// optional bearer; the WS-RPC plane always carries the connection actor) —
/// it keys the quarantine gate. Behavior-preserving.
pub(crate) async fn get_post_core(
    state: &Arc<AppState>,
    caller: Option<[u8; 32]>,
    post_id: [u8; 32],
) -> GetPostOutcome {
    match state.db.get_post(&post_id).await {
        Ok(Some((data, _blob_hash))) => {
            // Legal-obligation takedown gate — checked FIRST, before quarantine
            // and before the body is ever read/resolved (moderation.md
            // § Categories & enforcement item 1). A taken-down post's body is
            // withheld from *every* viewer, including its author and admins
            // (unlike quarantine, which is author/admin-visible) — genuinely
            // illegal content must not be re-served through the read path. The
            // author still learns of it via `fauna.moderation.actions` and can
            // `fauna.moderation.appeal`; the caller here gets the tombstone.
            if let Ok(Some(reference)) = state.db.get_post_legal_takedown(&post_id).await {
                return GetPostOutcome::LegalTakedown { reference };
            }
            // Quarantined posts: visible only to author and admins (spec §4.5).
            // The single read-authz gate, shared with `fauna.moderation.train`
            // (`CacheDb::caller_may_read_content` — same quarantine-visibility
            // policy and error handling).
            if !state
                .db
                .caller_may_read_content(caller.as_ref(), &post_id)
                .await
            {
                return GetPostOutcome::NotFound;
            }
            // Segment-first (post-cutover authoritative body store); fall back
            // to the inline `content.payload` (`data`, the row just read) —
            // the shapes `segments::post::load_post_body` documents.
            let resolved = match crate::segments::post::read_body_by_post_id(
                &state.post_segments,
                &state.db,
                &post_id,
            )
            .await
            {
                Ok(Some(seg_body)) => seg_body,
                _ => data,
            };
            GetPostOutcome::Found(resolved)
        }
        Ok(None) => {
            // Read-fallback: try fetching from worker
            if let Some(handle) = state.bridge.worker.get_handle().await {
                let post_id_hex = hex::encode(post_id);
                match handle
                    .fetch(crate::nest_link::protocol::PayloadKind::Post, &post_id_hex)
                    .await
                {
                    Ok(Some(data)) => {
                        // Cache locally — body → segment, projection with empty
                        // payload (the post-cutover authoritative body store).
                        if let Err(e) = crate::segments::post::store_post(
                            &state.post_segments,
                            &state.db,
                            &post_id,
                            &data,
                            None,
                        )
                        .await
                        {
                            tracing::warn!("failed to cache worker-fetched post: {e}");
                        }
                        return GetPostOutcome::Found(data);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("worker fetch failed: {e}");
                    }
                }
            }
            GetPostOutcome::NotFound
        }
        Err(e) => {
            tracing::error!("get_post error: {e}");
            GetPostOutcome::Error
        }
    }
}

/// Outcome of the shared post-delete core (`delete_post_core`).
pub(crate) enum PostDeleteOutcome {
    /// This call newly removed the post.
    Deleted,
    /// The post was already gone — the idempotent success, never an error.
    AlreadyGone,
}

/// Whether [`delete_post_core`] should re-render the author's web site inline
/// when the deleted post was still web-published, or leave that entirely to
/// the caller.
///
/// The five single-post doors (`fauna.posts.delete`, `unrepost`, the two
/// ATProto bridge deletes, the federation delete) always pass `Now` — a
/// delete is a rare, individually-bounded event, so an inline render is
/// cheap. `retract_actor_posts` — the account-deletion/eviction purge's
/// per-post retraction loop — passes `Skip` and renders **once for the whole
/// pass** instead: rendering per post there would cost up to
/// `MAX_TEMPLATES` × `MAX_RENDERED_POSTS` `render_bounded` calls per
/// retracted post (`web_content/service.rs`), every one of them thrown away
/// the moment the purge sweeps `web_rendered` right after
/// (`account-data-plane.md` § Nest-side requirements item 1).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderSite {
    /// Render inline whatever the author's site is owed.
    Now,
    /// Never render inline. The delete's transaction still records the owed
    /// render, so the batching caller renders what its whole pass left owed
    /// (`WebContentService::render_owed`) — and a pass that dies first leaves
    /// the marker for the boot drain.
    Skip,
}

/// Failure modes of the shared post-delete core.
pub(crate) enum PostDeleteError {
    /// The caller (or the tombstone) is not the stored post's author.
    NotAuthor,
    /// Storage failure.
    Internal(String),
}

/// The read-only half of [`delete_post_core`]: may this actor delete this post?
///
/// `Ok(None)` means proceed; `Ok(Some(outcome))` means the delete is already
/// settled (the post is gone) and `Err(NotAuthor)` refuses it. Nothing here
/// mutates, which is what lets a caller ask the question *before* committing to
/// anything.
///
/// Extracted so the answer has ONE owner and two callers. The external-write
/// batch path pre-flights it (`bridge_atproto_handlers.rs`), because a batch is
/// all-or-nothing and a refusal discovered inside an apply arm would leave the
/// rows before it already applied; `delete_post_core` still runs it as the
/// guarantee, so no caller can skip it by forgetting to ask. Re-deriving the
/// rule at the pre-flight site instead would give a security-relevant check two
/// owners, free to drift apart.
pub(crate) async fn check_post_delete_authorization(
    state: &Arc<AppState>,
    actor: [u8; 32],
    tombstone: &fauna_core::data::Tombstone,
    digest: [u8; 32],
) -> Result<Option<PostDeleteOutcome>, PostDeleteError> {
    let internal = |e: anyhow::Error| PostDeleteError::Internal(format!("{e:#}"));

    // Author check 1: the connection actor IS the tombstone author (the
    // envelope signature already binds `tombstone.author`; this binds the
    // transport identity, so a captured author-signed tombstone cannot be
    // replayed from someone else's session).
    if tombstone.author.0 != actor {
        return Err(PostDeleteError::NotAuthor);
    }

    // Author check 2: the STORED post belongs to that author — the
    // segment-record scope (authoritative post-cutover), falling back to the
    // `content` row's author column for an inline row (a replicated post or
    // one with an undecodable author).
    let seg = crate::segments::post::lookup_scope_by_post_id(&state.db, &digest)
        .await
        .map_err(internal)?;
    let row_author = state.db.get_post_author(&digest).await.map_err(internal)?;
    if seg.is_none() && row_author.is_none() {
        return Ok(Some(PostDeleteOutcome::AlreadyGone));
    }
    if let Some((scope, _)) = seg
        && scope != actor
    {
        return Err(PostDeleteError::NotAuthor);
    }
    if let Some(author) = row_author
        && author != actor
    {
        return Err(PostDeleteError::NotAuthor);
    }
    Ok(None)
}

/// Resolve who authored a post — the segment-record scope when the post has
/// cut over to sealed-per-message storage, else the inline `content.author`
/// column (the same fallback order [`check_post_delete_authorization`] uses).
/// `None` when the post is unknown to both. Any handler that must refuse
/// acting on a post it does not own can compare its actor against this.
pub(crate) async fn resolve_post_author(
    state: &AppState,
    post_id: &[u8; 32],
) -> anyhow::Result<Option<[u8; 32]>> {
    if let Some((scope, _)) =
        crate::segments::post::lookup_scope_by_post_id(&state.db, post_id).await?
    {
        return Ok(Some(scope));
    }
    state.db.get_post_author(post_id).await
}

/// [`delete_post_core`]'s step 1c: under [`RenderSite::Now`], render the
/// author's site if a render is owed to it, failing closed. Logged, never
/// surfaced — the delete has landed either way.
async fn render_site_owed_after_post_delete(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    policy: RenderSite,
) {
    if policy == RenderSite::Now
        && let Some(wcs) = &state.web_content_service
        && let Err(e) = wcs.render_owed(actor, "a post delete").await
    {
        tracing::warn!("web render after post delete: {e:#}");
    }
}

/// [`delete_post_core`]'s steps 3b–7: every leg that carries the delete off
/// this box's projection — the ATProto bridge nudge, Nostr kind-5, the Bluesky
/// write-through delete, the paired-replica twin and the ActivityPub `Delete`.
/// Each leg is idempotent and non-fatal (the local delete has already landed),
/// which is what lets it run twice: once after a delete's own steps 1–3, and
/// again on a retry that finds the post already gone (`delete_post_core`'s
/// settled path, gated on the post-delete witness naming the same actor).
async fn spawn_post_delete_outward_legs(state: &Arc<AppState>, actor: [u8; 32], digest: [u8; 32]) {
    // 3b. ATProto projection nudge (S3, `atproto-pds-bridge.md` § Where logic
    // lives). The delete *witness* the bridge reads is written atomically with
    // step 1 — the projection rows are physically GONE and the segment
    // tombstone carries no delete timestamp, so that `tombstone/post` row
    // (payload = bare canonical `Tombstone`, at the DELETE instant) is the one
    // durable, timestamped signal the bridge's cursor-paged
    // `fetch_public_posts` stream can interleave, letting a cursor that long
    // passed the post still see its delete. Deterministic id ⇒ a retry
    // converges on the same row. The nudge itself stays best-effort like the
    // legs below: the poll backstop is what guarantees the bridge notices.
    crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(state, Some(actor))
        .await;

    // 4. Nostr propagation — a derived relay event must not outlive the post
    // (kind-5 leg, `nostr.md` § The relay event store NIP-09). Idempotent, so
    // a retry's second run chases derived rows a first attempt left behind.
    // Non-fatal — the local delete stands.
    #[cfg(feature = "nostr")]
    if let Err(e) = crate::nostr::propagate_post_delete(state, &actor, &digest).await {
        tracing::warn!("nostr kind-5 post-delete propagation: {e}");
    }

    // 5. Bluesky write-through delete — a deleted post's cross-posted Bluesky
    // record must not outlive it (`feed.md` § Post deletion → Propagation).
    // Fire-and-forget + non-fatal, mirroring the create-side write-through
    // (`spawn_write_through`); a clean no-op for the vast majority of posts
    // (never cross-posted). The mapping is retained until the remote delete
    // succeeds, so a retry — or the `post_delete_redrive` sweep — still
    // chases a record a first attempt failed to remove.
    #[cfg(feature = "bluesky")]
    crate::bluesky::spawn_write_through_delete(state.clone(), actor, digest);

    // 6. Paired-replica tombstone twin — a post replicated to the paired public
    // nest via the `nest_link` worker (`spawn_replicate_post`) must not outlive
    // its original (`feed.md` § Post deletion → Propagation). Fire-and-forget +
    // non-fatal, mirroring the create-side replicate; a no-op when no worker is
    // connected. The `worker_replication` marker stays until the worker acks,
    // so a retry, the `post_delete_redrive` sweep, or the worker's next
    // connect still chases a replica a first attempt left behind.
    spawn_replicate_delete(state.clone(), digest);

    // 7. ActivityPub Delete push — a post whose Create was pushed to
    // fediverse followers must not outlive it (`feed.md` § Post deletion →
    // Propagation; mechanics `activitypub.md` § Post deletion). Pushes iff a
    // Create was pushed (the ap_post_map row is the witness). Fire-and-forget
    // + non-fatal; the tombstoned map row still resolves, so a retry chases a
    // Delete a first attempt failed to enqueue.
    #[cfg(feature = "activitypub")]
    crate::activitypub::push::spawn_delete_push(state.clone(), actor, digest);
}

/// Shared post-delete core — the pipeline behind `fauna.posts.delete`
/// (`feed.md` § State & data shape → *Post deletion*, ratified 2026-07-15).
/// The caller (`posts_handlers::posts_delete_handler`) has already verified
/// the tombstone's envelope signature against `tombstone.author`
/// (`fauna_core::encoding::decode_tombstone`); this core enforces the two
/// remaining author checks, then removes in the crash-safe order:
///
/// 1. **Projection rows first** (`CacheDb::delete_post_projection`) — the
///    serving gate: after this, `fauna.posts.get` and every feed query are
///    correctly dark. A crash after this step leaves only an
///    unreachable-but-live segment record (reclaimed on a delete retry),
///    never a servable half-state — the reverse order would serve an
///    empty-bodied ghost.
/// 2. **Segment-record tombstone** (`segments::post::tombstone_by_cid`,
///    idempotent; the body bytes are reclaimed by compaction).
/// 3. **Reference-counter reversal** (`CacheDb::reverse_reference_engagements`,
///    best-effort like its create-side mirror — a counter hiccup must not
///    fail the delete).
///
/// Trending withdraws at the trend sweep's next `content_meta` re-probe
/// (≤15 min); an immediate-withdraw hook is a named follow-on. Propagation
/// legs (paired-replica / outbox / Nostr kind-5 / Bluesky / ActivityPub) hook
/// in behind this core as they land — per-leg status in `feed.md`
/// § Implementation status today.
///
/// A still-web-published post also gets its author's site re-rendered right
/// after step 1 — iff `policy` is [`RenderSite::Now`] — so the stale
/// `post/{slug}.html` page stops serving instead of surviving until an
/// unrelated publish happens to trigger the next render
/// (`web-content-hosting.md` § Routing, render, serving). Under
/// [`RenderSite::Skip`] the render never runs here; the owed-render marker
/// step 1's transaction wrote carries it to the caller's own end-of-pass
/// render.
pub(crate) async fn delete_post_core(
    state: &Arc<AppState>,
    actor: [u8; 32],
    tombstone: &fauna_core::data::Tombstone,
    digest: [u8; 32],
    policy: RenderSite,
) -> Result<PostDeleteOutcome, PostDeleteError> {
    let internal = |e: anyhow::Error| PostDeleteError::Internal(format!("{e:#}"));

    if let Some(settled) = check_post_delete_authorization(state, actor, tombstone, digest).await? {
        // A retry of a delete that already ran to its end still owes the site
        // whatever render the first attempt left undone (§ 1c below).
        render_site_owed_after_post_delete(state, &actor, policy).await;
        // ...and still owes every outward leg a first attempt may have failed
        // (steps 3b–7): a Bluesky outage, a disconnected worker, a crash
        // before the ActivityPub enqueue. Gated on the step-1 witness naming
        // THIS actor, so the chase runs only for the author the delete ran
        // for — a stranger's tombstone over someone else's gone digest passes
        // the author check above (it names the stranger) and must run nothing.
        // A retry is not the only chase: `post_delete_redrive` sweeps the
        // retained Bluesky mappings and replica markers on its own, since an
        // author whose first delete answered `Deleted` has no reason to retry.
        match state.db.post_delete_witness_author(&digest).await {
            Ok(Some(author)) if author == actor => {
                spawn_post_delete_outward_legs(state, actor, digest).await;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("post-delete witness read on a settled delete: {e:#}"),
        }
        return Ok(settled);
    }

    // The scope the tombstone step below writes into. Re-read rather than
    // returned from the check above: the check owns the *rule*, not this row,
    // and threading it out would tie a read-only authorization question to one
    // caller's later needs.
    let seg = crate::segments::post::lookup_scope_by_post_id(&state.db, &digest)
        .await
        .map_err(internal)?;

    // Read the body BEFORE removal — the counter reversal needs the
    // references the deleted post carried. Segment-first, then the inline `content.payload` body.
    let body_bytes =
        match crate::segments::post::read_body_by_post_id(&state.post_segments, &state.db, &digest)
            .await
        {
            Ok(Some(bytes)) => Some(bytes),
            _ => match state.db.get_post(&digest).await {
                Ok(Some((data, _))) if !data.is_empty() => Some(data),
                _ => None,
            },
        };

    // 0. The compelled fact, captured before the row that carries it goes.
    //
    // Every post-side takedown withhold keys on
    // `content_meta.legal_takedown_ref` — the export's segment-pair set, the
    // blob door's rebuilt set — and step 1 below deletes exactly that row,
    // while step 2 only tombstones the mirror and leaves the bytes for
    // compaction. So without this the author's own delete put the compelled
    // body back in the next `include_blobs` export and re-served its
    // attachments until the GC sweep (`moderation.md` § Legal takedown →
    // *Posts*).
    //
    // It has to happen HERE and nowhere later: the obligation ledger cannot
    // answer the question afterwards, because an overturn writes no
    // counter-row into it, so *taken down → deleted* and *taken down →
    // restored → deleted* are indistinguishable there — and the blob digests
    // are unreadable once step 2 lands, `load_post_body` answering `None` for
    // a tombstoned record. The flag is authoritative right up to step 1, and
    // `body_bytes` above is the last readable copy of the record.
    //
    // Best-effort by the same rule as 1a/1b: a delete must not fail on this.
    // The failure direction is the safe one only because the row is what the
    // withhold READS — a missing row over-serves, so it is logged loudly
    // rather than swallowed.
    if let Ok(Some(reference)) = state.db.get_post_legal_takedown(&digest).await {
        let digests: Vec<[u8; 32]> = body_bytes
            .as_deref()
            .and_then(crate::db::posts::decode_stored_post)
            .map(|post| post.blob_refs().into_iter().map(|h| h.digest()).collect())
            .unwrap_or_default();
        if let Err(e) = state
            .db
            .record_taken_down_post_deleted(
                &digest,
                &tombstone.author.0,
                &reference,
                &digests,
                tombstone.created_at.0 as i64,
            )
            .await
        {
            tracing::error!(
                post = hex::encode(digest),
                error = %e,
                "FAILED to record a taken-down post's deletion — its compelled body may ride \
                 the author's next export until compaction and its attachments serve until the \
                 GC sweep (moderation.md § Legal takedown)"
            );
        }
    }

    // 1. Projection rows — the serving gate. The ATProto delete witness rides
    // in the SAME transaction (see 3b below for what the witness is for):
    // "removed ∧ witnessed" is atomic, so a lost witness can never leave a
    // retracted post live on Bluesky forever. So does the web site's revoke:
    // the transaction decides off the post's own `web_published` link — the
    // one its cascade is about to remove — whether the author's rendered site
    // now owes a render, and records that beside the delete (§ 1c below).
    let witness_bytes = fauna_core::encoding::canonical_encode(tombstone)
        .map_err(|e| {
            tracing::warn!("atproto projection tombstone encode: {e:#}");
            e
        })
        .ok();
    let row_removed = state
        .db
        .delete_post_projection_with_witness(
            &digest,
            witness_bytes
                .as_ref()
                .map(|b| (&actor, b.as_slice(), tombstone.created_at.0 as i64)),
        )
        .await
        .map_err(internal)?;

    // 1a. Immediate trending-withdraw: `content_meta` is gone now, so
    // `recompute_trend_score` sees no row (`is_public = None`), scores 0, and
    // DELETEs any stale `content_scores` trending row for this post right
    // away — instead of leaving it live until the sweep's next ≤15-min
    // re-probe. Runs on every outcome (idempotent, matching the nostr
    // propagation call below) and is best-effort like it — a scoring hiccup
    // must not fail the delete.
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    if let Err(e) = state.db.recompute_trend_score(&digest, now_us).await {
        tracing::warn!("trend withdraw on post delete: {e}");
    }

    // 1b. The room view of a room-restricted post (`crate::room_post_view`): a
    // search hit naming a deleted post is a derivation outliving it. Runs on
    // every outcome, idempotent, best-effort like 1a — a no-op for every post
    // no room indexed.
    if let Err(e) = state.db.purge_room_post_views_for_post(&digest).await {
        tracing::warn!("room post view withdraw on post delete: {e}");
    }

    // 1c. Re-render the author's web site so a still-published post's stale
    // rendered page (`post/{slug}.html`, the index, `feed.xml`) stops serving
    // at once instead of surviving until some unrelated publish/unpublish
    // happens to trigger the next render (`web-content-hosting.md` §
    // Routing, render, serving → *A revoke is durable*; `feed.md` § Post
    // deletion → Propagation). Keyed on the owed-render marker step 1's
    // transaction wrote, so an unpublished post's delete — the common case —
    // renders nothing, and so a RETRY of a delete torn after step 1 still
    // renders: it removes nothing, and finds the render owed all the same.
    // Gated on `policy` too: `retract_actor_posts` passes `Skip` and renders
    // once for its whole retraction pass instead (see [`RenderSite`]). Fails
    // closed — a render that errors clears the site rather than keep serving
    // the post its author just deleted — and never fails the delete.
    render_site_owed_after_post_delete(state, &actor, policy).await;

    // 2. Segment-record tombstone (idempotent).
    let mut seg_removed = 0;
    if let Some((scope, seg_id)) = seg {
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(digest);
        seg_removed = crate::segments::post::tombstone_by_cid(&state.db, &scope, seg_id, &cid)
            .await
            .map_err(internal)?;
    }

    // 3. Reverse the reference counters this post bumped when it landed.
    if let Some(bytes) = body_bytes
        && let Err(e) = state
            .db
            .reverse_reference_engagements(&digest, &bytes)
            .await
    {
        tracing::warn!("reverse_reference_engagements: {e}");
    }

    // 3b–7. The outward legs — every copy of the post off this box's
    // projection (see [`spawn_post_delete_outward_legs`]).
    spawn_post_delete_outward_legs(state, actor, digest).await;

    Ok(if row_removed || seg_removed > 0 {
        PostDeleteOutcome::Deleted
    } else {
        PostDeleteOutcome::AlreadyGone
    })
}

/// GET /api/v1/posts/:post_id — Return raw dag-cbor-encoded post.
///
/// **Permanent public-byte residue, not a deprecated twin** (`api-layers.md`
/// § Remaining HTTP). Local clients fetch their own posts via the WS-RPC kind
/// `fauna.posts.get` (`posts_handlers::posts_get_handler`); this optional-auth
/// HTTP route is the cross-nest post-fetch endpoint — the `fetch_url`
/// `peer_query.rs` builds for discovery-feed contributors points here, the
/// discovery poller stores it as the post-index `source`, and a client reads it
/// from the author's nest. Not a nest↔nest call, so the federation channel does
/// not carry it. The read logic lives in the shared `get_post_core` (also used
/// by the WS-RPC handler).
pub async fn get_post(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(post_id_hex): Path<String>,
) -> impl IntoResponse {
    let post_id = match parse_32_bytes(&post_id_hex) {
        Some(id) => id,
        None => return ApiError::bad_request("invalid post_id hex").into_response(),
    };

    let caller = optional_bearer_auth(&headers, &state).await.map(|id| id.0);
    match get_post_core(&state, caller, post_id).await {
        GetPostOutcome::Found(bytes) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            bytes,
        )
            .into_response(),
        // Legally taken down: withhold the body from the cross-nest federation
        // fetch too (a taken-down post must not propagate to peer nests), but
        // DISCLOSE the withholding — 451 Unavailable For Legal Reasons, the
        // transparent, non-silent posture (ratified 2026-07-06; the WS channel
        // twin discloses via `FedPostGetReply.legal_takedown`). No body: the
        // tombstone reference render is an origin-nest client concern.
        GetPostOutcome::LegalTakedown { .. } => {
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response()
        }
        GetPostOutcome::NotFound => StatusCode::NOT_FOUND.into_response(),
        GetPostOutcome::Error => ApiError::internal("storage error").into_response(),
    }
}

/// GET /api/v1/health
///
/// The `commit` field is served **unauthenticated by deliberate design**. The review flagged it as a commit→CVE
/// hint, but the production redeploy-verify contract reads it over a plain,
/// credential-less `curl https://<host>/api/v1/health` to confirm a Watchtower
/// auto-update flipped the running image (memory
/// `redeploy-faunafan-watchtower-autoupdate`) — gating it behind auth would
/// break that workflow on a fresh, pre-claim box that has no admin credential
/// to present. The residual leak is marginal: `version`
/// (`CARGO_PKG_VERSION`) already approximates the build, and the deployed SHA
/// reveals nothing an attacker couldn't read from the public source tree. So
/// the operational need outweighs the hint, and the field stays open. If a
/// future deployment makes the leak unacceptable, replace the verify signal
/// (e.g. an authed `/health/detail`) *before* gating `commit` here, not after.
///
/// `build_id` joins it for the same operational reason and on the same terms:
/// the release pipeline's promotion gate must distinguish two *builds* of one
/// commit, which `commit` alone cannot do (`crate::build_identity` owns the
/// pair and the rationale).
pub async fn health() -> Json<serde_json::Value> {
    Json(crate::build_identity::identity_json("ok"))
}

// `GET /api/v1/setup-status` removed in S4c2: setup
// progress is served by the `fauna.setup.status` WS-RPC kind
// (`discovery_handlers::setup_status_handler`), reading the shared
// `discovery_core::setup_status_core`. The typed WS reply flattens the legacy
// nested `dns`/`tls`/`email`/`admin` JSON objects to top-level booleans.

// `POST /api/v1/auth/token` (`post_auth_token` + `AuthTokenRequest` +
// `auth_error_to_http_token`) was the LAST control-plane HTTP twin; it was
// deleted in the WS-RPC-everywhere endgame once every app minted the
// bearer over the pre-identity kind `fauna.auth.handshake` (`auth_handlers`,
// shared `auth_core::direct_auth_core` + its `AuthError`→reply mapping). The
// FFI `mint_bearer` / `WsChallengeBearer` are the uniform replacement.

/// Verify that an inbox payload is a valid signed (EmbedAsBytes-cr,
/// EmbedAsBytes-post) tuple: both envelopes verify, the contact request's
/// sender is the post's author, and the request names this post's id. Both ride
/// the embed-as-bytes wire shape under sign-over-CID per
/// `docs/goal/architecture/serialization.md` (Tasks 2.5–2.6).
fn verify_inbox_payload(payload: &[u8]) -> std::result::Result<(), String> {
    use fauna_core::encoding::{
        EmbedAsBytes, compute_post_id, decode_signed_bytes, verify_envelope,
    };
    let (cr_wire, post_wire): (EmbedAsBytes, EmbedAsBytes) =
        canonical_decode(payload).map_err(|e| format!("invalid payload: {e}"))?;
    let (post_bytes, post_env) = post_wire
        .into_signed()
        .map_err(|e| format!("split post envelope: {e}"))?;
    let post: Post = decode_signed_bytes(&post_bytes).map_err(|e| format!("decode post: {e}"))?;
    let (cr_bytes, cr_env) = cr_wire
        .into_signed()
        .map_err(|e| format!("split CR envelope: {e}"))?;
    let cr: ContactRequest =
        decode_signed_bytes(&cr_bytes).map_err(|e| format!("decode CR: {e}"))?;

    if verify_envelope(&post, &post_bytes, &post_env).is_err() {
        return Err("invalid post signature".into());
    }

    if verify_envelope(&cr, &cr_bytes, &cr_env).is_err() {
        return Err("invalid contact request signature".into());
    }

    if cr.sender != post.author {
        return Err("sender mismatch: CR.sender != post.author".into());
    }

    let expected_id = compute_post_id(&post).map_err(|e| format!("compute post_id: {e}"))?;
    if cr.post_id != expected_id {
        return Err("post_id mismatch".into());
    }

    Ok(())
}

/// Extract the **sender** actor id from a signed `(ContactRequest, Post)` inbox
/// payload, without verifying signatures (full verification is
/// [`verify_inbox_payload`], run inside [`deliver_inbox_payload_core`]). Used by
/// the authed `fauna.inbox.send` handler to bind `cr.sender == caller` before
/// delivery — a property the unauthenticated HTTP twin and the nest↔nest
/// federation leg structurally cannot enforce (neither carries an authenticated
/// caller identity). Returns the 32-byte sender id, or a decode-error string.
pub(crate) fn inbox_payload_sender(payload: &[u8]) -> std::result::Result<[u8; 32], String> {
    use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes};
    let (cr_wire, _post_wire): (EmbedAsBytes, EmbedAsBytes) =
        canonical_decode(payload).map_err(|e| format!("invalid payload: {e}"))?;
    let cr: ContactRequest =
        decode_signed_bytes(&cr_wire.bytes).map_err(|e| format!("decode CR: {e}"))?;
    Ok(cr.sender.0)
}

pub fn parse_actor_id(hex_str: &str) -> Option<[u8; 32]> {
    parse_32_bytes(hex_str)
}

/// Decode a hex string into a 32-byte id (actor/channel/nest), or `None` on
/// malformed hex / wrong length. Shared across the nest handlers (it outlived
/// `channel_routes`, which Spec Y2 slice 5 deleted).
pub fn parse_32_bytes(hex_str: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(hex_str).ok()
}

/// Parse a wire `name_hash` into a fixed 32-byte array (S5b, `path-sealing.md`).
/// `None` passes through unchanged; a present-but-malformed length is refused
/// via the caller-supplied `invalid_request` builder, never silently
/// downgraded to the plaintext-name arm (the `resolve_folder_by_name` admin
/// precedent, `admin_ws_handlers.rs`, which now also routes through this
/// function). `folder_handlers`, `filesync_handlers` and `sync_handlers` each
/// hand-copied this exact body — differing only in how each names its own RPC
/// namespace in the error — so the shared core takes the error constructor as
/// a closure (round 188 of the shared-Rust harvest sweep). The length check itself delegates to `rpc_errors::require_bytes32`
/// — the one general 32-byte wire-field primitive — while keeping this
/// field's richer message.
pub(crate) fn parse_name_hash(
    name_hash: &Option<fauna_protocol::ByteBuf>,
    invalid_request: impl FnOnce(&str) -> fauna_protocol::RpcError,
) -> Result<Option<[u8; 32]>, fauna_protocol::RpcError> {
    name_hash
        .as_ref()
        .map(|h| {
            crate::rpc_errors::require_bytes32("name_hash", h.as_ref()).map_err(|_| {
                invalid_request("name_hash must be exactly 32 bytes (a BLAKE3 set-name digest)")
            })
        })
        .transpose()
}

#[cfg(test)]
mod parse_name_hash_tests {
    use super::parse_name_hash;

    fn err(msg: &str) -> fauna_protocol::RpcError {
        crate::rpc_errors::invalid_request_ns("test", msg)
    }

    #[test]
    fn absent_name_hash_passes_through_as_none() {
        assert_eq!(parse_name_hash(&None, err).unwrap(), None);
    }

    #[test]
    fn a_32_byte_name_hash_round_trips_exactly() {
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
        let wire = Some(fauna_protocol::ByteBuf::from(bytes.to_vec()));
        assert_eq!(parse_name_hash(&wire, err).unwrap(), Some(bytes));
    }

    #[test]
    fn a_wrong_length_name_hash_is_refused_not_silently_downgraded() {
        let wire = Some(fauna_protocol::ByteBuf::from(vec![1, 2, 3]));
        let result = parse_name_hash(&wire, err);
        assert!(
            result.is_err(),
            "a malformed-length name_hash must error, never fall through as if absent"
        );
    }

    #[test]
    fn a_longer_than_32_byte_name_hash_is_refused_not_truncated() {
        let wire = Some(fauna_protocol::ByteBuf::from(vec![7u8; 33]));
        let result = parse_name_hash(&wire, err);
        assert!(
            result.is_err(),
            "an over-length name_hash must error, never be silently truncated to the first 32 bytes"
        );
    }
}

/// Decode a 64-byte Ed25519 signature from hex and verify it (binding-strictly,
/// via `verify_detached` — small-order/weak keys are rejected) against
/// `message`. The one primitive every actor-key verifier funnels through, so the
/// weak-key guarantee (`weak_key_probe_tests`) holds on every tagged path.
pub(crate) fn verify_detached_hex(
    actor_bytes: &[u8; 32],
    message: &[u8],
    signature_hex: &str,
) -> Result<(), &'static str> {
    let sig_bytes = hex::decode(signature_hex)
        .ok()
        .filter(|b| b.len() == 64)
        .ok_or("invalid signature hex")?;
    if fauna_core::identity::verify_detached(actor_bytes, message, &sig_bytes) {
        Ok(())
    } else {
        Err("signature verification failed")
    }
}

/// Verify a `fauna.auth.claim_admin` signature: domain-tagged only, over
/// `CLAIM_ADMIN_V1 ‖ actor_id ‖ timestamp_be`
/// (`fauna_protocol::claim::claim_admin_signed_message`), so a captured login
/// handshake or lockout signature can never satisfy it (item 1 of the
/// finding — the untagged accept was deleted under the
/// 2026-08-17 no-existing-users ratification; rule #8 owns the registry).
pub fn verify_claim_admin_signature(
    actor_bytes: &[u8; 32],
    timestamp: u64,
    signature_hex: &str,
) -> Result<(), &'static str> {
    let tagged = fauna_protocol::claim::claim_admin_signed_message(actor_bytes, timestamp);
    verify_detached_hex(actor_bytes, &tagged, signature_hex)
}

/// Verify a `fauna.account.lockout` signature: domain-tagged only, over
/// `ACCOUNT_LOCKOUT_V1 ‖ actor_id ‖ timestamp_be`
/// (`fauna_protocol::account::account_lockout_signed_message`) — structural
/// separation from claim-admin / login rather than resting on the seconds-vs-ms
/// value range (item 2 of the finding).
pub fn verify_account_lockout_signature(
    actor_bytes: &[u8; 32],
    timestamp: u64,
    signature_hex: &str,
) -> Result<(), &'static str> {
    let tagged = fauna_protocol::account::account_lockout_signed_message(actor_bytes, timestamp);
    verify_detached_hex(actor_bytes, &tagged, signature_hex)
}

#[cfg(test)]
mod weak_key_probe_tests {
    use super::{verify_account_lockout_signature, verify_claim_admin_signature};

    /// PROBE-370-A — the nest's shared actor-signature gate must be *binding*.
    ///
    /// The permissive `VerifyingKey::verify` is not a signature check at all for
    /// a small-order public key: an all-zero `actor_id` is the order-1 point, and
    /// an all-zero signature (`R = 0`, `s = 0`) satisfies the verification
    /// equation for every message whose challenge scalar clears the key's order.
    /// `actor_id` is attacker-supplied on every route the shared
    /// `verify_detached_hex` funnel serves (claim-admin, lockout), so a
    /// non-binding gate there admits requests for identities nobody holds a key
    /// to.
    ///
    /// This pins the same property on the nest side that a prior sweep
    /// did not reach.
    #[test]
    fn a_small_order_actor_id_never_verifies() {
        let weak = [0u8; 32];
        let zero_sig = "0".repeat(128);

        let accepted = (0u64..64)
            .filter(|ts| {
                verify_claim_admin_signature(&weak, *ts, &zero_sig).is_ok()
                    || verify_account_lockout_signature(&weak, *ts, &zero_sig).is_ok()
            })
            .count();

        assert_eq!(
            accepted, 0,
            "the all-zero (small-order) actor_id forged a valid signature on \
             {accepted} of 64 messages — the gate is not binding"
        );
    }
}

/// Actor-key domain separation. These pin the
/// verifiers' tagged-only contract: the invariant red-verifies against the
/// known historical defect (a login signature verifying as a claim), which is
/// now structurally impossible — no untagged accept path exists.
#[cfg(test)]
mod actor_sig_domain_tests {
    use super::{verify_account_lockout_signature, verify_claim_admin_signature};
    use ed25519_dalek::{Signer, SigningKey};

    fn key_and_actor(seed: u8) -> (SigningKey, [u8; 32]) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let actor = sk.verifying_key().to_bytes();
        (sk, actor)
    }

    /// Sign the BARE `actor_id ‖ timestamp_be` — the byte string that the
    /// finding was about: what a no-nonce login handshake and `claim_admin`
    /// both produced/verified before the sweep.
    fn bare_shaped_signature(sk: &SigningKey, actor: &[u8; 32], ts: u64) -> String {
        let mut m = Vec::with_capacity(40);
        m.extend_from_slice(actor);
        m.extend_from_slice(&ts.to_be_bytes());
        hex::encode(sk.sign(&m).to_bytes())
    }

    /// The escalation, closed structurally: neither the legacy bare login shape
    /// nor a CURRENT tagged login signature can ever be replayed as a claim —
    /// the claim verifier accepts exactly the claim-admin-tagged message and
    /// nothing else. A correctly-tagged claim still verifies.
    #[test]
    fn login_signature_can_never_claim() {
        let (sk, actor) = key_and_actor(0x11);
        let ts = 1_700_000_000_000u64;

        // (a) The pre-sweep bare 40-byte shape → refused (no untagged accept
        //     exists any more).
        let bare_sig = bare_shaped_signature(&sk, &actor, ts);
        assert!(
            verify_claim_admin_signature(&actor, ts, &bare_sig).is_err(),
            "a bare legacy login-shaped signature must never claim"
        );

        // (b) A CURRENT login signature (tagged with AUTH_HANDSHAKE_V2) →
        //     refused (wrong domain tag).
        let login_msg =
            fauna_protocol::auth::handshake_signed_message(&actor, ts, &[0x5e; 32], &[0x42; 32]);
        let login_sig = hex::encode(sk.sign(&login_msg).to_bytes());
        assert!(
            verify_claim_admin_signature(&actor, ts, &login_sig).is_err(),
            "a tagged login signature lacks the claim-admin tag"
        );

        // (c) The correct domain-tagged claim signature verifies.
        let tagged = fauna_protocol::claim::claim_admin_signed_message(&actor, ts);
        let claim_sig = hex::encode(sk.sign(&tagged).to_bytes());
        assert!(verify_claim_admin_signature(&actor, ts, &claim_sig).is_ok());
    }

    /// Cross-context: a signature valid as a lockout (tagged) is NOT valid as a
    /// claim (tagged), and vice versa — the two actor-key contexts can never be
    /// confused.
    #[test]
    fn claim_and_lockout_tagged_signatures_do_not_cross() {
        let (sk, actor) = key_and_actor(0x33);
        let ts = 1_700_000_000u64;
        let claim_msg = fauna_protocol::claim::claim_admin_signed_message(&actor, ts);
        let lockout_msg = fauna_protocol::account::account_lockout_signed_message(&actor, ts);
        let claim_sig = hex::encode(sk.sign(&claim_msg).to_bytes());
        let lockout_sig = hex::encode(sk.sign(&lockout_msg).to_bytes());

        // Right tag → ok; wrong tag → refused.
        assert!(verify_claim_admin_signature(&actor, ts, &claim_sig).is_ok());
        assert!(verify_account_lockout_signature(&actor, ts, &lockout_sig).is_ok());
        assert!(verify_claim_admin_signature(&actor, ts, &lockout_sig).is_err());
        assert!(verify_account_lockout_signature(&actor, ts, &claim_sig).is_err());
    }
}

fn spawn_replicate_inbox(state: Arc<AppState>, actor_id: [u8; 32], body: Vec<u8>, row_id: i64) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        let handle = match state.bridge.worker.get_handle().await {
            Some(h) => h,
            None => return,
        };
        let actor_hex = hex::encode(actor_id);
        let payload_hex = hex::encode(&body);
        match handle
            .store(
                crate::nest_link::protocol::PayloadKind::Inbox,
                &actor_hex,
                &payload_hex,
                Some(row_id),
            )
            .await
        {
            Ok(true) => {
                if let Err(e) = state
                    .db
                    .mark_replicated("inbox", &actor_id, Some(row_id))
                    .await
                {
                    tracing::warn!("failed to mark inbox replicated: {e}");
                }
            }
            Ok(false) => {
                tracing::warn!("worker rejected inbox store for {actor_hex}");
            }
            Err(e) => {
                tracing::warn!("inbox replication failed: {e}");
            }
        }
    });
}

/// Whether `author`'s posts are forwarded from this nest: it is private (the
/// live client-set `AppState.node_mode`, not the boot seed) and the author
/// holds a live local pairing row carrying `post_forward` — the user's own
/// choice in the app, no config-file switch (`private-mode.md`
/// § Implementation status today, ruled 2026-10-01). A failed pairing read
/// forwards nothing (logged): the post itself is already stored.
async fn author_forwards_posts(state: &Arc<AppState>, author: &[u8; 32]) -> bool {
    if !crate::nest_sync_worker::is_private(state).await {
        return false;
    }
    match state.db.author_pairing_targets(author).await {
        Ok(rows) => rows.iter().any(|r| {
            !r.expired && r.has_capability(fauna_protocol::pair::capability::POST_FORWARD)
        }),
        Err(e) => {
            tracing::warn!("post forwarding: pairing read failed, not queueing: {e}");
            false
        }
    }
}

/// Relay a just-ingested post to the nests its author paired this one with, if
/// this nest is private and the author's pairing row carries `post_forward`
/// (`private-mode.md` § Post Forwarding). Enqueues the post's canonical `EmbedAsBytes` **body verbatim** —
/// the same bytes `store_post` wrote, whose blake3 is the content-addressed
/// `post_id`, so the peer derives the identical id without a re-encode.
///
/// No nest signature rides along: the outbox worker sends over the federation
/// channel, which authenticates this nest to the peer once at handshake, and the
/// post already carries its author's sign-over-CID envelope end-to-end. (The
/// retired HTTP twin's nest-signed JSON `ForwardedPost` envelope is gone with
/// the route it served.)
///
async fn maybe_enqueue_outbox(state: &Arc<AppState>, author: &[u8; 32], body: &[u8]) {
    if !author_forwards_posts(state, author).await {
        return;
    }

    if let Err(e) = state
        .db
        .outbox_enqueue(author, body, crate::outbox::ENTRY_TYPE_FORWARDED_POST)
        .await
    {
        // Non-fatal: the post is already stored locally. Losing the relay of one
        // post must never fail the author's create.
        tracing::warn!("failed to enqueue post to outbox: {e}");
    }
}

/// The delete twin of [`maybe_enqueue_outbox`]: on a private nest whose author
/// forwards posts, queue an author-signed post deletion for relay to the
/// paired nests, so a forwarded copy of the post there does not outlive
/// the original (`feed.md` § State & data shape → *Post deletion* →
/// Propagation). `signed_tombstone` is the verbatim signed embed-as-bytes
/// `Tombstone` (the `req.body` from `fauna.posts.delete`), so the peer
/// re-verifies the author envelope exactly as the forward leg does. Called only
/// from `posts_delete_handler` (the signed surface) — `unrepost` builds an
/// unsigned tombstone and so has no author-signed artifact to relay.
pub(crate) async fn maybe_enqueue_delete_outbox(
    state: &Arc<AppState>,
    author: &[u8; 32],
    signed_tombstone: &[u8],
) {
    if !author_forwards_posts(state, author).await {
        return;
    }

    if let Err(e) = state
        .db
        .outbox_enqueue(
            author,
            signed_tombstone,
            crate::outbox::ENTRY_TYPE_FORWARDED_DELETE,
        )
        .await
    {
        // Non-fatal: the post is already deleted locally. Losing the relay of
        // one deletion must never fail the author's delete.
        tracing::warn!("failed to enqueue post deletion to outbox: {e}");
    }
}

/// The producer half of post forwarding: `fauna.posts.create` → `ingest_post_core`
/// → outbox. These tests exist because the previous builder was never *called* —
/// it type-checked, carried a signature test, and forwarded nothing. Asserting
/// the enqueue happens **through `ingest_post_core`** is the point; a test that
/// only exercised `maybe_enqueue_outbox` directly would have passed against the
/// dead code too. The wire half is `conformance_cross_nest_post_forward.rs`.
#[cfg(test)]
mod outbox_producer_tests {
    use super::*;
    use crate::config::NodeMode;
    use crate::db::CacheDb;
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    fn state_for(mode: NodeMode) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        Arc::new(AppState::for_test_with_node_mode(db, mode))
    }

    /// `author`'s own pairing row on this nest, as `fauna.pair.add` stores it.
    async fn pair(state: &Arc<AppState>, author: &ActorKeypair, capabilities: &[&str]) {
        let caps: Vec<String> = capabilities.iter().map(|c| c.to_string()).collect();
        state
            .db
            .store_pairing(
                &author.actor_id().0,
                &[0x5au8; 32],
                &caps,
                None,
                Some("https://relay.example"),
                None,
            )
            .await
            .unwrap();
    }

    const POST_FORWARD: &str = fauna_protocol::pair::capability::POST_FORWARD;

    /// `ingest_post_core`, panicking on rejection. (`PostCreateError` is not
    /// `Debug`, so `.expect()` is unavailable.)
    async fn ingest(state: &Arc<AppState>, author: &ActorKeypair, body: &Bytes) -> [u8; 32] {
        match ingest_post_core(state, author.actor_id().0, body).await {
            Ok(post_id) => post_id,
            Err(_) => panic!("ingest_post_core rejected a well-formed signed post"),
        }
    }

    /// The embed-as-bytes wire shape a client posts (and `store_post` stores).
    fn signed_post_body(author: &ActorKeypair) -> Bytes {
        let post = Post {
            author: author.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "relayed".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (post_bytes, post_env) = sign_envelope(author, &post).unwrap();
        let wire = EmbedAsBytes::from_signed(post_bytes, post_env);
        Bytes::from(canonical_encode(&wire).unwrap())
    }

    /// A post created on a private nest by an author whose pairing row carries
    /// `post_forward` is queued **verbatim** — no config-file switch involved —
    /// and its queued bytes hash to the very `post_id` the ingest returned —
    /// the content-address identity the peer relies on.
    #[tokio::test]
    async fn private_nest_with_forwarding_enqueues_the_stored_body_verbatim() {
        let state = state_for(NodeMode::Private);
        let author = ActorKeypair::from_secret([0x09u8; 32]);
        pair(&state, &author, &[POST_FORWARD]).await;
        let body = signed_post_body(&author);

        let post_id = ingest(&state, &author, &body).await;

        let pending = state.db.outbox_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1, "the created post was queued for relay");
        assert_eq!(
            pending[0].entry_type,
            crate::outbox::ENTRY_TYPE_FORWARDED_POST
        );
        assert_eq!(
            pending[0].payload,
            body.to_vec(),
            "queued verbatim — no re-encode, no JSON, no nest signature"
        );
        assert_eq!(
            blake3::hash(&pending[0].payload).as_bytes(),
            &post_id,
            "the queued bytes are content-addressed by the same post_id"
        );
        // The row's one person column, stamped by the producer: what the
        // author's deletion finds the queued forward by.
        assert_eq!(
            state.db.outbox_author_stamps().await.unwrap(),
            vec![author.actor_id().0.to_vec()]
        );
    }

    ///  One user re-uploading another's validly signed
    /// post is refused at the door, so the forward queue can never hold a row
    /// stamped with the uploader rather than the author — a row the author's
    /// deletion would miss and the worker would forward after they are gone.
    /// The author's own upload still queues under the author, and their
    /// deletion takes it.
    #[tokio::test]
    async fn a_reupload_of_anothers_post_is_refused_and_never_queued() {
        let state = state_for(NodeMode::Private);
        let author = ActorKeypair::from_secret([0x0cu8; 32]);
        let uploader = ActorKeypair::from_secret([0x0du8; 32]);
        pair(&state, &author, &[POST_FORWARD]).await;
        pair(&state, &uploader, &[POST_FORWARD]).await;
        let body = signed_post_body(&author);

        assert!(
            ingest_post_core(&state, uploader.actor_id().0, &body)
                .await
                .is_err(),
            "a post signed by someone else is not the uploader's to create"
        );
        assert_eq!(state.db.outbox_depth().await.unwrap(), 0);

        ingest(&state, &author, &body).await;
        assert_eq!(
            state.db.outbox_author_stamps().await.unwrap(),
            vec![author.actor_id().0.to_vec()]
        );
        state
            .db
            .purge_orphaned_actor_rows(&author.actor_id().0)
            .await
            .unwrap();
        assert_eq!(state.db.outbox_depth().await.unwrap(), 0);
    }

    /// A public nest never forwards — it *is* the relay.
    #[tokio::test]
    async fn public_nest_does_not_enqueue() {
        let state = state_for(NodeMode::Public);
        let author = ActorKeypair::from_secret([0x0au8; 32]);
        pair(&state, &author, &[POST_FORWARD]).await;
        ingest(&state, &author, &signed_post_body(&author)).await;
        assert_eq!(state.db.outbox_depth().await.unwrap(), 0);
    }

    /// Private, but the author's pairing row does not carry `post_forward`
    /// (or the author has no row at all): nothing is queued.
    #[tokio::test]
    async fn an_author_whose_row_lacks_post_forward_is_not_enqueued() {
        let state = state_for(NodeMode::Private);
        let paired = ActorKeypair::from_secret([0x0bu8; 32]);
        pair(
            &state,
            &paired,
            &[fauna_protocol::pair::capability::MAIL_PULL],
        )
        .await;
        ingest(&state, &paired, &signed_post_body(&paired)).await;
        let unpaired = ActorKeypair::from_secret([0x0eu8; 32]);
        ingest(&state, &unpaired, &signed_post_body(&unpaired)).await;
        assert_eq!(state.db.outbox_depth().await.unwrap(), 0);
    }

    /// One author's row decides for that author only: a housemate's
    /// `post_forward` row forwards nothing of theirs.
    #[tokio::test]
    async fn forwarding_follows_each_authors_own_row() {
        let state = state_for(NodeMode::Private);
        let forwarder = ActorKeypair::from_secret([0x0fu8; 32]);
        let housemate = ActorKeypair::from_secret([0x10u8; 32]);
        pair(&state, &forwarder, &[POST_FORWARD]).await;
        ingest(&state, &housemate, &signed_post_body(&housemate)).await;
        ingest(&state, &forwarder, &signed_post_body(&forwarder)).await;
        assert_eq!(
            state.db.outbox_author_stamps().await.unwrap(),
            vec![forwarder.actor_id().0.to_vec()]
        );
    }
}

/// Fire-and-forget replication of a post to the worker.
/// Create-side bridge fan-out: the produce legs that mirror a stored post
/// outward under the author's own per-actor bridge opt-ins — the Bluesky
/// write-through, the ActivityPub `Create`-push and the nostr create-side
/// publish. Fires wherever a post
/// enters the store as a **new** row: a local `fauna.posts.create`
/// (`ingest_post_core`) and the forwarded-post receive on a paired public
/// nest (`federation_handlers::post_forward_handler`) alike — fan-out
/// authority is the author's enablement state on this nest, never the post's
/// arrival path (`activitypub.md` § The produce direction, paired-deployments
/// bullet). Each leg is fire-and-forget + non-fatal and self-gates on the
/// author's opt-in (`write_through` mode / enabled AP account / a deposited
/// nostr key plus a nostr reference or `auto_publish`), so it is a
/// clean no-op for authors with no bridge linked. Storage replication and
/// the private-mode outbox enqueue are deliberately NOT part of this set —
/// they are not per-actor bridge legs.
#[cfg_attr(
    not(any(feature = "bluesky", feature = "activitypub", feature = "nostr")),
    allow(unused_variables)
)]
pub(crate) fn spawn_post_bridge_fanout(
    state: &Arc<AppState>,
    author: [u8; 32],
    post_id: [u8; 32],
    body: &[u8],
) {
    // Count the fan-out at the point it is DECIDED — synchronously, inside the
    // caller's handler, before that handler replies. This is the observable a
    // negative assert anchors to ("the re-delivered forward did not fan out
    // again"); see `post_fanout_test_hook` for why the initiation point is the
    // only sound place for it, and why the caller's RPC reply is then a
    // sufficient barrier.
    #[cfg(feature = "test-hooks")]
    state.post_fanout_initiations.note_initiated();
    #[cfg(feature = "bluesky")]
    crate::bluesky::spawn_write_through(state.clone(), author, post_id, body.to_vec());
    #[cfg(feature = "activitypub")]
    crate::activitypub::push::spawn_create_push(state.clone(), author, post_id, body.to_vec());
    #[cfg(feature = "nostr")]
    crate::nostr::publish::spawn_create_publish(state.clone(), author, post_id, body.to_vec());
}

fn spawn_replicate_post(state: Arc<AppState>, post_id: [u8; 32], body: Vec<u8>) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        let handle = match state.bridge.worker.get_handle().await {
            Some(h) => h,
            None => return,
        };
        let post_hex = hex::encode(post_id);
        let payload_hex = hex::encode(&body);
        match handle
            .store(
                crate::nest_link::protocol::PayloadKind::Post,
                &post_hex,
                &payload_hex,
                None,
            )
            .await
        {
            Ok(true) => {
                if let Err(e) = state.db.mark_replicated("post", &post_id, None).await {
                    tracing::warn!("failed to mark post replicated: {e}");
                }
            }
            Ok(false) => {
                tracing::warn!("worker rejected post store for {post_hex}");
            }
            Err(e) => {
                tracing::warn!("post replication failed: {e}");
            }
        }
    });
}

/// Propagate a post deletion to the paired public nest's replica — the delete
/// twin of [`spawn_replicate_post`]. Fire-and-forget + non-fatal: a clean no-op
/// when no worker is connected, and any failure only logs (the local delete has
/// already succeeded — best-effort eventual consistency, like the other
/// post-delete propagation legs). On success the replication-tracking row is
/// removed so `replication_count` never over-counts a post that no longer
/// exists.
fn spawn_replicate_delete(state: Arc<AppState>, post_id: [u8; 32]) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        let handle = match state.bridge.worker.get_handle().await {
            Some(h) => h,
            None => return,
        };
        replicate_delete_once(&state, &handle, post_id).await;
    });
}

/// One attempt at removing a post's replica through `handle` — the body of
/// [`spawn_replicate_delete`], shared with the `post_delete_redrive` re-drive.
/// Clears the `worker_replication` marker only on the worker's ack, so a
/// failed attempt leaves the row a later re-drive finds. Returns whether the
/// worker acked.
pub(crate) async fn replicate_delete_once(
    state: &Arc<AppState>,
    handle: &crate::nest_link::proxy::WorkerHandle,
    post_id: [u8; 32],
) -> bool {
    let post_hex = hex::encode(post_id);
    match handle
        .delete(crate::nest_link::protocol::PayloadKind::Post, &post_hex)
        .await
    {
        Ok(true) => {
            if let Err(e) = state.db.unmark_replicated("post", &post_id, None).await {
                tracing::warn!("failed to unmark post replicated: {e}");
            }
            true
        }
        Ok(false) => {
            tracing::warn!("worker rejected post delete for {post_hex}");
            false
        }
        Err(e) => {
            tracing::warn!("post delete replication failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod loopback_gate_tests {
    //! Dispatch-layer tests for the loopback gate on `requires_loopback_peer`
    //! kinds — bridge self-enrollment is trusted only from a same-host peer; a
    //! remote source IP is refused. Per `mail-bridge-lifecycle.md` § Cold boot.
    use super::*;
    use std::sync::Arc;

    fn enroll_request_frame() -> fauna_protocol::Request {
        // The loopback gate fires on (kind, peer_addr) *before* the payload is
        // decoded, so a Null payload is sufficient to exercise the gate.
        fauna_protocol::Request {
            ty: fauna_protocol::Request::TYPE,
            correlation_id: 1,
            kind: "fauna.bridges.request_enrollment".to_string(),
            idempotency_key: [0u8; 16],
            payload: fauna_protocol::Value::Null,
            replay_forbidden: None,
            deadline_ms: None,
        }
    }

    async fn recv_error(
        rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) -> fauna_protocol::RpcError {
        let bytes = rx.recv().await.expect("a reply frame");
        match fauna_protocol::decode_frame(&bytes).expect("decode reply frame") {
            fauna_protocol::Frame::Reply(reply) => {
                assert!(!reply.ok, "expected an error reply");
                let pbytes = fauna_protocol::encode_canonical(&reply.payload).unwrap();
                fauna_protocol::decode_strict::<fauna_protocol::RpcError>(&pbytes).unwrap()
            }
            other => panic!("expected a Reply frame, got {other:?}"),
        }
    }

    async fn anon_state() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        Arc::new(AppState::for_test(db))
    }

    #[tokio::test]
    async fn request_enrollment_refused_from_non_loopback_peer() {
        let state = anon_state().await;
        let remote: std::net::SocketAddr = "203.0.113.7:9999".parse().unwrap();
        let (conn, mut rx) = state.ws.subscribe_anonymous(Some(remote));

        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            enroll_request_frame(),
        )
        .await;

        let err = recv_error(&mut rx).await;
        assert_eq!(err.code, "fauna.bridges.remote_enrollment_unsupported");
    }

    #[tokio::test]
    async fn request_enrollment_refused_when_peer_addr_unknown() {
        // No ConnectInfo (a connection with no peer addr — both listener paths
        // populate it in production, so this is the test-only / unknown case) ⇒
        // treated as non-loopback ⇒ refused.
        let state = anon_state().await;
        let (conn, mut rx) = state.ws.subscribe_anonymous(None);

        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            enroll_request_frame(),
        )
        .await;

        let err = recv_error(&mut rx).await;
        assert_eq!(err.code, "fauna.bridges.remote_enrollment_unsupported");
    }

    #[tokio::test]
    async fn request_enrollment_from_loopback_peer_passes_the_gate() {
        // A loopback peer passes the loopback gate, so dispatch proceeds past it
        // to kind lookup. The error here is therefore NOT the loopback refusal —
        // proving the gate let it through. (`AppState::for_test`'s router does
        // not register handlers, so the next step surfaces `unknown_kind`; the
        // handler itself is covered by the `bridge_blob_handlers` tests.)
        let state = anon_state().await;
        let local: std::net::SocketAddr = "127.0.0.1:5555".parse().unwrap();
        let (conn, mut rx) = state.ws.subscribe_anonymous(Some(local));

        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            enroll_request_frame(),
        )
        .await;

        let err = recv_error(&mut rx).await;
        assert_ne!(
            err.code, "fauna.bridges.remote_enrollment_unsupported",
            "loopback peer must pass the gate, not be refused"
        );
        assert_eq!(err.code, "fauna.protocol.unknown_kind");
    }
}

#[cfg(test)]
mod new_sign_in_notice_tests {
    //! New-IP detection through the real dispatcher (`login.md` § the
    //! handshake's side effects → *New-IP detection*): the anonymous
    //! connection's recorded peer reaches both bearer mints through
    //! `dispatch_core::current_caller_ip`, and a mint from an address other
    //! than the actor's last one rings exactly one `security.notice` row.
    //!
    //! Driven through `dispatch_request` on `subscribe_anonymous(Some(addr))`
    //! connections — the one seam where a test can choose the caller's
    //! address — so these pins cover the threading, not just the core.
    use super::*;
    use ed25519_dalek::Signer;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::auth::{ChallengeReply, ChallengeRequest, HandshakeRequest, VerifyRequest};
    use std::sync::Arc;

    async fn auth_state() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        Arc::new(AppState {
            rpc_router: Arc::new({
                let mut b = crate::rpc_router::RpcRouter::builder();
                crate::auth_handlers::register_auth_handlers(&mut b);
                b.build()
            }),
            ..AppState::for_test(db)
        })
    }

    fn to_value<T: serde::Serialize>(t: &T) -> fauna_protocol::Value {
        fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(t).unwrap()).unwrap()
    }

    /// One request on a fresh anonymous connection from `peer`; the ok reply's
    /// payload, decoded.
    async fn call<Req: serde::Serialize, Rep: serde::de::DeserializeOwned>(
        state: &Arc<AppState>,
        peer: &str,
        kind: &str,
        req: &Req,
    ) -> Rep {
        let peer: std::net::SocketAddr = peer.parse().unwrap();
        let (conn, mut rx) = state.ws.subscribe_anonymous(Some(peer));
        dispatch_request(
            Arc::clone(state),
            conn,
            fauna_protocol::Request {
                ty: fauna_protocol::Request::TYPE,
                correlation_id: 1,
                kind: kind.to_string(),
                idempotency_key: rand::random(),
                payload: to_value(req),
                replay_forbidden: None,
                deadline_ms: None,
            },
        )
        .await;
        let bytes = rx.recv().await.expect("a reply frame");
        match fauna_protocol::decode_frame(&bytes).expect("decode reply frame") {
            fauna_protocol::Frame::Reply(reply) => {
                let pbytes = fauna_protocol::encode_canonical(&reply.payload).unwrap();
                assert!(reply.ok, "{kind} from {peer} refused: {:?}", reply.payload);
                fauna_protocol::decode_strict(&pbytes).unwrap()
            }
            other => panic!("expected a Reply frame, got {other:?}"),
        }
    }

    /// The silent challenge — the ceremony every app-held bearer rides.
    async fn verify_from(state: &Arc<AppState>, who: &ActorKeypair, peer: &str) {
        let actor = who.actor_id().0;
        let issued: ChallengeReply = call(
            state,
            peer,
            "fauna.auth.challenge",
            &ChallengeRequest {
                actor_id: hex::encode(actor),
                extra: Default::default(),
            },
        )
        .await;
        let nonce = fauna_core::hex32::decode(&issued.nonce).unwrap();
        let nest = state.bound_identity();
        let msg = fauna_protocol::auth::challenge_verify_signed_message(&actor, &nonce, &nest);
        let _: fauna_protocol::Value = call(
            state,
            peer,
            "fauna.auth.verify",
            &VerifyRequest {
                actor_id: hex::encode(actor),
                nonce: issued.nonce,
                signature: hex::encode(who.signing_key().sign(&msg).to_bytes()),
                nest_id: hex::encode(nest),
                client_nonce: None,
                extra: Default::default(),
            },
        )
        .await;
    }

    /// The direct handshake — tests, scripts and machine-to-machine callers.
    async fn handshake_from(state: &Arc<AppState>, who: &ActorKeypair, peer: &str) {
        let actor = who.actor_id().0;
        let nest = state.bound_identity();
        let ts = fauna_core::data::Timestamp::now_millis();
        let client_nonce: [u8; 32] = rand::random();
        let msg = fauna_protocol::auth::handshake_signed_message(&actor, ts, &nest, &client_nonce);
        let _: fauna_protocol::Value = call(
            state,
            peer,
            "fauna.auth.handshake",
            &HandshakeRequest {
                actor_id: hex::encode(actor),
                timestamp: ts,
                signature: hex::encode(who.signing_key().sign(&msg).to_bytes()),
                client_nonce: fauna_protocol::ByteBuf::from(client_nonce.to_vec()),
                nest_id: hex::encode(nest),
                extra: Default::default(),
            },
        )
        .await;
    }

    async fn registered(state: &Arc<AppState>) -> ActorKeypair {
        let who = ActorKeypair::generate();
        state
            .db
            .create_user(&who.actor_id().0, "free", "test")
            .await
            .unwrap();
        who
    }

    /// The `security.notice` rows `actor` holds, once `want` of them have
    /// landed. The notice is spawned off the mint (it must not delay the
    /// reply), so this waits on the row count — a state condition, never a
    /// wall-clock sleep; the outer timeout only bounds a regression.
    async fn notices_once(state: &Arc<AppState>, actor: &[u8; 32], want: usize) -> Vec<String> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let rows: Vec<String> = state
                    .db
                    .list_notifications(actor, None, 50)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|r| {
                        r.notif_type == fauna_protocol::notifications::NotifType::SecurityNotice
                    })
                    .map(|r| r.summary)
                    .collect();
                if rows.len() >= want {
                    return rows;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fewer than {want} security notices ever landed"))
    }

    /// Sign-ins from A, A, B, B, C ring exactly twice — for B and for C. The
    /// first address ever seen records silently; a repeat of the last address
    /// never rings. Asserted once C's ring has landed: every silent mint was
    /// spawned (or not) before it.
    #[tokio::test]
    async fn verify_rings_once_per_new_address_and_never_for_the_same_one() {
        let state = auth_state().await;
        let who = registered(&state).await;
        let actor = who.actor_id().0;

        verify_from(&state, &who, "198.51.100.1:4000").await;
        verify_from(&state, &who, "198.51.100.1:4001").await;
        verify_from(&state, &who, "203.0.113.7:4000").await;
        verify_from(&state, &who, "203.0.113.7:5000").await;
        verify_from(&state, &who, "192.0.2.44:4000").await;

        let rows = notices_once(&state, &actor, 2).await;
        assert_eq!(rows.len(), 2, "one ring per address change: {rows:#?}");
        assert!(rows.iter().any(|s| s.contains("203.0.113.7")), "{rows:#?}");
        assert!(rows.iter().any(|s| s.contains("192.0.2.44")), "{rows:#?}");
        assert!(
            rows.iter().all(|s| s.contains("new sign-in")),
            "only new-sign-in notices: {rows:#?}"
        );
    }

    /// The handshake runs the same detection, and shares the last-address
    /// record with verify: a handshake from the address verify last used is
    /// silent, one from elsewhere rings.
    #[tokio::test]
    async fn handshake_shares_the_new_address_record_with_verify() {
        let state = auth_state().await;
        let who = registered(&state).await;
        let actor = who.actor_id().0;

        verify_from(&state, &who, "198.51.100.1:4000").await;
        handshake_from(&state, &who, "198.51.100.1:4002").await;
        handshake_from(&state, &who, "203.0.113.7:4000").await;

        let rows = notices_once(&state, &actor, 1).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert!(rows[0].contains("203.0.113.7"), "{rows:#?}");
    }
}

#[cfg(test)]
mod central_capability_gate_tests {
    //! Dispatch-layer tests for the central capability gate (gate 1d) — the
    //! `bridge_method_allowlist` chokepoint enforced in `dispatch_request` for
    //! authenticated connections (`docs/goal/architecture/apps/
    //! bridges.md` § Implementation status). These exercise the *gate* (the real
    //! `dispatch_request` path), distinct from the per-class matrix unit tests in
    //! `bridge_method_allowlist` and the per-handler `require_class` checks.
    use super::*;
    use crate::db::bridge_service_users::BridgeRole;
    use std::sync::Arc;

    /// AppState whose `rpc_router` is the *live* router (the empty `for_test`
    /// router would make gate 1d's `contains` check false, skipping it).
    fn state_with_live_router(db: Arc<crate::db::CacheDb>) -> Arc<AppState> {
        Arc::new(AppState {
            rpc_router: Arc::new(crate::build_rpc_router()),
            ..AppState::for_test(db)
        })
    }

    async fn approve_mda(state: &Arc<AppState>, bridge_actor: [u8; 32]) {
        state
            .db
            .create_pending_bridge_service_user(&bridge_actor, BridgeRole::Mda, "mda-gate-test")
            .await
            .unwrap();
        state
            .db
            .upsert_bridge_x25519(&bridge_actor, &[1u8; 32])
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&bridge_actor, None)
            .await
            .unwrap();
    }

    fn req_frame(kind: &str) -> fauna_protocol::Request {
        // Gate 1d fires on (kind, caller class) *before* the payload is decoded,
        // so a Null payload suffices to exercise it.
        fauna_protocol::Request {
            ty: fauna_protocol::Request::TYPE,
            correlation_id: 1,
            kind: kind.to_string(),
            idempotency_key: [0u8; 16],
            payload: fauna_protocol::Value::Null,
            replay_forbidden: None,
            deadline_ms: None,
        }
    }

    /// (ok, error_code). For an ok reply the code is empty.
    async fn recv_reply_code(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> (bool, String) {
        let bytes = rx.recv().await.expect("a reply frame");
        match fauna_protocol::decode_frame(&bytes).expect("decode reply frame") {
            fauna_protocol::Frame::Reply(reply) => {
                if reply.ok {
                    (true, String::new())
                } else {
                    let pbytes = fauna_protocol::encode_canonical(&reply.payload).unwrap();
                    let err =
                        fauna_protocol::decode_strict::<fauna_protocol::RpcError>(&pbytes).unwrap();
                    (false, err.code)
                }
            }
            other => panic!("expected a Reply frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_actor_denied_on_user_kind() {
        // The core behavior: a bridge actor (a compromised MTA/MDA) sending a
        // User-class kind is refused at the central gate, before the handler runs.
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let bridge_actor = [55u8; 32];
        let state = state_with_live_router(db);
        approve_mda(&state, bridge_actor).await;

        let (conn, mut rx) = state.ws.subscribe(bridge_actor);
        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            req_frame("fauna.subscriptions.tiers.list"),
        )
        .await;

        let (ok, code) = recv_reply_code(&mut rx).await;
        assert!(!ok, "bridge call to a User kind must be refused");
        // The refusal still fires at the central gate before the handler runs;
        // since the one-seam derivation a LISTED kind's class refusal
        // carries the kind's family code — the central
        // `fauna.bridges.permission_denied` is reserved for unknown/revoked
        // actors and unlisted kinds.
        assert_eq!(
            code, "fauna.subscriptions.permission_denied",
            "a listed kind's class refusal carries the kind's family code"
        );
    }

    #[tokio::test]
    async fn user_actor_passes_gate_on_user_kind() {
        // A regular user — a registered, non-bridge, non-admin actor →
        // CallerClass::User — sending a User-class kind passes the gate. The Null
        // payload then fails decode *inside the handler* — proving the gate let it
        // through (the error is anything but permission_denied). The `users` row is
        // load-bearing: since the authority gate denies an actor with no row, a
        // bare unseeded actor would be refused *at* the gate, not past it.
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let user_actor = [7u8; 32];
        db.create_user(&user_actor, "free", "gate-test user")
            .await
            .unwrap();
        let state = state_with_live_router(db);

        let (conn, mut rx) = state.ws.subscribe(user_actor);
        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            req_frame("fauna.subscriptions.tiers.list"),
        )
        .await;

        let (_ok, code) = recv_reply_code(&mut rx).await;
        assert_ne!(
            code, "fauna.bridges.permission_denied",
            "a User caller must pass the central gate for a User kind"
        );
    }

    #[tokio::test]
    async fn authed_actor_exempt_on_pre_identity_kind() {
        // Pre-identity kinds (discovery/bootstrap) are exempt from the class gate
        // even on an authenticated connection — an authed user may still call
        // `fauna.nest.info`, which is NOT in the class allowlist, so it must not be
        // gate-denied.
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let user_actor = [9u8; 32];
        let state = state_with_live_router(db);

        let (conn, mut rx) = state.ws.subscribe(user_actor);
        dispatch_request(
            Arc::clone(&state),
            Arc::clone(&conn),
            req_frame("fauna.nest.info"),
        )
        .await;

        let (_ok, code) = recv_reply_code(&mut rx).await;
        assert_ne!(
            code, "fauna.bridges.permission_denied",
            "a pre-identity kind must be exempt from the central gate"
        );
    }
}

#[cfg(test)]
mod family_reach_gate_tests {
    //! Unit tests for the family-safety reach gate inside
    //! `deliver_inbox_payload_core` — specifically the `federation_contact`
    //! knob, which only the federation-origin call path can trip (the WS-level
    //! contact-approval coverage lives in `tests/conformance_family.rs`).
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use std::sync::Arc;

    fn signed_payload(sender: &ActorKeypair, recipient: &[u8; 32]) -> Bytes {
        super::knock_bound_tests::signed_knock_payload(
            sender,
            recipient,
            b"http://peer.example",
            "hi",
        )
    }

    /// A supervised ward with a policy row; returns (ward_id, guardian_id).
    async fn supervised_ward(db: &crate::db::CacheDb) -> ([u8; 32], [u8; 32]) {
        let guardian = [9u8; 32];
        db.create_user_with_handle(&guardian, "personal", "parent", None)
            .await
            .unwrap();
        let ward_kp = ActorKeypair::generate();
        let ward = ward_kp.actor_id().0;
        db.create_user_with_handle(&ward, "free", "kid", Some(&guardian))
            .await
            .unwrap();
        (ward, guardian)
    }

    #[tokio::test]
    async fn federation_contact_off_suppresses_new_party_initiation_only() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let (ward, _guardian) = supervised_ward(&db).await;
        db.update_guardian_policy(
            &ward, false, "allow", false, "allow", None, None, None, None, None,
        )
        .await
        .unwrap();

        // A federation-origin stranger is suppressed — no knock stored.
        let stranger = ActorKeypair::generate();
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&stranger, &ward),
            ArrivalOrigin::Federation,
        )
        .await;
        assert!(
            matches!(
                outcome,
                InboxDeliveryOutcome::Rejected(InboxRejection::ContactsOnly)
            ),
            "federation stranger suppressed, got {outcome:?}"
        );
        assert!(
            db.poll_knocks(&ward).await.unwrap().is_empty(),
            "no knock stored for suppressed federation arrival"
        );

        // The same stranger arriving locally still knocks (default mode).
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&stranger, &ward),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "local stranger still knocks, got {outcome:?}"
        );

        // An ESTABLISHED cross-nest contact keeps delivering — the knob gates
        // initiation only.
        let friend = ActorKeypair::generate();
        db.upsert_contact(&ward, &friend.actor_id().0, "accepted")
            .await
            .unwrap();
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&friend, &ward),
            ArrivalOrigin::Federation,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::Delivered(_)),
            "established federation contact delivers, got {outcome:?}"
        );
    }

    /// The knock push carries the knock row's own localized body — the same
    /// `notifications.row_knock` key and args the row's `fauna.notification`
    /// push carries — so an OS-level knock notification is localized exactly
    /// like the row it announces (`behavior/notifications.md` § Localized body).
    #[tokio::test]
    async fn the_knock_push_carries_the_rows_localized_body() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let (ward, _guardian) = supervised_ward(&db).await;
        db.update_guardian_policy(
            &ward, true, "allow", true, "allow", None, None, None, None, None,
        )
        .await
        .unwrap();
        let (_conn, mut rx) = state.ws.subscribe(ward);

        let stranger = ActorKeypair::generate();
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&stranger, &ward),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "got {outcome:?}"
        );

        // Both pushes are queued synchronously inside the delivery call.
        let (mut knock, mut row) = (None, None);
        while let Ok(bytes) = rx.try_recv() {
            let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&bytes).unwrap()
            else {
                continue;
            };
            match fauna_protocol::PushEvent::from_push(&push.kind, push.payload) {
                fauna_protocol::PushEvent::Knock(p) => knock = Some(p),
                fauna_protocol::PushEvent::Notification(p) => row = Some(p),
                _ => {}
            }
        }
        let knock = knock.expect("a stored knock must push fauna.knock");
        let row = row.expect("a stored knock must push its fauna.notification row");

        let expected = knock_body(&stranger.actor_id().0, "hi");
        crate::db::notifications::assert_body_is_catalog_complete(&expected);
        assert_eq!(knock.body.as_ref(), Some(&expected));
        assert_eq!(
            knock.body, row.body,
            "the knock push and its row must say the same thing"
        );
        assert_eq!(knock.summary, "hi", "`summary` stays the knocker's message");
    }

    #[tokio::test]
    async fn contact_approval_forces_knock_even_in_open_mode() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let (ward, _guardian) = supervised_ward(&db).await;
        db.update_guardian_policy(
            &ward, true, "allow", true, "allow", None, None, None, None, None,
        )
        .await
        .unwrap();
        db.set_inbox_mode(&ward, "open").await.unwrap();

        let stranger = ActorKeypair::generate();
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&stranger, &ward),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "open-mode stranger forced onto the knock path, got {outcome:?}"
        );
        // Re-arrival while pending: KnockPending, not a duplicate knock.
        let outcome = deliver_inbox_payload_core(
            &state,
            &ward,
            &signed_payload(&stranger, &ward),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(matches!(
            outcome,
            InboxDeliveryOutcome::Rejected(InboxRejection::KnockPending)
        ));
    }
}

#[cfg(test)]
mod knock_bound_tests {
    //! A stranger's knock must not park unbounded bytes on a recipient who has
    //! not accepted them: the knocker-written fields are refused past a
    //! hard-coded size at the door, and a recipient holds at most
    //! `MAX_PENDING_KNOCKS_PER_RECIPIENT` knocks (`ui/contacts.md` § Persistence).
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use std::sync::Arc;

    pub(super) fn signed_knock_payload(
        sender: &ActorKeypair,
        recipient: &[u8; 32],
        sender_node: &[u8],
        summary: &str,
    ) -> Bytes {
        use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
        use fauna_core::encoding::{
            EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope,
        };
        let author = sender.actor_id();
        let post = Post {
            author,
            created_at: Timestamp::now(),
            body: PostBody::Structured {
                schema: "note/v1".into(),
                fields: vec![StructuredField {
                    key: "to".into(),
                    value: hex::encode(recipient),
                }],
                content: Some("hello".into()),
                facets: vec![],
                items: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (post_bytes, post_env) = sign_envelope(sender, &post).unwrap();
        let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);
        let post_id = compute_post_id(&post).unwrap();
        let cr = ContactRequest {
            sender: author,
            post_id,
            sender_node: sender_node.to_vec(),
            summary: summary.into(),
            created_at: Timestamp::now(),
        };
        let (cr_bytes, cr_env) = sign_envelope(sender, &cr).unwrap();
        let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);
        Bytes::from(canonical_encode(&(&cr_wire, &post_wire)).unwrap())
    }

    async fn recipient(db: &crate::db::CacheDb) -> [u8; 32] {
        let actor = ActorKeypair::generate().actor_id().0;
        db.create_user_with_handle(&actor, "free", "bob", None)
            .await
            .unwrap();
        actor
    }

    async fn notification_count(db: &crate::db::CacheDb, actor: &[u8; 32]) -> usize {
        db.list_notifications(actor, None, 1000)
            .await
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn an_over_cap_knock_summary_is_refused_and_stores_nothing() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let bob = recipient(&db).await;
        let stranger = ActorKeypair::generate();

        let summary = "x".repeat(MAX_KNOCK_SUMMARY_BYTES + 1);
        let outcome = deliver_inbox_payload_core(
            &state,
            &bob,
            &signed_knock_payload(&stranger, &bob, b"http://peer.example", &summary),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(
                outcome,
                InboxDeliveryOutcome::Rejected(InboxRejection::KnockTooLarge(_))
            ),
            "an over-cap summary is refused, got {outcome:?}"
        );
        assert!(
            db.poll_knocks(&bob).await.unwrap().is_empty(),
            "no knock row"
        );
        assert_eq!(
            notification_count(&db, &bob).await,
            0,
            "no notification row"
        );
        assert_eq!(
            db.get_contact_status(&bob, &stranger.actor_id().0)
                .await
                .unwrap(),
            None,
            "a refused knock leaves no pending edge"
        );

        // Exactly at the cap still knocks — the bound is generous, not a trim.
        let summary = "y".repeat(MAX_KNOCK_SUMMARY_BYTES);
        let outcome = deliver_inbox_payload_core(
            &state,
            &bob,
            &signed_knock_payload(&stranger, &bob, b"http://peer.example", &summary),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "an at-cap summary knocks, got {outcome:?}"
        );
        let knocks = db.poll_knocks(&bob).await.unwrap();
        assert_eq!(knocks.len(), 1);
        assert_eq!(knocks[0].summary, summary, "stored whole, never truncated");
    }

    #[tokio::test]
    async fn an_over_cap_sender_node_is_refused_and_stores_nothing() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let bob = recipient(&db).await;
        let stranger = ActorKeypair::generate();

        let node = vec![b'n'; MAX_KNOCK_SENDER_NODE_BYTES + 1];
        let outcome = deliver_inbox_payload_core(
            &state,
            &bob,
            &signed_knock_payload(&stranger, &bob, &node, "hi"),
            ArrivalOrigin::Local,
        )
        .await;
        assert!(
            matches!(
                outcome,
                InboxDeliveryOutcome::Rejected(InboxRejection::KnockTooLarge(_))
            ),
            "an over-cap sender_node is refused, got {outcome:?}"
        );
        assert!(
            db.poll_knocks(&bob).await.unwrap().is_empty(),
            "no knock row"
        );
        assert_eq!(
            notification_count(&db, &bob).await,
            0,
            "no notification row"
        );
    }

    /// The rotating-keypair flood: every knock comes from a fresh identity, so
    /// the per-(recipient, sender) dedup never fires. The per-recipient cap is
    /// what stops the table growing.
    #[tokio::test]
    async fn a_recipient_holds_at_most_the_pending_knock_cap() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let bob = recipient(&db).await;
        let flood_sender = |i: usize| {
            let mut sender = [0u8; 32];
            sender[..8].copy_from_slice(&(i as u64).to_le_bytes());
            sender
        };
        for i in 0..MAX_PENDING_KNOCKS_PER_RECIPIENT {
            db.push_knock(&bob, &flood_sender(i), b"n", "flood", &[])
                .await
                .unwrap();
        }

        let stranger = ActorKeypair::generate();
        let outcome = deliver_inbox_payload_core(
            &state,
            &bob,
            &signed_knock_payload(&stranger, &bob, b"http://peer.example", "hi"),
            ArrivalOrigin::Federation,
        )
        .await;
        assert!(
            matches!(
                outcome,
                InboxDeliveryOutcome::Rejected(InboxRejection::KnockQueueFull)
            ),
            "the knock past the cap is refused, got {outcome:?}"
        );
        assert_eq!(
            db.poll_knocks(&bob).await.unwrap().len(),
            MAX_PENDING_KNOCKS_PER_RECIPIENT,
            "the table did not grow past the cap"
        );
        assert_eq!(
            notification_count(&db, &bob).await,
            0,
            "no notification row"
        );

        // Clearing one frees one slot — the cap bounds storage, it is not a ban.
        db.dismiss_knock(&bob, &flood_sender(0)).await.unwrap();
        let outcome = deliver_inbox_payload_core(
            &state,
            &bob,
            &signed_knock_payload(&stranger, &bob, b"http://peer.example", "hi"),
            ArrivalOrigin::Federation,
        )
        .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "a freed slot takes the next knock, got {outcome:?}"
        );
    }

    /// `behavior/notifications.md` § Retention, rule 3: a re-knock supersedes
    /// its own doorbell. The path that leaves a doorbell standing with no
    /// knock behind it is accept-then-lapse: accept keeps the doorbell (the
    /// message's only home) and deletes the knock, then the accepted edge
    /// lapses to no-edge (`expire_accepted_contacts`'s outcome, reached here
    /// by `delete_contact`). The sender's second knock must then produce ONE
    /// fresh, unread doorbell rather than being swallowed by
    /// `insert_notification`'s `(actor, type, sender, content)` dedup.
    #[tokio::test]
    async fn a_re_knock_supersedes_the_senders_standing_doorbell() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let bob = recipient(&db).await;
        let stranger = ActorKeypair::generate();
        let sender = stranger.actor_id().0;
        let knock =
            |summary: &str| signed_knock_payload(&stranger, &bob, b"http://peer.example", summary);

        let outcome =
            deliver_inbox_payload_core(&state, &bob, &knock("first"), ArrivalOrigin::Federation)
                .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "{outcome:?}"
        );
        let first = db.list_notifications(&bob, None, 10).await.unwrap();
        assert_eq!(first.len(), 1);
        db.mark_notifications_read(&bob, i64::MAX).await.unwrap();

        assert!(
            crate::contacts_handlers::accept_contact_core(&state, &bob, &sender)
                .await
                .is_ok(),
            "accept ok"
        );
        assert_eq!(
            db.list_notifications(&bob, None, 10).await.unwrap().len(),
            1,
            "accept keeps the doorbell"
        );
        db.delete_contact(&bob, &sender).await.unwrap();
        let outcome =
            deliver_inbox_payload_core(&state, &bob, &knock("second"), ArrivalOrigin::Federation)
                .await;
        assert!(
            matches!(outcome, InboxDeliveryOutcome::KnockStored),
            "{outcome:?}"
        );

        let rows = db.list_notifications(&bob, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1, "one doorbell per sender — the latest");
        assert_ne!(rows[0].id, first[0].id, "a fresh row, not the old one");
        assert!(!rows[0].is_read, "and it rings again");
        assert!(rows[0].summary.contains("second"), "{}", rows[0].summary);
    }
}

#[cfg(test)]
mod upgrade_revocation_race_tests {
    //! The per-token revoke's upgrade-time TOCTOU —
    //! [`AppState::register_upgraded_connection`]
    //! (`transport-connection.md` § Connection lifecycle → *Revocation
    //! teardown* → *The per-token twin*).
    //!
    //! `fauna.sessions.{revoke,revoke_all}` deletes the token row and then
    //! sweeps `WsState.subs`. A connection whose bearer was validated before
    //! the revoke but which registers after the sweep was missed by both, and
    //! nothing downstream could deny it: `dispatch_core` never re-reads the
    //! token store, and the dispatch gate's authority read sees only the
    //! *actor*, which a per-token revoke leaves healthy. So it dispatched as
    //! `User` for ever. These pins park a real upgrade inside that window with
    //! [`super::upgrade_race`], run the whole revoke through the real handlers
    //! over a sibling session's socket, and only then let the registration
    //! complete.
    //!
    //! In-crate rather than beside its siblings in
    //! `tests/conformance_revocation_teardown.rs` only because the rendezvous
    //! is `#[cfg(test)]`, which an integration test cannot reach. Wire-shaped
    //! all the same: a real axum server, a real WebSocket over tungstenite, the
    //! real session handlers and `CacheDb`, no mocks.
    use super::*;
    use crate::rpc_router::{RpcKindMeta, RpcRouter};
    use fauna_core::identity::{ActorId, ActorKeypair};
    use fauna_protocol::sessions::{RevokeAllRequest, RevokeRequest};
    use fauna_protocol::{Frame, Request, Value, decode_frame, decode_strict, encode_canonical};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A real kind name with a trivial always-ok handler, so "did dispatch
    /// happen" is a clean signal — the same choice, for the same reason
    /// (gate 1d refuses an invented kind), as the integration file's echo.
    const ECHO_KIND: &str = "fauna.posts.create";

    struct Harness {
        url: String,
        kp: ActorKeypair,
        state: Arc<AppState>,
    }

    impl Harness {
        /// Mint a session for the harness actor: `(raw bearer, token_id)`.
        async fn mint(&self) -> (String, String) {
            self.mint_by(None).await
        }

        /// [`Self::mint`], optionally as if `device_key` had minted it over
        /// `fauna.auth.device_handshake`.
        async fn mint_by(&self, device_key: Option<[u8; 32]>) -> (String, String) {
            let minted = self
                .state
                .auth
                .token_store
                .insert_with_metadata(self.kp.actor_id(), 3600, None, device_key)
                .await;
            (minted.token, minted.token_id)
        }

        async fn open(&self, token: &str) -> Ws {
            let url = format!(
                "{}/api/v1/ws/{}",
                self.url,
                hex::encode(self.kp.actor_id().0)
            );
            let mut req = url.into_client_request().unwrap();
            req.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                format!("fauna.v1, bearer.{token}").parse().unwrap(),
            );
            let (ws, _) = tokio_tungstenite::connect_async(req)
                .await
                .expect("authed upgrade should succeed");
            ws
        }
    }

    async fn start() -> Harness {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let kp = ActorKeypair::generate();
        db.create_user(&kp.actor_id().0, "free", "alice")
            .await
            .unwrap();
        let mut b = RpcRouter::builder();
        b.add(
            ECHO_KIND,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler: Box::new(|_state, _actor, _payload| {
                    Box::pin(
                        async move { Ok(Bytes::from(encode_canonical(&true).unwrap().to_vec())) },
                    )
                }),
            },
        );
        crate::session_handlers::register_sessions_handlers(&mut b);
        let state = Arc::new(AppState {
            rpc_router: Arc::new(b.build()),
            ..AppState::for_test(db)
        });
        let app = crate::build_router(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test): the server under test; it lives for the test's
        // runtime and dies with it.
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Harness {
            url: format!("ws://{addr}"),
            kp,
            state,
        }
    }

    fn request(kind: &str, corr: u64, payload: Value) -> Message {
        Message::Binary(
            fauna_protocol::encode_frame(&Frame::Request(Request {
                ty: Request::TYPE,
                correlation_id: corr,
                kind: kind.to_string(),
                idempotency_key: [corr as u8; 16],
                payload,
                replay_forbidden: Some(false),
                deadline_ms: None,
            }))
            .unwrap(),
        )
    }

    fn echo(corr: u64) -> Message {
        request(ECHO_KIND, corr, Value::Null)
    }

    fn to_value<T: serde::Serialize>(t: &T) -> Value {
        decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
    }

    fn revoke(corr: u64, token_id: &str) -> Message {
        request(
            "fauna.sessions.revoke",
            corr,
            to_value(&RevokeRequest {
                token_id: token_id.to_string(),
                extra: Default::default(),
            }),
        )
    }

    fn revoke_all(corr: u64, keep_token_id: &str) -> Message {
        request(
            "fauna.sessions.revoke_all",
            corr,
            to_value(&RevokeAllRequest {
                keep_token_id: keep_token_id.to_string(),
                extra: Default::default(),
            }),
        )
    }

    /// Read until an `ok` Reply for `corr` on a socket expected to stay open;
    /// a close, an error or the hang-guard deadline is `false`.
    async fn ok_reply(ws: &mut Ws, corr: u64) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Binary(bytes)))) => match decode_frame(&bytes) {
                    Ok(Frame::Reply(r)) if r.correlation_id == corr => return r.ok,
                    _ => continue,
                },
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    return false;
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// Read until the socket closes: `(close code, whether an ok Reply for
    /// corr arrived first)`. The deadline is a hang guard only — every
    /// assertion below is on the close code and the reply, never on timing.
    async fn until_close(ws: &mut Ws, corr: u64) -> (Option<u16>, bool) {
        let mut replied = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Close(frame)))) => {
                    return (frame.map(|f| u16::from(f.code)), replied);
                }
                Ok(Some(Ok(Message::Binary(bytes)))) => {
                    if let Ok(Frame::Reply(r)) = decode_frame(&bytes)
                        && r.correlation_id == corr
                        && r.ok
                    {
                        replied = true;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_))) | Ok(None) | Err(_) => return (None, replied),
            }
        }
    }

    /// Park the doomed session's upgrade inside the window, run the revoke
    /// `revoke_frame(keeper_id, doomed_id)` builds to completion on the
    /// keeper's socket, then let the registration finish — and assert the raced
    /// socket closes 4401 having dispatched nothing, while the keeper carries
    /// on.
    async fn assert_raced_upgrade_is_closed(revoke_frame: impl FnOnce(&str, &str) -> Message) {
        let h = start().await;
        let (keeper_token, keeper_id) = h.mint().await;
        let (doomed_token, doomed_id) = h.mint().await;

        let mut keeper = h.open(&keeper_token).await;
        keeper.send(echo(1)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 1).await,
            "precondition: the keeper session dispatches"
        );

        let barrier = upgrade_race::arm(&doomed_id);
        // The 101 is out when this returns; server-side the upgrade is parked
        // in `register_upgraded_connection`, validated and not yet registered.
        let mut doomed = h.open(&doomed_token).await;
        barrier.validated.notified().await;

        // The WHOLE revoke, inside the window: its row delete and its socket
        // sweep have both run by the time its Reply is read, and the sweep
        // walked a `subs` the doomed connection has not joined.
        keeper
            .send(revoke_frame(&keeper_id, &doomed_id))
            .await
            .unwrap();
        assert!(
            ok_reply(&mut keeper, 2).await,
            "the revoke itself must succeed"
        );
        assert!(
            !h.state
                .auth
                .token_store
                .has_session(&h.kp.actor_id(), &doomed_id)
                .await,
            "precondition: the doomed session's row is gone before it registers"
        );

        barrier.may_register.notify_one();
        let _ = doomed.send(echo(3)).await;
        let (code, replied) = until_close(&mut doomed, 3).await;
        assert!(
            !replied,
            "a session revoked while its upgrade was in flight answered an RPC — \
             it registered after the revoke's sweep and nothing re-checked its \
             token, so it dispatches as `User` for ever"
        );
        assert_eq!(
            code,
            Some(4401),
            "the raced connection must close 4401, so the client discards the \
             bearer rather than reconnecting with it"
        );

        keeper.send(echo(4)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 4).await,
            "the re-check closed a session that was never revoked"
        );
    }

    #[tokio::test]
    async fn a_session_revoked_while_its_upgrade_is_in_flight_never_dispatches() {
        assert_raced_upgrade_is_closed(|_keep, doomed| revoke(2, doomed)).await;
    }

    #[tokio::test]
    async fn revoke_all_while_an_upgrade_is_in_flight_leaves_it_nothing_to_dispatch() {
        assert_raced_upgrade_is_closed(|keep, _doomed| revoke_all(2, keep)).await;
    }

    /// The ORDERING half of the argument, which the two pins above cannot see:
    /// they run the whole revoke inside the window, so a helper that swept
    /// before it deleted would pass them too. Here the doomed upgrade
    /// registers **after the helper's sweep and before the rest of the
    /// helper** ([`super::revoke_race`]), the one interleaving the
    /// delete-before-sweep order exists for. With the delete already done,
    /// the registration's re-read finds the row gone and closes the
    /// connection; a helper reordered to sweep first would reach its park
    /// with the row still present, and the connection would outlive both.
    ///
    /// `revoke` runs one sub-actor helper on a task of its own:
    /// `(state, actor, keeper token_id, doomed token_id, device key)`.
    async fn assert_registration_after_the_sweep_is_closed(
        revoke: impl FnOnce(
            Arc<AppState>,
            [u8; 32],
            String,
            String,
            [u8; 32],
        ) -> tokio::task::JoinHandle<()>,
    ) {
        let h = start().await;
        let actor = h.kp.actor_id().0;
        let device_key = [0xd7; 32];
        let (keeper_token, keeper_id) = h.mint().await;
        let (doomed_token, doomed_id) = h.mint_by(Some(device_key)).await;

        let mut keeper = h.open(&keeper_token).await;
        keeper.send(echo(1)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 1).await,
            "precondition: the keeper session dispatches"
        );

        let upgrade = upgrade_race::arm(&doomed_id);
        let mut doomed = h.open(&doomed_token).await;
        upgrade.validated.notified().await;

        let sweep = revoke_race::arm(actor);
        let task = revoke(
            Arc::clone(&h.state),
            actor,
            keeper_id,
            doomed_id.clone(),
            device_key,
        );
        // The sweep has run over a `subs` the doomed connection has not joined.
        sweep.swept.notified().await;
        upgrade.may_register.notify_one();
        upgrade.registered.notified().await;
        sweep.may_finish.notify_one();
        task.await.expect("the revoke helper panicked");
        assert!(
            !h.state
                .auth
                .token_store
                .has_session(&h.kp.actor_id(), &doomed_id)
                .await,
            "precondition: the revoke deleted the doomed session's row"
        );

        let _ = doomed.send(echo(3)).await;
        let (code, replied) = until_close(&mut doomed, 3).await;
        assert!(
            !replied,
            "a connection that registered after the revoke's sweep answered an RPC — \
             the helper swept before it deleted, so the registration's re-read \
             still found the row and the sweep had already missed it"
        );
        assert_eq!(code, Some(4401), "the raced connection must close 4401");

        keeper.send(echo(4)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 4).await,
            "the re-check closed a session that was never revoked"
        );
    }

    #[tokio::test]
    async fn revoke_session_authority_deletes_before_it_sweeps() {
        assert_registration_after_the_sweep_is_closed(|state, actor, _keep, doomed, _key| {
            // spawn-ok(test): the helper under test, joined before the asserts.
            tokio::spawn(async move { state.revoke_session_authority(&actor, &doomed).await })
        })
        .await;
    }

    #[tokio::test]
    async fn revoke_other_sessions_authority_deletes_before_it_sweeps() {
        assert_registration_after_the_sweep_is_closed(|state, actor, keep, _doomed, _key| {
            // spawn-ok(test): the helper under test, joined before the asserts.
            tokio::spawn(async move {
                state.revoke_other_sessions_authority(&actor, &keep).await;
            })
        })
        .await;
    }

    #[tokio::test]
    async fn revoke_device_authority_deletes_before_it_sweeps() {
        assert_registration_after_the_sweep_is_closed(|state, actor, _keep, _doomed, key| {
            // spawn-ok(test): the helper under test, joined before the asserts.
            tokio::spawn(async move {
                state.revoke_device_authority(&actor, &key).await;
            })
        })
        .await;
    }

    /// The re-read's own half of the ordering: it must run AFTER the
    /// subscribe. The upgrade park above sees a read hoisted to the top of
    /// `register_upgraded_connection`, but not one moved only to between that
    /// park and the subscribe. This pin parks the read itself once it has
    /// answered ([`crate::token_store::read_race`]) and runs the whole revoke
    /// there. Read after the subscribe, the answer "still held" means the
    /// connection is already in `subs`, so the revoke's sweep closes it. Read
    /// anywhere before the subscribe, the connection registers after the sweep
    /// on a stale answer and dispatches.
    #[tokio::test]
    async fn a_re_read_answered_before_a_revoke_is_backed_by_its_sweep() {
        let h = start().await;
        let (keeper_token, _) = h.mint().await;
        let (doomed_token, doomed_id) = h.mint().await;

        let mut keeper = h.open(&keeper_token).await;
        keeper.send(echo(1)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 1).await,
            "precondition: the keeper session dispatches"
        );

        let read = crate::token_store::read_race::arm(&doomed_id);
        let mut doomed = h.open(&doomed_token).await;
        // The registration re-read has answered "still held" and not returned.
        read.answered.notified().await;
        keeper.send(revoke(2, &doomed_id)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 2).await,
            "the revoke itself must succeed"
        );
        read.may_return.notify_one();

        let _ = doomed.send(echo(3)).await;
        let (code, replied) = until_close(&mut doomed, 3).await;
        assert!(
            !replied,
            "a connection whose re-read answered before a revoke answered an RPC — \
             the read ran before the subscribe, so the revoke's sweep walked a `subs` \
             the connection had not joined and the stale answer let it register"
        );
        assert_eq!(code, Some(4401), "the revoked connection must close 4401");

        keeper.send(echo(4)).await.unwrap();
        assert!(
            ok_reply(&mut keeper, 4).await,
            "the sweep closed a session that was never revoked"
        );
    }

    /// The control that keeps the window pins above honest: an upgrade parked
    /// in the same window with **no** revoke registers and dispatches
    /// normally. Without it, a rendezvous that broke the connection by itself
    /// would pass every one of them vacuously.
    #[tokio::test]
    async fn an_upgrade_parked_in_the_window_without_a_revoke_dispatches_normally() {
        let h = start().await;
        let (token, token_id) = h.mint().await;
        let barrier = upgrade_race::arm(&token_id);
        let mut ws = h.open(&token).await;
        barrier.validated.notified().await;
        barrier.may_register.notify_one();
        ws.send(echo(1)).await.unwrap();
        assert!(
            ok_reply(&mut ws, 1).await,
            "a parked upgrade whose session still stands must dispatch"
        );
    }

    /// The no-`token_id` arm is untouched: a connection that recorded no
    /// session has no row to re-read, and closing it here would be a guess.
    /// It keeps the conservative resolution each teardown gives it instead
    /// (`transport-connection.md` § *The per-token twin*). The contrast arm is
    /// the same registration naming a session that does not exist.
    #[tokio::test]
    async fn a_connection_with_no_token_id_is_never_closed_by_the_registration_re_read() {
        let state = AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()));
        let actor = [0x5e; 32];
        assert!(
            state
                .auth
                .token_store
                .list_sessions(&ActorId(actor))
                .await
                .is_empty(),
            "precondition: the actor holds no session row at all"
        );

        let (untracked, _rx) = state.register_upgraded_connection(actor, None, None).await;
        assert!(
            !untracked.is_revoked(),
            "a connection with no recorded token_id was closed by the re-read"
        );

        let (gone, _rx) = state
            .register_upgraded_connection(actor, Some("0123456789abcdef".into()), None)
            .await;
        assert!(
            gone.is_revoked(),
            "a connection whose session row is absent at registration must be \
             revoked before it can dispatch"
        );
    }
}
