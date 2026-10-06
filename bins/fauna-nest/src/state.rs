//! Sub-struct definitions for AppState decomposition.
//!
//! Each struct groups related fields from the monolithic AppState into a
//! cohesive unit.  Default impls create lightweight in-memory instances
//! suitable for tests; production code constructs them with real values.

use std::sync::Arc;

use crate::bridge_management::BridgeProviderRegistry;
use crate::challenge_auth::ChallengeStore;
use crate::chunk_relay::ChunkResolver;
#[cfg(feature = "bluesky")]
use crate::db::CacheDb;
use crate::nest_link::proxy::WorkerState;
use crate::routes::RegistrationConfig;
use crate::token_store::TokenStore;

// ---------------------------------------------------------------------------
// SyncState
// ---------------------------------------------------------------------------

/// The chunk relay (`chunk_relay`). (The former destination ORCHESTRATOR died
/// with the phantom `folder_destinations` rail, 2026-08-18 — delivery is the
/// remote-change nudge + each seat's catch-up pull; the legacy data plane's
/// device registry left with its route, 2026-10-02.)
pub struct SyncState {
    pub chunk_resolver: Arc<ChunkResolver>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            chunk_resolver: Arc::new(ChunkResolver::new(None, None, false)),
        }
    }
}

// ---------------------------------------------------------------------------
// EmailState
// ---------------------------------------------------------------------------

/// Email-related runtime state. The deployment's mail domain(s) are **not**
/// held here — every reader resolves the active primary from the runtime
/// `local_domains` table (`db::lookup_primary_mail_domain`), provisioned by a
/// client (the product invariant: nest config comes from clients, not boot
/// args). The legacy boot-time `domain` field was removed once the last test
/// seam migrated to a real `mail_domains` row. Mail-enabled *status*
/// (`fauna.setup.status`) reads the db `mail_enabled` toggle.
/// `inbound_deliver_key` is the key the in-process inbound handler validates
/// against. The legacy SMTP fields (`dkim_keys`, `inbox_events`, `clamd_host`,
/// `rspamd_url`) lived here until the I6 mail-bridge cutover.
pub struct EmailState {
    pub inbound_deliver_key: Option<String>,
    /// Production TLSRPT aggregator (RFC 8460). Constructed in
    /// `AppState` so the same `Arc<Mutex<…>>` is shared between every
    /// recorder + the daily emitter. The `report_tls_attempt` handler records
    /// one bucket per outbound TLS attempt the Go MTA bridge reports, and
    /// `spawn_tlsrpt_daily_dispatch` (nest-side) drains it at 00:00 UTC. Cheap
    /// when idle (empty HashMap).
    pub tlsrpt_aggregator:
        std::sync::Arc<std::sync::Mutex<fauna_mail::outbound::tlsrpt::TlsrptAggregator>>,
}

#[allow(clippy::derivable_impls)]
impl Default for EmailState {
    fn default() -> Self {
        Self {
            inbound_deliver_key: None,
            tlsrpt_aggregator: std::sync::Arc::new(std::sync::Mutex::new(
                fauna_mail::outbound::tlsrpt::TlsrptAggregator::default(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// BridgeState
// ---------------------------------------------------------------------------

/// Bridge provider registry and worker proxy state.
pub struct BridgeState {
    pub providers: Option<Arc<BridgeProviderRegistry>>,
    pub worker: WorkerState,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            providers: None,
            worker: WorkerState::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// AuthState
// ---------------------------------------------------------------------------

/// Authentication: token store, challenge store, registration config.
///
/// Per-source rate limiting of the anonymous registration / invite write
/// surfaces is **not** here — it lives on `AppState` as the uniform
/// `register_rate_limit` / `invite_request_rate_limit` (`bridge_rate_limit::Limiter`,
/// keyed on the real client IP, checked in the dispatcher), alongside the
/// discovery / claim / invite-verify limiters. The former governor
/// `registration_limiter` field was dead code (never `.check_key`'d after the
/// HTTP twin retired) and was removed with that wiring.
pub struct AuthState {
    pub token_store: Arc<TokenStore>,
    /// Short-TTL scoped tokens for the WebDAV MDA's bulk-byte plane. Disjoint
    /// from `token_store` so a bulk token can never validate as a full session
    /// bearer (`bulk_byte_token.rs`, `webdav-server.md` § Bulk-byte plane).
    pub bulk_byte_tokens: Arc<crate::bulk_byte_token::BulkByteTokenStore>,
    pub challenge_store: Arc<ChallengeStore>,
    /// Nonces for the **seed-escrow** restore ceremony
    /// (`identity-succession.md` § Seed escrow), in a store disjoint from
    /// `challenge_store`.
    ///
    /// Same type, separate instance — deliberately. The two ceremonies are
    /// authenticated by *different roots* (identity seed vs. offline
    /// RecoveryKey), so sharing one nonce pool would make a nonce minted for a
    /// sign-in presentable to the escrow fetch and vice-versa. The per-role
    /// domain tags already make the signatures non-interchangeable, but keeping
    /// the pools disjoint means that guarantee never rests on a single layer.
    pub escrow_challenge_store: Arc<ChallengeStore>,
    /// Nonces for the **replacement veto** ceremony
    /// (`identity-succession.md:37`) — the current RecoveryKey holder's
    /// pre-identity contest of a pending seed-initiated replacement. Third
    /// disjoint instance, same reasoning as `escrow_challenge_store`: one
    /// pool per ceremony, so a nonce minted for the escrow fetch is never
    /// presentable to the veto even though both are signed by the same root
    /// (the domain tags differ too — disjoint pools keep that guarantee from
    /// resting on a single layer).
    pub veto_challenge_store: Arc<ChallengeStore>,
    /// Single-use guard for verified direct-auth signatures (security review
    /// § L4) — closes the ±30 s replay window on `fauna.auth.handshake`.
    pub replay_guard: Arc<crate::auth_core::ReplayGuard>,
    /// Single-use nonces for the attested age claim
    /// (`fauna.account.age_nonce` → `age_attest::verify_age_attestation`;
    /// `family-safety.md` § The account age band). Its own pool, not a fourth
    /// `ChallengeStore`: the mint is pre-identity (no actor to bind — the
    /// actor is bound inside the platform-signed payload instead), so the
    /// entry shape differs.
    pub age_nonce_store: Arc<crate::age_attest::AgeNonceStore>,
    pub registration: RegistrationConfig,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            token_store: Arc::new(TokenStore::new()),
            bulk_byte_tokens: Arc::new(crate::bulk_byte_token::BulkByteTokenStore::new()),
            challenge_store: Arc::new(ChallengeStore::new()),
            escrow_challenge_store: Arc::new(ChallengeStore::new()),
            veto_challenge_store: Arc::new(ChallengeStore::new()),
            replay_guard: Arc::new(crate::auth_core::ReplayGuard::new()),
            age_nonce_store: Arc::new(crate::age_attest::AgeNonceStore::new()),
            registration: RegistrationConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// MlsState
// ---------------------------------------------------------------------------

/// MLS key distribution engine and transition threshold.
pub struct MlsState {
    pub engine: Option<Arc<fauna_mls::engine::MlsEngine>>,
    pub transition_threshold: usize,
}

impl Default for MlsState {
    fn default() -> Self {
        Self {
            engine: None,
            transition_threshold: 2000,
        }
    }
}

// ---------------------------------------------------------------------------
// BlueskyState
// ---------------------------------------------------------------------------

/// Bluesky bridge: the OAuth client and DM poller **derived from the
/// deployment's own identity domain**, plus the ingredients they are derived
/// from.
///
/// The client is not built at boot and is not configured by anyone. Its
/// `client_id` is `https://<identity-domain>/.well-known/atproto-oauth-client`,
/// and the identity domain is learned at CLAIM — after boot — so a boot-time
/// build could never see it. It is instead derived on demand by
/// [`Self::resolve`] and cached under the URL it was built for, so a later
/// domain change re-derives rather than serving a `client_id` Bluesky's
/// authorization server can no longer resolve.
///
/// Until 2026-09-02 this held a boot-built `Option` fed solely by the
/// `--bluesky-public-url` CLI flag. No shipped launch line passed that flag, so
/// the bridge was `available: false` on every real deployment; the flag was
/// also the banned operator tier of `principles.md` § One configuration surface
/// (a nest's own public URL is nobody's *choice*), and was deleted the way
/// `--push-relay-url` was.
#[cfg(feature = "bluesky")]
#[derive(Default)]
pub struct BlueskyState {
    /// What a client is built FROM: where the ES256 keypair lives (stable on
    /// disk, `load_or_generate`, so only the `client_id` moves when the domain
    /// does) and the database the OAuth state/session backend and the poller's
    /// store ride on. `None` on a deployment artifact that opts out of the
    /// bridge entirely (the embedded desktop nest).
    seed: Option<BlueskySeed>,
    /// The derivation for the identity domain it was built under. Read and
    /// written only by [`Self::resolve`]; a `std::sync::Mutex` (never held
    /// across an `await` — the whole build is synchronous) so two concurrent
    /// callers cannot each build a client and leak the loser's poller task.
    derived: std::sync::Mutex<Option<Arc<DerivedBluesky>>>,
}

/// The ingredients [`BlueskyState::resolve`] builds from.
#[cfg(feature = "bluesky")]
pub struct BlueskySeed {
    keypair_path: std::path::PathBuf,
    db: Arc<CacheDb>,
}

/// One derivation: the OAuth client for `public_url`.
#[cfg(feature = "bluesky")]
pub struct DerivedBluesky {
    /// The URL this client's `client_id` is under — the cache key. A derived
    /// URL that no longer equals this one means the deployment's identity
    /// domain moved and the client must be rebuilt.
    pub public_url: String,
    pub oauth: Arc<fauna_bridge_atproto::oauth::BlueskyOAuthClient>,
}

/// Build the deployment's Bluesky OAuth client. Production: the real network.
/// Under `test-hooks` **only**, and only when `FAUNA_TEST_ATPROTO_FAR_END`
/// names a loopback origin, every request the client makes (handle and DID
/// resolution, OAuth metadata, PAR, token, the PDS's XRPC) goes to that origin
/// instead — the e2e harness's consume-side fake
/// (`tests/e2e-unified/helpers/atproto_fakes.py::FakeAtprotoFarEnd`), the
/// running-nest twin of the in-process `FakeHttpResponder` the tier_3s use.
/// Shaped after `activitypub::outbound`'s `FAUNA_TEST_AP_*` hooks: compiled
/// out of every production binary, and loopback-only even when compiled in.
#[cfg(feature = "bluesky")]
fn build_bluesky_oauth_client(
    config: fauna_bridge_atproto::oauth::BlueskyOAuthConfig,
) -> anyhow::Result<fauna_bridge_atproto::oauth::BlueskyOAuthClient> {
    #[cfg(feature = "test-hooks")]
    if let Some(origin) = std::env::var("FAUNA_TEST_ATPROTO_FAR_END")
        .ok()
        .filter(|o| !o.is_empty())
    {
        let url = url::Url::parse(&origin)?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d == "localhost",
            None => false,
        };
        anyhow::ensure!(
            loopback,
            "FAUNA_TEST_ATPROTO_FAR_END must name a loopback origin, got {origin}"
        );
        tracing::warn!("Bluesky OAuth client routed to the test far end at {origin}");
        return fauna_bridge_atproto::oauth::build_oauth_client_with_origin_override(
            config, &origin,
        );
    }
    fauna_bridge_atproto::oauth::build_oauth_client(config)
}

#[cfg(feature = "bluesky")]
impl BlueskyState {
    /// The bridge as the deployment artifact wires it: a keypair beside the
    /// database, and no choice for anyone to make.
    pub fn new(keypair_path: std::path::PathBuf, db: Arc<CacheDb>) -> Self {
        Self {
            seed: Some(BlueskySeed { keypair_path, db }),
            derived: std::sync::Mutex::new(None),
        }
    }

    /// The derivation for `handle_domain`, building (and caching) it if the
    /// cached one is for a different URL. `None` when the deployment has no
    /// public identity domain, when the artifact seeded no bridge, or when the
    /// build itself failed — all three are "the bridge is unavailable", and
    /// [`crate::bluesky::bridge_provider::BlueskyProvider`] turns that into an
    /// `available: false` row carrying a reason rather than a silent absence.
    ///
    /// Takes the domain rather than an `AppState` so the whole rule is
    /// exercisable without one (see this module's tests).
    pub fn resolve(&self, handle_domain: Option<&str>) -> Option<Arc<DerivedBluesky>> {
        let public_url = crate::bluesky::oauth_public_url(handle_domain)?;

        // The cache is consulted BEFORE the seed, not after: a test presets a
        // client without one (`Self::preset`), and a seed-first check would
        // make that preset unreachable while still reporting "unavailable".
        let mut slot = self.derived.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = slot.as_ref()
            && current.public_url == public_url
        {
            return Some(Arc::clone(current));
        }
        let seed = self.seed.as_ref()?;

        let keypair =
            match fauna_bridge_atproto::keypair::Es256Keypair::load_or_generate(&seed.keypair_path)
            {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!(
                        "Bluesky OAuth keypair unavailable at {}: {e}",
                        seed.keypair_path.display()
                    );
                    return None;
                }
            };
        let config = fauna_bridge_atproto::oauth::BlueskyOAuthConfig {
            public_url: public_url.clone(),
            keypair,
            backend: Arc::new(crate::bluesky::storage_backend::CacheDbBackend::new(
                Arc::clone(&seed.db),
            )),
        };
        let client = match build_bluesky_oauth_client(config) {
            Ok(client) => Arc::new(client),
            Err(e) => {
                tracing::warn!("Bluesky OAuth client failed for {public_url}: {e}");
                return None;
            }
        };
        tracing::info!("Bluesky OAuth client derived for {public_url}");

        let built = Arc::new(DerivedBluesky {
            public_url,
            oauth: client,
        });
        *slot = Some(Arc::clone(&built));
        Some(built)
    }

    /// Seed the cache with an already-built client for `public_url` — the
    /// **test** seam that replaces the old `state.bluesky.oauth = Some(..)`
    /// assignment. A test pairs it with an `identity_domain` that derives the
    /// same URL, so the production rule in [`Self::resolve`] still runs and the
    /// preset is what it finds rather than a branch that bypasses it.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn preset(
        &self,
        public_url: impl Into<String>,
        oauth: Arc<fauna_bridge_atproto::oauth::BlueskyOAuthClient>,
    ) {
        let built = Arc::new(DerivedBluesky {
            public_url: public_url.into(),
            oauth,
        });
        let mut slot = self.derived.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(built);
    }
}

// ---------------------------------------------------------------------------
// NostrState
// ---------------------------------------------------------------------------

/// Nostr bridge: relay broadcast and sync outbound channels.
#[cfg(feature = "nostr")]
pub struct NostrState {
    pub relay_tx: tokio::sync::broadcast::Sender<crate::nostr::relay_endpoint::NostrRelayEvent>,
    pub sync_tx: tokio::sync::mpsc::Sender<crate::nostr::sync_worker::OutboundEvent>,
    /// Best-effort wake nudge to the sync worker's NIP-46 bunker-drain
    /// reconciler. `fauna.nostr.bunker.create_invite` sends one per mint so a
    /// fresh signer's `#p` filter goes live on the paired public box
    /// immediately (the app's first request is ephemeral and never replayed —
    /// waiting for the worker's next 60s tick would drop it). Send with
    /// `try_send` and ignore the result: a full channel means a wake is
    /// already pending, a closed one means no worker is running.
    pub bunker_wake_tx: tokio::sync::mpsc::Sender<()>,
    /// Per-recipient rate limiter for the unauthenticated gift-wrap (kind 1059)
    /// inbox (slice B). `None` disables the limit (test states that never
    /// exercise the inbox); production always sets it.
    pub gift_wrap_limiter: Option<Arc<crate::nostr::relay_endpoint::GiftWrapLimiter>>,
    /// Per-signer rate limiter for the unauthenticated kind-24133 bunker
    /// carve-out. Same `None`-disables contract as `gift_wrap_limiter`.
    pub bunker_limiter: Option<Arc<crate::nostr::relay_endpoint::BunkerLimiter>>,
    /// Outstanding proof-of-possession challenges for `nip07` links, keyed by
    /// actor hex — one per actor, the newest superseding, each dead after
    /// [`crate::nostr::bridge_provider::LINK_CHALLENGE_TTL_SECS`]. In memory
    /// on purpose: a challenge is a few minutes of state between two calls
    /// from one app; a restart forgets it and the app simply asks again.
    pub link_challenges: std::sync::Mutex<
        std::collections::HashMap<String, crate::nostr::bridge_provider::LinkChallenge>,
    >,
    /// The SSRF seat for every outbound relay dial this nest makes:
    /// the sync worker takes it at construction, the interaction fan-out and
    /// the `remote`-link handshake read it here. Computed once by
    /// [`crate::nostr::relays::relay_dial_policy`] — `PublicOnly` in
    /// production and in `for_test` alike, so a test behaves like the shipping
    /// nest unless it says otherwise; an in-process test whose bunker or peer
    /// relay sits on loopback sets `PublicOrLoopback` on its own state (a
    /// dependency build carries neither `test-hooks` nor the e2e env).
    pub relay_dial_policy: fauna_bridge_nostr::relay_client::RelayDialPolicy,
}

#[cfg(feature = "nostr")]
impl Default for NostrState {
    fn default() -> Self {
        let (relay_tx, _) = tokio::sync::broadcast::channel(256);
        let (sync_tx, _) = tokio::sync::mpsc::channel(256);
        let (bunker_wake_tx, _) = tokio::sync::mpsc::channel(1);
        Self {
            relay_tx,
            sync_tx,
            bunker_wake_tx,
            gift_wrap_limiter: Some(Arc::new(
                crate::nostr::relay_endpoint::new_gift_wrap_limiter(),
            )),
            bunker_limiter: Some(Arc::new(crate::nostr::relay_endpoint::new_bunker_limiter())),
            link_challenges: Default::default(),
            relay_dial_policy: crate::nostr::relays::relay_dial_policy(),
        }
    }
}

// ---------------------------------------------------------------------------
// ActivityPubState
// ---------------------------------------------------------------------------

/// ActivityPub bridge: the delivery nudge and inbox rate limiter.
///
/// **There is deliberately no `domain` field.** Every AP surface resolves the
/// serving domain *at request time* via
/// [`AppState::handle_domain`](crate::routes::AppState::handle_domain) /
/// [`handle_domain_if_set`](crate::routes::AppState::handle_domain_if_set), the
/// same claim-refreshed `identity_domain` cache mail / discovery / the TLS apex
/// read. A boot-time snapshot of `config.nest.domain` is exactly the split-brain
/// `domains-and-tls-bootstrap.md` § *Claim: the handle domain IS the deployment's
/// identity domain* forbids: a provisioned box boots domainless, so the snapshot
/// was `None` forever and WebFinger/actor 404'd `"ActivityPub not configured"` on
/// every provisioned+claimed nest until a restart baked the domain into
/// `nest.toml`. Caught live by `test_activitypub_federation_live.py` (2026-07-23).
#[cfg(feature = "activitypub")]
pub struct ActivityPubState {
    /// Wakes the delivery worker to drain `ap_delivery_queue` now rather than
    /// at its next 30s poll. Every producer enqueues **durably** first and then
    /// nudges, so a missed nudge costs latency, never the activity
    /// (`activitypub.md` § Architecture → Outbound delivery). `notify_one`
    /// stores a permit when the worker is busy, so nudges coalesce instead of
    /// queueing up.
    pub delivery_nudge: Arc<tokio::sync::Notify>,
    pub inbox_limiter: Option<
        Arc<
            governor::RateLimiter<
                String,
                governor::state::keyed::DashMapStateStore<String>,
                governor::clock::DefaultClock,
            >,
        >,
    >,
}

#[cfg(feature = "activitypub")]
impl Default for ActivityPubState {
    fn default() -> Self {
        Self {
            delivery_nudge: Arc::new(tokio::sync::Notify::new()),
            inbox_limiter: None,
        }
    }
}

// ---------------------------------------------------------------------------
// AppState::for_test()
// ---------------------------------------------------------------------------

/// A stable test nest identity, **deterministic per `basename`**.
///
/// Production derives `nest_identity` from the reconciled deployment seed
/// (`NestIdentity::from_seed`, single-identity unification). A `for_test`
/// `AppState` has no reconciled deployment key (`nest_signing_key: None`), so it
/// just needs *some* stable identity. We derive a 32-byte seed from the basename
/// (BLAKE3) and build the identity from it — pure, no file I/O, so there is no
/// file race (the prior `load_or_generate` raced on `<basename>_<pid>.key` when
/// `cargo test`'s threads shared a process) and the same basename always yields
/// the same identity within and across runs.
fn cached_test_nest_identity(basename: &str) -> std::sync::Arc<crate::nest_identity::NestIdentity> {
    let seed: [u8; 32] = *blake3::hash(basename.as_bytes()).as_bytes();
    std::sync::Arc::new(crate::nest_identity::NestIdentity::from_seed(&seed))
}

/// The domain the web-content host resolver and per-subdomain cert loop key off
/// — `registration.handle_domain`, else the node `domain`, else the **empty**
/// string. This is the canonical resolved handle domain
/// ([`crate::routes::AppState::handle_domain`]) minus its `"localhost"` final
/// fallback: an empty string means "no web serving domain", so a domainless
/// localhost/IP box never strips a `.localhost` subdomain suffix nor issues a
/// `<handle>.localhost` HTTP-01 cert — it keeps the exact apex catch-all
/// behavior the shipped apex relies on. Keying off the handle domain (not the
/// raw `node.domain`) makes a `--handle-domain`-only deployment (empty
/// `node.domain`, the mail-fixture / production-mirror shape) serve subdomains.
/// `web-content-hosting.md` § Implementation status today (subdomain "Design
/// note (follow-on)"). Used by both `start_server` (which builds the
/// `HostResolver` before the `AppState` exists) and `main.rs`'s
/// `WebCertConfig.nest_domain`, keyed off the same two fields at both sites.
pub fn web_serving_domain(handle_domain: Option<&str>, node_domain: Option<&str>) -> String {
    handle_domain
        .or(node_domain)
        .map(str::to_string)
        .unwrap_or_default()
}

impl crate::routes::AppState {
    /// The nest's handle domain — the `identity_domain` cache (a projection of the
    /// primary `mail_domains` row, which IS the identity), else
    /// `registration.handle_domain`, else the node config `domain`, else
    /// `"localhost"`. This is the deployment's identity domain ("the handle IS the
    /// email"): the domain registration / discovery hand out and, on a single-box
    /// deploy, the domain mail rides on. A domainless box boots with
    /// `identity_domain == None` and learns its domain at claim (there is no
    /// `FAUNA_DOMAIN`; `--domain` is only a pre-claim seed). Canonical single
    /// definition — the registration (`discovery_core`) and account (`account_core`)
    /// handlers call this rather than each keeping a private copy.
    pub fn handle_domain(&self) -> String {
        self.handle_domain_if_set()
            .unwrap_or_else(|| "localhost".to_string())
    }

    /// [`handle_domain`](Self::handle_domain) without the `"localhost"`
    /// placeholder — `None` on a domainless box. For reports that must not
    /// present the placeholder as a real domain (`/internal/router-status`,
    /// which fauna-router projects into its public NodeInfo `handleDomain`).
    pub fn handle_domain_if_set(&self) -> Option<String> {
        self.identity_domain
            .load()
            .as_deref()
            .map(|s| s.to_string())
            // seed-read-ok(accessor): THE canonical fallback chain, and the only
            // production read of either seed there may be. The live
            // `identity_domain` cache is tried first, so these two rungs are
            // reachable only on a box that has never been claimed. Every other
            // site in the tree calls this instead of re-spelling the chain —
            // which is exactly what `nest_info_core` did not do until 2026-09-02.
            .or_else(|| self.auth.registration.handle_domain.clone())
            .or_else(|| self.config.nest.domain.clone())
    }

    /// Whether this deployment is **publicly reachable** — its identity domain
    /// is a public DNS name rather than `localhost` / a LAN address / nothing.
    ///
    /// The publicness test is the uniform one
    /// (`resolve_handle_domain(d).is_public_dns_name`, the same predicate
    /// `refuse_plain_http_for_public_domain` in `lib.rs` and the enable-email
    /// default use), so "public" means one thing across the nest. Held here
    /// because two custody-hosting call sites need it — the register door and
    /// the pump — and the idiom must not be spelled twice.
    ///
    /// ⚠ This is **not** `tls_enabled`. TLS terminates in the `fauna-sni-router`
    /// on a fronted deployment, so `tls_enabled` is false on plenty of real
    /// public nests; reading it as "is this box exposed" gets the answer exactly
    /// backwards where it matters most.
    ///
    /// ⚠ **Stated residual, not coverage:** a **domainless** box answers `false`
    /// and so keeps the local posture, including on a bare public IP. That is
    /// deliberate consistency with the uniform test above (which
    /// `refuse_plain_http_for_public_domain` shares — it too only fires on a
    /// configured public domain), and it is what keeps every bare-IP dev/e2e
    /// deployment working. If a bare-IP *public* deployment ever becomes a
    /// supported shape, **this predicate is the place that must be revisited** —
    /// finding exists because an unstated inherited residual went unnoticed,
    /// so this one is written down with its trigger.
    pub fn is_public_deployment(&self) -> bool {
        self.handle_domain_if_set().is_some_and(|d| {
            fauna_provisioning::probe::resolve_handle_domain(&d).is_public_dns_name
        })
    }

    /// The Bluesky OAuth client for this deployment's **current** identity
    /// domain, or `None` when it has no public one.
    ///
    /// THE reader of `state.bluesky` — the provider's availability, the link
    /// flow, the callback and the client-metadata document all come through
    /// here, so none of them can drift from the derivation
    /// ([`crate::bluesky::oauth_public_url`], and
    /// [`BlueskyState::resolve`] for the cache). Cheap on the hit path
    /// (a string compare under an uncontended mutex); a miss builds once per
    /// identity-domain change.
    #[cfg(feature = "bluesky")]
    pub fn bluesky_oauth(&self) -> Option<Arc<fauna_bridge_atproto::oauth::BlueskyOAuthClient>> {
        self.bluesky
            .resolve(self.handle_domain_if_set().as_deref())
            .map(|derived| Arc::clone(&derived.oauth))
    }

    /// The domain the web `HostResolver` treats as its apex — the client-set
    /// `identity_domain` (set at claim), else `registration.handle_domain`, else
    /// the node config `domain`, else **empty** (a domainless localhost/IP box has
    /// no apex/subdomain web routing — unlike [`handle_domain`](Self::handle_domain)'s
    /// `"localhost"` fallback). Used to reject a custom web domain that would
    /// collide with the nest's own apex / reserved hosts at registration.
    /// NB: a domain claimed *after* boot drives web-content/subdomain routing
    /// immediately, no restart — `identity_domain_core::apply_primary_identity`
    /// re-points the live `HostResolver` (`set_nest_domain`) alongside the
    /// `identity_domain` swap (`domains-and-tls-bootstrap.md` § Implementation
    /// status, the live web-apex re-point).
    pub fn web_serving_domain(&self) -> String {
        if let Some(d) = self.identity_domain.load().as_deref() {
            return d.to_string();
        }
        // seed-read-ok(accessor): the web-apex twin of `handle_domain_if_set`
        // — same identity-cache-first order (the early return above), differing
        // only in the final fallback: empty rather than `"localhost"`, so a
        // domainless box has no apex rather than a fake one.
        web_serving_domain(
            self.auth.registration.handle_domain.as_deref(),
            self.config.nest.domain.as_deref(),
        )
    }

    /// Test-friendly constructor. Creates an in-memory DB-backed AppState with
    /// sensible defaults. Override fields with struct update syntax.
    pub fn for_test(db: std::sync::Arc<crate::db::CacheDb>) -> Self {
        Self::for_test_with_node_mode(db, crate::config::NodeMode::Public)
    }

    /// The identity this box proves over `fauna.auth.nest_handshake` and
    /// requires in every nest-bound signature — the `nest_id` a login
    /// (`auth_core`) or a NAT-mode commit (`nat_mode_core`) must name
    /// (`login.md` § Binding the nest). The same key
    /// `auth_handlers::build_identity_binding` signs with: the reconciled
    /// deployment `nest_signing_key` when present, else the `nest_identity`
    /// key — in production one and the same key (`box-recovery.md` § Single-
    /// identity unification), the fallback only giving a keyless test nest an
    /// identity at all. One definition, so what the box *proves* and what it
    /// *requires* can never be two different keys.
    pub fn bound_identity(&self) -> [u8; 32] {
        self.nest_signing_key
            .as_ref()
            .map(|k| k.verifying_key().to_bytes())
            .unwrap_or_else(|| self.nest_identity.public_key_bytes())
    }

    /// Like [`for_test`](Self::for_test) but with an explicit NAT axis, so
    /// tests can exercise the private-mode branches (e.g. the `whoami`
    /// `node_mode` reply that drives the MDA's LAN-only bind default).
    pub fn for_test_with_node_mode(
        db: std::sync::Arc<crate::db::CacheDb>,
        mode: crate::config::NodeMode,
    ) -> Self {
        // Per-call sequence for the segment-store tempdirs below. Process id
        // alone is shared by every test in the binary, so concurrent mail/cal
        // segment writers (e.g. restore_mail + ingest_inbound_mail) raced on one
        // FramedSegmentStore and append could fail under full-suite parallelism.
        // Combining pid with a per-`for_test` sequence makes each AppState's
        // dirs test-unique while still distinct across separate test binaries.
        static FOR_TEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let test_token = format!(
            "{}-{}",
            std::process::id(),
            FOR_TEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let email_state = EmailState::default();
        // Per-test acme dir (same `test_token` the segment dirs use): a fixed
        // `/tmp/test-acme` was shared by every test in the binary, so two
        // concurrent `store_acme_material` PEM writes raced on the same
        // `fullchain.pem` temp file (ENOENT on the rename). Unique-per-instance
        // removes that contention — store_acme_material is exercised by every
        // ACME-finish / self-signed-cert provisioning path.
        let acme_dir = std::env::temp_dir().join(format!("fauna-test-acme-{test_token}"));
        let storage = std::sync::Arc::new(crate::storage::SealedStorage::new(
            db.clone(),
            acme_dir.clone(),
        )) as crate::storage::SharedStorage;
        Self {
            db: db.clone(),
            serve_generation: tokio_util::sync::CancellationToken::new(),
            serve_tasks: tokio_util::task::TaskTracker::new(),
            serve_restart: std::sync::Arc::new(tokio::sync::Notify::new()),
            ws: std::sync::Arc::new(crate::ws::WsState::with_durable(db.clone())),
            bridge_push_registry: std::sync::Arc::new(
                crate::bridge_push_registry::BridgePushRegistry::new(),
            ),
            delegation_leases: std::sync::Arc::new(crate::delegation_registry::LeaseRegistry::new()),
            delegation_runner_wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            backup_pass_health: Default::default(),
            spam_baseline_runs: Default::default(),
            succession_hints: Default::default(),
            oauth_as: std::sync::Arc::new(crate::oauth_as_state::OAuthAsRuntime::production(
                fauna_core::data::Timestamp::now_secs_or_zero(),
            )),
            // No data directory: a test that installs a plugin overrides this
            // with a runner over its own tempdir.
            plugins: std::sync::Arc::new(crate::plugin_runner::PluginRunner::new(None)),
            oauth_limiter: std::sync::Arc::new(crate::oauth_as_rate_limit::EndpointLimiter::new()),
            rpc_router: std::sync::Arc::new(crate::rpc_router::RpcRouter::builder().build()),
            federation_router: std::sync::Arc::new(
                crate::federation_router::FederationRouter::builder().build(),
            ),
            federation_pool: std::sync::Arc::new(
                crate::federation_pool::FederationChannelPool::new(),
            ),
            config: std::sync::Arc::new(crate::config::NestConfig {
                nest: crate::config::NestSection {
                    mode,
                    listen: "127.0.0.1:0".into(),
                    // `registration_mode` stays at its `None` default ⇒ the safe
                    // `closed` seed. Tests drive the live `AppState.registration_mode`
                    // field (set to `Open` below), not this pre-claim seed.
                    ..Default::default()
                },
                bridges: None,
                submission: None,
                acme: None,
                email: None,
                update: Default::default(),
            }),
            nest_identity: cached_test_nest_identity("test_nest"),
            nest_signing_key: None,
            served_cert_spki: None,
            acme_retry_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
            relay_cert_changed: tokio::sync::watch::Sender::new(0),
            relay_channels: Default::default(),
            http_client: reqwest::Client::new(),
            tls_enabled: false,
            cors_origins: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(vec![])),
            web_app_origin: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                fauna_protocol::web_app_origin::WebAppOrigin::Bundled,
            )),
            identity_domain: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
            acme_dir,
            backup_service: None,
            payload_store: None,
            // Plan 2 T6: per-process mail SegmentManager (kind = "mail").
            // Tests get a per-`for_test` tempdir (pid + sequence) so neither
            // concurrent test *runs* nor concurrent tests within one binary
            // collide on `{data-dir}/__mail/{actor}/...`.
            mail_segments: std::sync::Arc::new(fauna_segment_store::SegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-mail-{test_token}")),
                "mail",
            )),
            // Plan 7 T3: per-process conv SegmentManager (kind = "conv").
            // Tests get a per-`for_test` tempdir (the same `test_token` the
            // mail/placement/acme dirs use — pid + sequence) so neither
            // concurrent test *runs* nor concurrent tests within one binary
            // collide on `{data-dir}/__conv/{channel_id}/...`. Bare `pid` alone
            // (the prior shape) was shared by every `for_test` in the binary.
            conv_segments: std::sync::Arc::new(fauna_segment_store::SegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-conv-{test_token}")),
                "conv",
            )),
            // Track C posts: per-process post SegmentManager (kind = "post").
            // Per-`for_test` tempdir (same `test_token`) so concurrent test
            // runs/tests in one binary don't collide on `__post/{author}/...`.
            post_segments: std::sync::Arc::new(fauna_segment_store::SegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-post-{test_token}")),
                "post",
            )),
            // S6.4: per-process calendar CONTENT SegmentManager (kind =
            // "calendar"). Per-`for_test` tempdir (same `test_token`) so
            // concurrent runs don't collide on `__calendar/{actor}/...`.
            cal_segments: std::sync::Arc::new(fauna_segment_store::SegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-calendar-{test_token}")),
                "calendar",
            )),
            // S6.5: per-process card CONTENT SegmentManager (kind = "card").
            card_segments: std::sync::Arc::new(fauna_segment_store::SegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-card-{test_token}")),
                "card",
            )),
            mail_placement: std::sync::Arc::new(crate::segments::MailPlacementSegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-mail-placement-{test_token}")),
            )),
            cal_placement: std::sync::Arc::new(crate::segments::CalPlacementSegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-cal-placement-{test_token}")),
            )),
            card_placement: std::sync::Arc::new(crate::segments::CardPlacementSegmentManager::new(
                std::env::temp_dir().join(format!("fauna-test-card-placement-{test_token}")),
            )),
            security_notifier: std::sync::Arc::new(crate::security_notify::SecurityNotifier::new(
                db.clone(),
                cached_test_nest_identity("test_nest_sn"),
            )),
            feed_event_tx: tokio::sync::mpsc::channel(100).0,
            exchange_transition_tx: tokio::sync::watch::channel(0).0,
            update_status: tokio::sync::watch::channel(None).1,
            auth: AuthState::default(),
            sync: SyncState::default(),
            email: email_state,
            bridge: BridgeState::default(),
            mls: MlsState::default(),
            #[cfg(feature = "bluesky")]
            bluesky: BlueskyState::default(),
            #[cfg(feature = "nostr")]
            nostr: NostrState::default(),
            #[cfg(feature = "activitypub")]
            activitypub: ActivityPubState::default(),
            web_content_service: None,
            host_resolver: None,
            web_serve_holder: None,
            push_service: None,
            services_json_path: std::env::temp_dir()
                .join(format!("test_services_{}.json", std::process::id())),
            custody_hosting_root: None,
            sidecar_tokens: std::collections::HashMap::new(),
            bridge_rate_limit: std::sync::Arc::new(crate::bridge_rate_limit::Limiter::new()),
            spam_train_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::bridge_rate_limit::SPAM_TRAIN_LIMITER_CONFIG,
                ),
            ),
            channel_commit_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::bridge_rate_limit::CHANNEL_COMMIT_LIMITER_CONFIG,
                ),
            ),
            records_door_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::bridge_rate_limit::RECORDS_DOOR_LIMITER_CONFIG,
                ),
            ),
            per_ip_conn_limit: fauna_conn_limit::PerIpConnLimit::new(
                fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            ),
            federation_rate_limit: std::sync::Arc::new(crate::bridge_rate_limit::Limiter::new()),
            handler_semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
                crate::dispatch_core::MAX_INFLIGHT_HANDLERS,
            )),
            anonymous_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::anonymous_rate_limit::default_config(),
                ),
            ),
            anonymous_rate_limit_shed: std::sync::Arc::new(fauna_conn_limit::ShedCounter::new()),
            failed_credential_throttle: std::sync::Arc::default(),
            claim_rate_limit: std::sync::Arc::new(crate::bridge_rate_limit::Limiter::with_config(
                crate::anonymous_rate_limit::claim_config(),
            )),
            global_claim_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::anonymous_rate_limit::global_claim_config(),
                ),
            ),
            claim_rate_limit_shed: std::sync::Arc::new(fauna_conn_limit::ShedCounter::new()),
            invite_verify_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::anonymous_rate_limit::invite_verify_config(),
                ),
            ),
            invite_verify_rate_limit_shed: std::sync::Arc::new(fauna_conn_limit::ShedCounter::new()),
            register_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::anonymous_rate_limit::register_config(),
                ),
            ),
            register_rate_limit_shed: std::sync::Arc::new(fauna_conn_limit::ShedCounter::new()),
            invite_request_rate_limit: std::sync::Arc::new(
                crate::bridge_rate_limit::Limiter::with_config(
                    crate::anonymous_rate_limit::invite_request_config(),
                ),
            ),
            invite_request_rate_limit_shed: std::sync::Arc::new(
                fauna_conn_limit::ShedCounter::new(),
            ),
            // Mirror `config.nest.mode` (the same `mode` arg): tests exercising
            // the private-axis branches set it via `for_test_with_node_mode` and
            // the runtime readers consult `node_mode`, not `config.nest.mode`.
            node_mode: std::sync::Arc::new(tokio::sync::RwLock::new(mode)),
            // Tier quotas OFF by default: the 200-odd `for_test` callers create
            // actors and push inbox items freely without seeding a tier row. A test
            // exercising the quota ladder sets the field explicitly. (Production:
            // the server passes `true`, the desktop nest `false`.) This is NOT a
            // registration gate — an unregistered actor is refused in every mode,
            // in tests exactly as in production.
            enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            subhandles: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            // Production's hard-coded default: the age-verification gate is off
            // (family-safety.md § The account age band D5 — works out of the
            // box). A test arming the knob sets the field explicitly.
            age_verification_required: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            // The posture mirrors PRODUCTION's default — `Closed`
            // (`DEFAULT_REGISTRATION_MODE`), not a permissive test-only one. A
            // fixture that admits strangers when the real default refuses them is
            // how a fail-open regression hides: `pre_identity_ws` pins exactly this
            // (an anonymous `fauna.account.register` on a default nest must come
            // back `registration_closed`). It also matches what the old
            // `RegistrationConfig::default()` did (`open = false`).
            //
            // A test that wants self-service registration sets the field explicitly
            // — see `conformance_register::state_in` / `registration_mode_api`.
            // Tests do not need it to create actors: they call `create_user*`
            // directly, which is the admin-admission path, not the ceremony.
            registration_mode: std::sync::Arc::new(tokio::sync::RwLock::new((
                fauna_protocol::node_policy::DEFAULT_REGISTRATION_MODE,
                None,
            ))),
            // No node-wide storage cap in tests (mirrors `config.nest.max_storage_bytes`
            // default `None`); a test needing a cap sets the field explicitly.
            max_storage_bytes: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            storage,
            // No override by default: the app relay's demand doors verify
            // against the real compiled-in registry, exactly as production.
            // A test opts in via `install_region_registry_for_test`.
            region_registry_override: None,
            #[cfg(feature = "test-hooks")]
            outbound_clock_override: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
            #[cfg(feature = "test-hooks")]
            rescore_worklist_serves: std::sync::Arc::new(Default::default()),
            #[cfg(feature = "test-hooks")]
            post_fanout_initiations: std::sync::Arc::new(Default::default()),
            #[cfg(feature = "test-hooks")]
            epoch_sealing_test_override: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(feature = "test-hooks")]
            custody_hosting_periodic_held: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            mta_sts_fetcher: std::sync::Arc::new(fauna_mail::outbound::mta_sts::NullMtaStsFetcher),
            // Null resolver in tests: `fauna.dns.verify_records` answers
            // "checking" (no network). Comparator/cache behaviour is covered by
            // unit tests in `dns_verifier.rs`; a real resolver is injected only
            // in production.
            dns_verifier: std::sync::Arc::new(crate::dns_verifier::DnsVerifier::null(
                std::sync::Arc::new(fauna_core::data::Timestamp::now_secs_or_zero),
            )),
            #[cfg(feature = "test-hooks")]
            mta_sts_override: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            tlsa_resolver: std::sync::Arc::new(fauna_mail::outbound::dane::NullTlsaResolver),
            mx_resolver: std::sync::Arc::new(fauna_mail::outbound::mx::NullMxRrsetResolver),
            #[cfg(feature = "test-hooks")]
            tlsa_override: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            starttls_prober: std::sync::Arc::new(crate::mail_deliverability::NullStarttlsProber),
            link_preview: crate::link_preview::LinkPreviewState::new(),
            #[cfg(feature = "test-hooks")]
            link_preview_override: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            #[cfg(feature = "test-hooks")]
            bridge_status_override: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            #[cfg(feature = "test-hooks")]
            atproto_identity_withheld: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            #[cfg(feature = "test-hooks")]
            channel_send_refusal: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            #[cfg(feature = "test-hooks")]
            rpc_hold: std::sync::Arc::new(crate::rpc_hold_test_hook::RpcHoldRegistry::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// **Class guard: no runtime handler may read a domain BOOT SEED.**
    ///
    /// A nest boots domainless and learns its domain at *claim*
    /// (`domains-and-tls-bootstrap.md` § *Claim: the handle domain IS the
    /// deployment's identity domain*). Two fields are pre-claim **seeds**, and
    /// on a real box neither ever moves: `config.nest.domain` (the config
    /// `[nest] domain` seed) and `auth.registration.handle_domain` (written by
    /// the `--handle-domain` CLI arg in `main.rs` and by *nothing else in the
    /// tree*). No deployment artifact passes either — `docker/s6/fauna-nest/run`
    /// execs `fauna-nest --config /data/nest.toml --blob-dir /data/blobs`, and
    /// § *Env contract* retired `FAUNA_DOMAIN`. So on every provisioned box both
    /// are `None` for life, and any request-path read of one is frozen at boot:
    /// it serves `localhost` / `unknown` forever, the split-brain § Claim
    /// forbids. The live value is the `identity_domain` cache, reached through
    /// [`AppState::handle_domain`] / [`AppState::handle_domain_if_set`].
    ///
    /// This is a *class* guard, not a site guard, because the class is what
    /// recurred — and it recurred a second time **because the first guard was
    /// written for one seed's spelling**:
    ///
    /// * `config.nest.domain`, 2026-07-23: the same one-line mistake shipped in
    ///   ActivityPub (18 sites — all of federation dead on a provisioned box,
    ///   caught only by a paid live run on real infra), nostr NIP-42 auth + the
    ///   bunker connect-string + NIP-05 + the NIP-65 relay advertisement, the
    ///   video HLS playlists, federated Welcome delivery, and the setup-status
    ///   report. Two hand `rg` sweeps enumerated it and **both missed eight
    ///   sites**, purely because rustfmt had wrapped the chain across lines.
    /// * `auth.registration.handle_domain`, 2026-09-02: `nest_info_core` gated
    ///   its whole `registration` block — and sourced the top-level `domain` —
    ///   on the seed. So `fauna.nest.info`, the surface a stranger reads
    ///   *before* they have an account to discover that self-service
    ///   registration is open and at which domain, answered `domain: "unknown"`
    ///   and `registration: null` on every real box however the admin had
    ///   claimed and configured it; the ActivityPub NodeInfo document read the
    ///   same seed and told every crawler `openRegistrations: false`.
    ///
    /// So the seeds live in a **table**: a third one joins by adding a row,
    /// not by hand-copying this test into a shape that sees one field.
    ///
    /// The suppressor is a **line-level marker**, not a file exemption — a
    /// deliberate strengthening of the 2026-07-23 shape, whose two file-level
    /// entries exempted the whole of `state.rs` (which holds four seed reads of
    /// its own) and the whole of `sidecar_channel.rs`. Within the 8 lines above
    /// a read write `// seed-read-ok(<class>): <reason>`. Classes in use:
    /// `accessor` (the canonical resolver's own fallback chain — the only
    /// production read of a seed there may be), `boot-binding` (a stable seal
    /// AAD label, where following the live domain would make pre-claim-sealed
    /// material undecryptable), `test`.
    ///
    /// ⚠ Whitespace is normalized across a 5-line window before matching, so a
    /// rustfmt-wrapped `state\n.config\n.node\n.domain` reads the same as the
    /// single-line form — the exact wrapping that hid eight sites from two hand
    /// sweeps. ⚠ `#[cfg(test)]` regions are **not** skipped, for the reason
    /// [`boot_worker_spawns_are_generation_scoped_or_marked`] states at length:
    /// a region detector that over-claims by one item silently exempts
    /// *production* reads inside the over-claim. Four `seed-read-ok(test)`
    /// markers is the honest price.
    ///
    /// ⚠ A guard whose needle matches nothing passes forever, so each seed
    /// asserts its own floor: rename a field without updating the table and
    /// this test fails rather than going quietly blind.
    ///
    /// Boot-scoped reads are untouched: they take the seed as a plain argument
    /// off `nest_config` / `RegistrationConfig` before any `AppState` exists
    /// (`lib.rs`'s `web_serving_domain` call, `main.rs`), and the needles key on
    /// the read being made *through* the state's own field path.
    #[test]
    fn no_runtime_handler_reads_a_domain_boot_seed() {
        /// Flatten a line window, so rustfmt wrapping cannot hide a chain.
        fn flat(lines: &[&str]) -> String {
            lines
                .iter()
                .flat_map(|l| l.split_whitespace())
                .collect::<Vec<_>>()
                .concat()
        }

        // Split so this guard's own source contains neither needle whole.
        let seeds: [(&str, &str); 2] = [
            (
                concat!(".config.nest.", "domain"),
                "the `[nest] domain` config seed",
            ),
            (
                concat!(".auth.registration.", "handle_domain"),
                "the `--handle-domain` CLI seed",
            ),
        ];
        // Likewise split, so the marker check cannot match its own source.
        let marker = concat!("seed-read-", "ok(");

        let mut violations: Vec<String> = Vec::new();
        let mut seen = [0usize; 2];
        for (rel, text) in nest_source_files() {
            let lines: Vec<&str> = text.lines().collect();
            for i in 0..lines.len() {
                // A `//` line is prose *about* a seed, never a read of one —
                // this guard's own doc comment included.
                if lines[i].trim_start().starts_with("//") {
                    continue;
                }
                let here = flat(&lines[i..lines.len().min(i + 5)]);
                let next = flat(&lines[(i + 1).min(lines.len())..lines.len().min(i + 6)]);
                for (s, (needle, what)) in seeds.iter().enumerate() {
                    // In the window opening HERE but not in the one opening on
                    // the next line ⇒ this line begins the chain. Without the
                    // second half every wrapped read reports 5 times.
                    if !here.contains(needle) || next.contains(needle) {
                        continue;
                    }
                    seen[s] += 1;
                    if lines[i.saturating_sub(8)..i]
                        .iter()
                        .any(|l| l.contains(marker))
                    {
                        continue;
                    }
                    violations.push(format!(
                        "{rel}:{} reads {what} — `{}`",
                        i + 1,
                        lines[i].trim()
                    ));
                }
            }
        }

        for (s, (_, what)) in seeds.iter().enumerate() {
            assert!(
                seen[s] > 0,
                "this guard's needle for {what} matches nothing anywhere under `src/` — the \
                 field was renamed or removed and the guard is now VACUOUS. Update the `seeds` \
                 table to the new spelling (or drop the row if the seed is gone)."
            );
        }
        violations.sort();
        assert!(
            violations.is_empty(),
            "unmarked boot-seed reads:\n  {}\n\
             A runtime read must go through `state.handle_domain()` / `handle_domain_if_set()` so \
             it follows the claim (`domains-and-tls-bootstrap.md` § Claim). If the read is the \
             canonical accessor itself, a stable binding label, or a test, put a \
             `// seed-read-ok(<class>): <reason>` line within the 8 lines above it.",
            violations.join("\n  ")
        );
    }

    /// Every `.rs` file under this crate's `src/`, as `(path-relative-to-src,
    /// contents)`. Shared by all three source guards in this module — the
    /// boot-seed guard above and the two generation-scope guards below — so
    /// they cover the same tree by construction: a file none of them can forget
    /// about, which is the whole point of walking rather than listing.
    /// Paths are `/`-normalised so a violation reads the same on Windows.
    ///
    /// Reading **source** rather than inspecting compiled code is what makes
    /// both guards **flavor-independent**: they see `src/nostr/`,
    /// `src/activitypub/` and `src/bluesky/` from a default-features run, in
    /// which none of those modules is even compiled. That is the same property
    /// the rotation re-key walk needed (it keys off `sqlite_master` rather than
    /// cargo features, for the same reason): a completeness check gated on the
    /// build flavor is silently incomplete in every *other* flavor, and the
    /// shipped flavors differ. Note the converse, which still bites: a
    /// *behavioural* pin over gated code only runs under its feature, so run
    /// those with `--features` explicitly — the source guards do not cover them.
    fn nest_source_files() -> Vec<(String, String)> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&src)
                    .unwrap_or(&path)
                    .to_str()
                    .expect("utf-8 path")
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("read source")));
            }
        }
        out.sort();
        out
    }

    /// **Class guard: every long-lived spawn in the nest's boot/serve graph is
    /// generation-scoped or explicitly classified.**
    ///
    /// The deployment-seed rotation adopts a committed rotation by tearing down
    /// the serving generation and re-entering `start_server`
    /// (`box-recovery.md` § Deployment-seed rotation → *Adoption by the running
    /// process*). That teardown is complete only if every long-lived task went
    /// through [`AppState::spawn_scoped`] / [`AppState::scope_handle`] — a task
    /// that bypasses the scope survives the teardown holding the SUPERSEDED
    /// key material, silently, with every other gate green.
    ///
    /// So: **anywhere under `src/`**, a bare spawn — `tokio::spawn(`,
    /// `tokio::task::spawn(`, a `use tokio::spawn` import,
    /// `Handle::current().spawn(`, or `thread::spawn(` — must carry a
    /// `// spawn-ok(<class>): <reason>` marker within the 8 lines above it.
    /// Classes in use: `returns-handle-for-scope` (the helper returns its
    /// handle and every boot caller adopts it), `server-handle` (returned to
    /// the serve loop, aborted first at teardown), `connection-scoped` (dies
    /// with its connection; **see the drain-reach rule below**),
    /// `request-scoped` (one request, bounded by a **named** mechanism),
    /// `process-lifetime` (deliberately shared across generations, holds no
    /// AppState), `cross-generation-messenger` (causes/outlives the teardown
    /// by design), `test`. An unmarked spawn is a defect, not a formality —
    /// classify it or scope it.
    ///
    /// ⚠ **This guard deliberately does NOT skip `#[cfg(test)]` regions**, and
    /// the reasoning is the point rather than the conclusion (the question was
    /// inherited from an earlier pass, which had paid 18 `spawn-ok(test)` markers to
    /// cover two files). Skipping them needs the guard to *decide where a test
    /// region begins and ends* — brace matching over source it does not parse.
    /// A region detector that over-claims by even one item silently exempts
    /// **production** spawns inside the over-claim, which is the silencing
    /// failure this guard exists to prevent; and a guard whose whole value is
    /// that it cannot be argued with must not grow a place for a defect to
    /// hide. Measured, the honest alternative is cheap: going tree-wide cost
    /// **10** extra `spawn-ok(test)` markers, against 27 production sites that
    /// needed real classification. A dumb grep that costs a one-line marker in
    /// a new test beats a clever parser that can go quietly blind.
    ///
    /// The only two suppressors are narrow by construction, and neither can
    /// hide a call: a `//` comment line, and a needle sitting **inside a string
    /// literal** (an odd number of `"` before it — an escaped quote errs toward
    /// a false red, never a false green). The string rule replaced an accident:
    /// this test's own assert message mentions the needle, and was passing only
    /// because an unrelated `spawn-ok(` happened to sit within its 8-line
    /// window.
    ///
    /// ⚠ **DRAIN-REACH, NOT TASK SHAPE, DECIDES `connection-scoped`** — the rule
    /// was filed against, and the one a tree-wide extension of this
    /// guard must not blunt. The class's original justification was *"WS conns
    /// 1001-drain at teardown, plain-HTTP keep-alives are bounded by hyper's
    /// idle timeout"*: **two** clauses, each naming a mechanism that actually
    /// reaches the task. A federation or sidecar WS satisfied **neither** — it is
    /// not a `state.ws` client (so the 1001-drain loop never counts it) and not
    /// plain HTTP (so no idle timeout bounds it) — yet it *looked* connection-
    /// scoped, because it dies with its connection and the peer decides when
    /// that is. So the question is never "does this task end with its
    /// connection"; it is **"does some teardown mechanism reach it, and which
    /// one"**. If you cannot name the mechanism, the task is not
    /// `connection-scoped` — it is unscoped, and belongs in `spawn_scoped` /
    /// `scope_handle`. (The federation + sidecar channels were moved there in
    /// the fix; this folder now covers them, so a revert to a bare
    /// `tokio::spawn` reds here.)
    #[test]
    fn boot_worker_spawns_are_generation_scoped_or_marked() {
        // Every spelling that produces a detached task. Split so the guard's
        // own source doesn't match its needles: `tokio::spawn(` (the
        // re-export), `tokio::task::spawn(` (the canonical path), a
        // `use tokio::spawn` import (enables a bare `spawn(` call the needle
        // below it can't see — flagging the import is the guard's proxy for
        // it), a `Handle::current().spawn(`, and `thread::spawn(` (a
        // blocking worker that also survives generation teardown).
        let needles: [&str; 5] = [
            concat!("tokio::", "spawn("),
            concat!("tokio::", "task::spawn("),
            concat!("use ", "tokio::spawn"),
            concat!("Handle::current().", "spawn("),
            concat!("thread::", "spawn("),
        ];
        let mut violations = Vec::new();
        let mut checked = 0usize;
        for (rel, text) in nest_source_files() {
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for needle in needles {
                    let Some(at) = line.find(needle) else {
                        continue;
                    };
                    // A quoted mention (this test's own assert message, a doc
                    // string) is prose, not a call. Odd quote count before
                    // the needle ⇒ inside a string literal.
                    if line[..at].matches('"').count() % 2 == 1 {
                        continue;
                    }
                    // `use tokio::spawn` must not false-match a longer
                    // import (`use tokio::spawn_blocking`, `spawn_local`):
                    // require a non-identifier character (or line end)
                    // right after. The parenthesized needles are already
                    // delimited and skip this check.
                    if !needle.ends_with('(') {
                        let after = &line[at + needle.len()..];
                        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                            continue;
                        }
                    }
                    if line.contains("spawn_scoped") || line.contains("scope_handle") {
                        continue;
                    }
                    checked += 1;
                    let window = lines[i.saturating_sub(8)..i].join("\n");
                    if !window.contains("spawn-ok(") {
                        violations.push(format!("{rel}:{}", i + 1));
                    }
                }
            }
        }
        // Beside-control: a walk that silently found nothing — a moved `src`,
        // a broken suppressor — would otherwise pass vacuously forever. The
        // floor is re-based with deliberate headroom below the measured
        // count, not pinned to it (a floor equal to today's count reds on
        // the very first legitimate `spawn_scoped` conversion, which is
        // churn you want): 60 leaves room for up to 18 sites (23%) to
        // convert before this guard reds, while staying far enough above
        // zero to still catch a walk that silently stopped finding anything.
        assert!(
            checked >= 60,
            "the tree walk found only {checked} bare spawn sites; it is not looking at \
             what it claims to look at (78 were classified as of 2026-08-21, when this \
             floor was last re-based)"
        );
        assert!(
            violations.is_empty(),
            "bare spawn without a `spawn-ok(<class>)` marker: {violations:?}\n\
             Route it through `AppState::spawn_scoped`/`scope_handle` so the deployment-seed \
             rotation's generation teardown reaches it, or classify it with a marker comment \
             (see this test's doc for the classes). An unscoped long-lived task keeps signing \
             with the superseded key after a rotation."
        );
    }

    /// **Class guard: the nest has exactly ONE Ed25519 verification shape —
    /// `fauna_core::identity::verify_detached`.** Importing `ed25519_dalek`'s
    /// `Verifier` trait is the necessary condition for reaching the permissive
    /// [`ed25519_dalek::VerifyingKey::verify`], which for a small-order public
    /// key is not a signature check at all (`identity.rs::verify_detached` owns
    /// the mechanism and the measured forgery ratios). So the import itself is
    /// the needle: no production nest source may name it, whether spelled
    /// `Verifier` or pulled in invisibly by a `use ed25519_dalek::*` glob.
    ///
    /// ⚠ **This guard exists because the completeness claim it replaces was
    /// wrong, twice.** Fixed the class in
    /// `bins/fauna-push-relay` and stopped. Swept the
    /// nest, first found "seven" sites, re-swept tree-wide, concluded **"Ten
    /// nest sites verified permissively … All ten now route through one
    /// primitive"** — and still left `storage_mode_core.rs`, `nat_mode_core.rs`
    /// and `share_routes.rs` permissive. Two deliberate sweeps by two sessions
    /// missed the same three files, and the claim "the nest has a single
    /// Ed25519 verification shape" then propagated into
    /// `verify_detached`'s doc comment, `security.md`, and a dated verdict as
    /// settled fact. **A hand-enumerated "all N sites" is not a witness; a walk
    /// is.** The lesson generalizes past this class: any remedy whose value is
    /// its *completeness* must land with a mechanical re-runnable census, or
    /// the next session inherits the gap as closed.
    ///
    /// Suppressors are narrow and mirror
    /// [`boot_worker_spawns_are_generation_scoped_or_marked`], for the same
    /// reason: a `//` comment line, a mention inside a string literal, and an
    /// explicit `verify-ok(<class>)` marker in the preceding 8 lines. The guard
    /// deliberately does NOT try to detect `#[cfg(test)]` regions — deciding
    /// where one ends means brace-matching source it does not parse, and an
    /// over-claim silently exempts production code. A test that genuinely needs
    /// to hand-roll a signature check pays a one-line `verify-ok(test)` marker.
    ///
    /// **Scope is the nest binary, deliberately.** The same permissive shape
    /// survives in `libs/` (`fauna-cbor/src/envelope.rs`,
    /// `fauna-client-core/src/nest_trust.rs`, `fauna-core/src/generation.rs`,
    /// `grant_event.rs`, `scoring.rs`, `account_entry_crypto.rs`,
    /// `fauna-client-sync/src/lib.rs`, `fauna-client-atproto/src/plc_chain.rs`
    /// and `tombstone.rs`, `fauna-client-dns/src/acme_pure.rs`, …). Those are
    /// **not** covered here because each needs its own reachability ruling
    /// first — several verify a key that is the identity being established
    /// rather than an authorization, where the permissive/strict distinction
    /// changes nothing. Widening this walk to `libs/` is a follow-up, and must follow
    /// those rulings, not precede them.
    /// **Every capability-grant decode that authorizes checks the whole
    /// window** — the mechanical census finding's remedy owes itself.
    ///
    /// `capability_grants` stores `epoch_end` and no `epoch_start`
    /// (`db/migrations.rs`), so every SQL-level "live grants" filter is half a
    /// window check wearing a whole one's name. The start bound therefore
    /// survives only where a decoder asks for it, and on 2026-08-16 exactly one
    /// of three consumers did: a custody grant post-dated by a month authorized
    /// pulls on the day it was minted
    /// (`conformance_custody_nest_door_client::a_post_dated_custody_row_must_not_admit_before_its_window_opens`).
    ///
    /// So the rule is structural, not remembered: a nest source that decodes a
    /// `GrantBlob` must also name `grant_window_is_open`. This is the census the
    /// sibling guard above exists for the same reason — a remedy whose value is
    /// its completeness ships with a re-runnable check, or the next reader
    /// inherits the gap as closed (`security.md`; `account-data-plane.md`
    /// § The custody grant + ceremony).
    ///
    /// A decode that genuinely does not authorize — reading a blob to extract
    /// storage columns at mint, to re-encode it, to list it back to its owner —
    /// pays a `window-ok(<why>)` marker comment in the preceding 8 lines, the
    /// same beside-control idiom.
    #[test]
    fn every_authorizing_grant_decode_checks_the_whole_window() {
        let decode = concat!("GrantBlob::from_", "canonical_bytes");
        let checker = concat!("grant_window_", "is_open");
        let mut violations = Vec::new();
        let mut decode_sites = 0usize;
        for (rel, text) in nest_source_files() {
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") || !line.contains(decode) {
                    continue;
                }
                decode_sites += 1;
                // The window check may sit either side of the decode within the
                // same block — the decode has to happen first to have a blob to
                // ask about, so look forward as well as back.
                let lo = i.saturating_sub(8);
                let hi = (i + 20).min(lines.len());
                let window = lines[lo..hi].join("\n");
                if !window.contains(checker) && !window.contains("window-ok(") {
                    violations.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        // Beside-control: a walk that found nothing would pass vacuously forever.
        assert!(
            decode_sites >= 5,
            "the tree walk found only {decode_sites} grant-blob decode sites in the nest; it is \
             not looking at what it claims to look at"
        );
        assert!(
            violations.is_empty(),
            "a capability-grant blob is decoded without the window being checked: {violations:?}\n\
             `fetch_capability_grants_for_holder` and every other storage filter can express \
             \"not expired\" and CANNOT express \"already started\" — there is no `epoch_start` \
             column. An authorizing decode must call \
             `fauna_mls::wrapped_blob::grant_window_is_open` (both bounds, fails closed). A \
             decode that does not authorize pays a `window-ok(<why>)` marker comment in the \
             preceding 8 lines."
        );
    }

    #[test]
    fn nest_has_one_ed25519_verification_shape() {
        // Split so the guard's own source lines don't match their needles.
        // `trait_needle` alone misses `use ed25519_dalek::*` — a glob pulls
        // `Verifier` into scope without ever spelling its name — so
        // `glob_needle` covers that spelling too.
        let trait_needle = concat!("Veri", "fier");
        let glob_needle = concat!("ed25519_dalek::", "*");
        let mut violations = Vec::new();
        let mut scanned_files = 0usize;
        for (rel, text) in nest_source_files() {
            // `Verifier` is only the ed25519 trait in a file that names the
            // crate; this also keeps `DnsVerifier` out of the population.
            if !text.contains("ed25519_dalek") {
                continue;
            }
            scanned_files += 1;
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for (needle, check_left_boundary) in [(trait_needle, true), (glob_needle, false)] {
                    let Some(at) = line.find(needle) else {
                        continue;
                    };
                    // Reject `DnsVerifier` and friends: a preceding identifier
                    // char means this is a different type, not the trait.
                    // Meaningless for the glob needle, which has no such twin.
                    if check_left_boundary
                        && line[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        continue;
                    }
                    // A quoted mention (an assert message, a doc string) is
                    // prose, not an import. Odd quote count before the
                    // needle ⇒ inside a string literal.
                    if line[..at].matches('"').count() % 2 == 1 {
                        continue;
                    }
                    let window = lines[i.saturating_sub(8)..i].join("\n");
                    if !window.contains("verify-ok(") {
                        violations.push(format!("{rel}:{}", i + 1));
                    }
                }
            }
        }
        // Beside-control: a walk that silently found nothing — a moved `src`, a
        // renamed dependency — would otherwise pass vacuously forever. Checked
        // alongside the sibling `boot_worker_spawns_are_generation_scoped_or_marked`
        // floor: this one had drifted the same way — pinned at 10
        // (the site count when the guard went tree-wide) against a measured
        // 25 today, 2.5× slack. Re-based with the same deliberate-headroom
        // shape: 19 leaves room for up to 6 sites (24%) to stop naming
        // `ed25519_dalek` (e.g. a call site fully migrating onto the
        // `fauna_core::identity` wrapper) before this guard reds.
        assert!(
            scanned_files >= 19,
            "the tree walk found only {scanned_files} nest sources naming ed25519_dalek; it is \
             not looking at what it claims to look at (25 were classified as of 2026-08-21, \
             when this floor was last re-based)"
        );
        assert!(
            violations.is_empty(),
            "ed25519_dalek's permissive `Verifier` trait is imported, directly or via a glob, \
             in the nest: {violations:?}\n\
             Route the verification through `fauna_core::identity::verify_detached` (small-order \
             key refusal + `verify_strict`), which is the nest's only sanctioned Ed25519 \
             verification shape. For a wire-supplied key the permissive `verify` is not a \
             binding check at all. A test that must hand-roll one pays a `verify-ok(test)` \
             marker comment in the preceding 8 lines."
        );
    }

    /// **Every peer/sidecar WS upgrade hands its connection future to the
    /// serving generation** — the half of the fix the marker ratchet
    /// above structurally CANNOT see.
    ///
    /// That guard greps for a bare `tokio::spawn(`. Reverting these routes to
    /// their pre-fix shape — `.on_upgrade(move |socket| serve_listener(state,
    /// socket))` — removes the spawn entirely, so the ratchet stays green while
    /// the connection task goes back to riding axum's detached per-connection
    /// task, which `teardown_serving_generation` never reaches. A guard that
    /// only inspects the spawns it can see is blind to the defect that consists
    /// of *not* spawning; this pins the shape instead.
    ///
    /// Scoping the drivers alone would not do: the drivers are separate tasks,
    /// and the connection future itself (`serve_listener` awaiting its serve
    /// loop) is its own long-lived `Arc<AppState>` holder.
    ///
    /// ⚠ **Row 89 widened this from a 2-file list to the whole tree, and the
    /// widening alone found three more of the same population** — the sync
    /// daemon channel, the nest-link worker channel, and the public `/nostr`
    /// relay endpoint, the last of which faces arbitrary third-party clients,
    /// so its duration is not merely peer-controlled but *stranger*-controlled.
    /// None of the three was a new regression: each had ridden axum's detached
    /// per-connection task since it was written, and the survey simply
    /// never looked outside the two files it had in hand. That is the argument
    /// for a walk over a list — the list is only ever as complete as the sweep
    /// that wrote it.
    ///
    /// The `EXEMPT` entries below each name the mechanism that actually reaches
    /// the connection, per the drain-reach rule. An entry without a named
    /// mechanism is not an exemption; it is an unscoped task.
    #[test]
    fn peer_channel_upgrades_are_generation_scoped() {
        /// `(path relative to src, the teardown mechanism that reaches it)`.
        const EXEMPT: &[(&str, &str)] = &[
            (
                "routes.rs",
                "the client WS-RPC endpoints (authenticated + anonymous) are \
                 `state.ws` connections: `begin_shutdown` flips the watch every \
                 `run_connection` send task holds, which drains in-flight replies \
                 and closes WS 1001. (The anonymous door is reached by that flag \
                 but NOT counted by the drain-wait's `connection_count`, which \
                 reads `subs` — so teardown may proceed while one is still \
                 closing. Bounded and self-terminating: it is finishing a close \
                 it has already been told to perform.)",
            ),
            (
                "principal_session.rs",
                "the third-party principal session is a `state.ws` connection too: \
                 its handler runs the same `run_connection` driver as the client \
                 endpoints, whose send task holds the watch `begin_shutdown` flips \
                 (drain, then WS 1001), and the session is registered through \
                 `subscribe_principal`, which the drain-wait's `connection_count` \
                 counts",
            ),
            (
                "degraded_serve.rs",
                "the degraded pre-boot server is not part of any serving \
                 generation and its handler holds no `AppState` — only a \
                 heartbeat policy",
            ),
        ];

        // Split so this guard's own source line doesn't match its needle.
        let needle = concat!("on_", "upgrade(");
        let mut violations = Vec::new();
        let mut checked = 0usize;
        for (rel, text) in nest_source_files() {
            if EXEMPT.iter().any(|(f, _)| *f == rel) {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let Some(at) = line.find(needle) else {
                    continue;
                };
                // Same two suppressors as the spawn ratchet: a quoted mention
                // is prose (this guard's own EXEMPT reasons say `on_upgrade`),
                // and `..._on_upgrade()` in a test's name is not a call site.
                if line[..at].matches('"').count() % 2 == 1 || line.contains("fn ") {
                    continue;
                }
                checked += 1;
                // The scope call sits inside the closure, within a few lines.
                let window = lines[i..(i + 10).min(lines.len())].join("\n");
                if !window.contains("spawn_scoped") {
                    violations.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        assert!(
            checked >= 4,
            "expected at least the 4 known non-exempt WS upgrades (federation, \
             sidecar relay, nest-link worker, nostr relay — the sync-device \
             upgrade left with the `/sync/ws` data plane); found {checked} — a \
             route was renamed or removed, so \
             this pin is not looking at what it claims to look at"
        );
        assert!(
            violations.is_empty(),
            "WS upgrade(s) not handed to the serving generation: {violations:?}\n\
             Wrap the connection future in `AppState::spawn_scoped` inside the \
             `on_upgrade` closure. These channels are neither `state.ws` clients \
             (no 1001-drain) nor plain HTTP (no idle timeout), so nothing else at \
             teardown reaches them — they would outlive the rotation holding the \
             superseded deployment key for a peer-controlled duration. If you \
             believe a route IS reached, add it to EXEMPT *naming the mechanism*."
        );
    }

    /// The generation scope's semantics, pinned: cancel kills a scoped task
    /// AND an adopted helper handle, and `teardown_serving_generation` waits
    /// for both. The beside-control (the tracker does NOT drain while the
    /// tasks are alive) is what keeps this from passing vacuously.
    #[tokio::test]
    async fn generation_teardown_kills_scoped_tasks_and_adopted_handles() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = crate::routes::AppState::for_test(db);

        let scoped_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = scoped_started.clone();
        state.spawn_scoped(async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        // spawn-ok(test)
        let adopted = tokio::spawn(std::future::pending::<()>());
        state.scope_handle(adopted);
        tokio::task::yield_now().await;
        assert!(
            scoped_started.load(std::sync::atomic::Ordering::SeqCst),
            "the scoped task should have started"
        );

        // Beside-control: with both tasks alive, the tracker must NOT drain.
        state.serve_tasks.close();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                state.serve_tasks.wait()
            )
            .await
            .is_err(),
            "tracker drained while both generation tasks were still alive — \
             the teardown assertion below would be vacuous"
        );

        // The teardown cancels the scope and waits: both tasks die.
        // spawn-ok(test)
        let server_stand_in = tokio::spawn(std::future::pending::<()>());
        state.teardown_serving_generation(server_stand_in).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), state.serve_tasks.wait())
                .await
                .is_ok(),
            "generation tasks survived the teardown"
        );
    }
}
