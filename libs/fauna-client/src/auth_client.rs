use std::sync::{Arc, RwLock};

use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{ApiError, BearerSource};

use crate::error::NestClientError;
use crate::ws_challenge_bearer::{
    LastAuthRefusal, LockedLatch, SupersededLatch, WsChallengeBearer,
};

/// Shared, swappable nest base URL. The data-plane WS (`ws_adapter`), the
/// residual HTTP content API, and the [`WsChallengeBearer`] token mint all read
/// the *current* value through this cell, so an SRV-reconnect serving-port
/// change ([`AuthClient::try_srv_recover`]) updates every consumer atomically.
/// `std::sync::RwLock` (not `tokio`) because reads are sync + short — clone the
/// string out, never hold the guard across an `.await`.
pub(crate) type SharedNestUrl = Arc<RwLock<String>>;

/// The two identity sources an [`AuthClient`] was assembled from disagree.
///
/// An `AuthClient` can be built from a **path** identity (the keypair — or, for
/// [`bearer_only`](AuthClient::bearer_only), the actor id — that becomes the
/// authenticated WS URL's actor) and, separately, a caller-supplied
/// [`BearerSource`] that mints the token presented on that same connection.
/// Nothing structurally ties the two together: linux and tui hand in a
/// `LaunchMachineBearer` whose secret came from persistence, while the keypair
/// came from wherever the launch flow resolved it, and a bug in *either* lookup
/// makes them name different actors.
///
/// The nest then refuses the WS upgrade with `403` on every attempt (the bearer
/// names one actor, the URL path another), the reconnect supervisor backs off,
/// and the app simply shows nothing — a failure that, before this check, was
/// visible **only in a nest log**. It cost several days of misdiagnosis in
/// 2026-08: a credential-store read-modify-write race restored a signed-out
/// actor's stored index, so the launch machine and the connection keypair
/// resolved different identities.
///
/// This is defence in depth, not the fix for that race — it turns a silent
/// blackout into a named, greppable client-side fact. Read it via
/// [`AuthClient::identity_mismatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BearerIdentityMismatch {
    /// The actor the authenticated WS URL will name (from the keypair /
    /// supplied actor id).
    pub path_actor_id: [u8; 32],
    /// The actor the bearer source mints for.
    pub bearer_actor_id: [u8; 32],
}

/// Compare the path identity against the bearer source's, when the source can
/// name one, and log loudly on disagreement.
///
/// A source that returns `None` from
/// [`BearerSource::bearer_actor_id`](fauna_nest_http::BearerSource::bearer_actor_id)
/// cannot say who it mints for (a `StaticBearer` in a test, a launch machine not
/// yet holding a secret) — that is "unknown", never "mismatched", so it passes.
fn check_bearer_identity(
    path_actor_id: [u8; 32],
    bearer: &Arc<dyn BearerSource>,
) -> Option<BearerIdentityMismatch> {
    let bearer_actor_id = bearer.bearer_actor_id()?;
    if bearer_actor_id == path_actor_id {
        return None;
    }
    tracing::error!(
        path_actor = %hex::encode(path_actor_id),
        bearer_actor = %hex::encode(bearer_actor_id),
        "identity mismatch assembling AuthClient: the bearer source mints for a \
         different actor than the connection keypair. The nest will refuse every \
         authenticated WS upgrade with 403 and this client will never connect."
    );
    Some(BearerIdentityMismatch {
        path_actor_id,
        bearer_actor_id,
    })
}

/// Shared authentication state for talking to a fauna nest.
///
/// Owns the actor keypair and the pooled HTTP client, and delegates the
/// bearer-token lifecycle to an [`Arc<dyn BearerSource>`] — by default a
/// [`WsChallengeBearer`], which mints the token over the pre-identity WS-RPC
/// silent challenge `fauna.auth.{challenge,verify}` (sign the nest's nonce, no
/// client timestamp; TTL-cache the reply on this device's clock with a 60 s
/// pre-expiry buffer, double-checked-lock refresh,
/// [`clear_token`](Self::clear_token) (→ `notify_401`) on a 4xx/401). The
/// source is pluggable so a client embedding `AuthClient` alongside another
/// bearer cache (e.g. the linux desktop's `LaunchMachineBearer`) can share one
/// cache rather than running two parallel token fetches.
///
/// The pooled `http` reqwest client below is retained for the still-HTTP
/// content API (`NestClient`'s `*/content/*` paths) and `POST /api/v1/register`
/// — token *acquisition* no longer rides HTTP, but those content surfaces have
/// their own (separately-tracked) WS-RPC migration.
pub struct AuthClient {
    http: reqwest::Client,
    nest_url: SharedNestUrl,
    /// Public actor id (always set). For the keypair-present constructors this
    /// is `keypair.actor_id().0`; for [`bearer_only`](Self::bearer_only) it is
    /// the owner's public actor id supplied directly. Drives `actor_id_hex()`
    /// (→ the authenticated WS URL path).
    actor_id: [u8; 32],
    /// The identity keypair, or `None` for a capability-scoped bearer-only
    /// helper (the Windows on-demand hydration host). The data plane never
    /// signs with the keypair — the authenticated WS presents the bearer in
    /// `Sec-WebSocket-Protocol`. It is consumed only by the
    /// `POST /api/v1/register` flow and the pairing / caldav secret-export
    /// paths, all of which are unreachable for a bearer-only helper.
    keypair: Option<ActorKeypair>,
    bearer: Arc<dyn BearerSource>,
    /// Set when the two identity sources this client was assembled from
    /// disagree — see [`BearerIdentityMismatch`]. `None` is the healthy case
    /// (and the only case a single-source constructor can produce).
    identity_mismatch: Option<BearerIdentityMismatch>,
    /// The superseded-refusal channel of the bearer mint, when this client owns
    /// a [`WsChallengeBearer`] (the FFI clients' path).
    ///
    /// `None` for [`with_bearer_source`](Self::with_bearer_source) callers: a
    /// caller-supplied bearer mints somewhere this client cannot see. linux and
    /// tui hand in a `LaunchMachineBearer`, which carries the refusal typed
    /// instead (`ApiError::Superseded`, mapped to the wire refusal by
    /// `map_api_err`), so their supervisors still stop on it by value.
    superseded: Option<SupersededLatch>,
    /// The locked-refusal channel of the bearer mint — same availability note
    /// as `superseded` above. linux and tui lose nothing by its absence: their
    /// `LaunchMachineBearer` mints through `fauna-launch-machine`, which parks
    /// a lock itself and signs nothing until `locked_until`.
    locked: Option<LockedLatch>,
    /// The last-auth-refusal channel of the bearer mint, when this client owns
    /// a [`WsChallengeBearer`] — same availability note as `superseded`
    /// above: `None` for a caller-supplied bearer, whose own refusal path
    /// this crate cannot see.
    last_refusal: Option<LastAuthRefusal>,
    /// A typed handle on the [`WsChallengeBearer`] behind `bearer`, when this
    /// client owns one — the e2e wrong-clock witness reads its held schedule
    /// ([`Self::held_bearer_for_test`]). `None` for a caller-supplied bearer,
    /// the same availability note as `superseded` above.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    challenge_bearer: Option<Arc<WsChallengeBearer>>,
}

/// The held session bearer's schedule, read without minting — the four UniFFI
/// apps' half of the wrong-clock refresh witness's `launch_token` observable
/// (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`; tui and linux read the launch
/// machine's instead, which holds their bearer). Both on the one client clock
/// ([`fauna_protocol::client_clock`]).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldBearerForTest {
    /// The held bearer's deadline, anchored at receipt on the client clock;
    /// `None` while no bearer is cached.
    pub expires_at_secs: Option<u64>,
    /// Every own session id still live — the client-side mint count.
    pub own_session_ids: Vec<String>,
}

/// The HTTP client every direct-Rust client hands to [`AuthClient`].
///
/// It authenticates a self-signed nest by **pinning the SPKI the WS handshake
/// graduated** for `nest_url`'s host (security.md § Cross-connection binding),
/// read live per-handshake; a public-CA nest takes the boring WebPKI path; an
/// un-graduated self-signed cert is refused (never accept-any). This replaces
/// the old `FAUNA_INSECURE_TLS=1 → accept any cert` escape hatch — the last
/// MITM-open surface (security.md § Transport trust). Pinning is keyed on the
/// *host* (port-independent), so an SRV-reconnect port swap keeps the same pin
/// valid.
///
/// A build failure is fatal rather than a silent fallback to
/// `reqwest::Client::new()`: an unpinned client would talk to a self-signed
/// nest with no SPKI check at all, which is precisely the hole this closes.
pub fn pinned_http_client(nest_url: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("fauna-client")
        .timeout(std::time::Duration::from_secs(30))
        .use_preconfigured_tls(fauna_anon_client::store_pinned_reqwest_tls(nest_url))
        .build()
        .expect("failed to build reqwest client")
}

impl AuthClient {
    /// Create an auth client backed by a [`WsChallengeBearer`] (the standalone
    /// shape: one keypair, one private cache). The bearer is minted over the
    /// WS-RPC silent challenge — no HTTP token-fetch. Does not
    /// authenticate yet.
    pub fn new(nest_url: String, keypair: ActorKeypair) -> Self {
        let nest_url = nest_url.trim_end_matches('/').to_string();
        // The residual HTTP content API + `POST /register` ride this reqwest
        // client.
        let http = pinned_http_client(&nest_url);
        // One shared, swappable URL cell behind BOTH the data WS and the bearer's
        // token mint, so an SRV-reconnect serving-port change (`try_srv_recover`)
        // reaches both atomically. Built here (not via `with_bearer_source`)
        // precisely so the bearer and this `AuthClient` hold the *same* cell.
        let url_cell: SharedNestUrl = Arc::new(RwLock::new(nest_url));
        let actor_id = keypair.actor_id().0;
        let ws_bearer = WsChallengeBearer::with_shared_url(
            Arc::clone(&url_cell),
            actor_id,
            keypair.signing_key().clone(),
        );
        let superseded = Some(ws_bearer.superseded_latch());
        let locked = Some(ws_bearer.locked_latch());
        let last_refusal = Some(ws_bearer.last_auth_refusal_latch());
        let ws_bearer = Arc::new(ws_bearer);
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        let challenge_bearer = Some(Arc::clone(&ws_bearer));
        let bearer: Arc<dyn BearerSource> = ws_bearer;
        Self {
            http,
            nest_url: url_cell,
            actor_id,
            keypair: Some(keypair),
            // Single-source by construction — the bearer above was just built
            // from this same `actor_id` — so the check is a tautology here. Run
            // it anyway: it costs one comparison and it is what keeps the
            // invariant true if this constructor is ever rewired.
            identity_mismatch: check_bearer_identity(actor_id, &bearer),
            bearer,
            superseded,
            locked,
            last_refusal,
            #[cfg(any(debug_assertions, feature = "e2e-agent"))]
            challenge_bearer,
        }
    }

    /// The successor-naming refusal this client's bearer mint has met, if any.
    ///
    /// `None` both when the identity is fine and when this client's bearer is
    /// caller-supplied (see the field's note) — callers must not read absence as
    /// proof the identity is live.
    pub fn superseded_refusal(&self) -> Option<fauna_protocol::RpcError> {
        self.superseded.as_ref().and_then(|l| l.get())
    }

    /// Whether the bearer mint has latched a supersession — the supervisor's
    /// terminal test.
    pub fn is_superseded(&self) -> bool {
        self.superseded.as_ref().is_some_and(|l| l.is_set())
    }

    /// The unlock time (Unix seconds) of the account lockout this client's
    /// bearer mint last met, lapsed or not — what a locked surface renders
    /// (`devices.md` § The locked state). `None` when no mint was refused
    /// locked, and when this client's bearer is caller-supplied.
    pub fn locked_until_secs(&self) -> Option<u64> {
        self.locked.as_ref().and_then(|l| l.locked_until_secs())
    }

    /// How long the account lockout the bearer mint met still stands — the
    /// reconnect supervisor's hold. `None` once it has lapsed.
    pub fn locked_hold(&self) -> Option<std::time::Duration> {
        self.locked.as_ref().and_then(|l| l.remaining())
    }

    /// The nest-side refusal this client's bearer mint most recently met, if
    /// its last attempt was refused rather than left unanswered.
    ///
    /// Read this right after an [`authenticate`](Self::authenticate) /
    /// [`ensure_auth`](Self::ensure_auth) failure to tell "the nest refused
    /// this" from "no nest answered" — both flatten to
    /// [`NestClientError::Auth`], which does not otherwise carry the
    /// distinction.
    ///
    /// `None` both when the last mint attempt succeeded or hit a transport
    /// fault, and when this client's bearer is caller-supplied (see
    /// `last_refusal`'s field note) — callers must not read absence as proof
    /// the last attempt succeeded.
    pub fn last_auth_refusal(&self) -> Option<fauna_protocol::RpcError> {
        self.last_refusal.as_ref().and_then(|l| l.get())
    }

    /// The held session bearer's schedule, read without minting and without
    /// waiting ([`HeldBearerForTest`]). `None` when this client's bearer is
    /// caller-supplied (its holder publishes its own) or a mint is in flight.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn held_bearer_for_test(&self) -> Option<HeldBearerForTest> {
        self.challenge_bearer.as_ref()?.held_bearer_for_test()
    }

    /// Stand in for a mint the nest refused, on a client that owns a
    /// [`WsChallengeBearer`] — the supervisor's stop-naming tests read it.
    #[cfg(test)]
    pub(crate) fn latch_last_auth_refusal_for_test(&self, refusal: fauna_protocol::RpcError) {
        self.last_refusal
            .as_ref()
            .expect("only a WsChallengeBearer client has the channel")
            .set(Some(refusal));
    }

    /// Create an auth client over a caller-supplied [`BearerSource`].
    /// Used by callers that already maintain a bearer cache they want
    /// the WS handshake to share — e.g. `apps/fauna-linux`'s
    /// `LaunchMachineBearer`, so one `/api/v1/auth/token` round trip +
    /// one TTL-pre-expiry refresh loop + one 4xx-reactive invalidation
    /// path backs both the HTTP `ReqwestNestContentApi` and the WS
    /// `NestClient`.
    ///
    /// `keypair` is kept on the `AuthClient` so the
    /// `POST /api/v1/register` flow (which signs from `auth::build_register_request`)
    /// still works without round-tripping through the bearer source.
    pub fn with_bearer_source(
        nest_url: String,
        keypair: ActorKeypair,
        bearer: Arc<dyn BearerSource>,
        http: reqwest::Client,
    ) -> Self {
        let actor_id = keypair.actor_id().0;
        Self {
            http,
            // Own cell — the caller's `bearer` mints at its own (independent)
            // URL, so SRV-reconnect recovery (`try_srv_recover`) is a no-op here
            // anyway: this constructor backs same-box / loopback bearers (linux's
            // `LaunchMachineBearer`), which are local targets with no public SRV
            // zone. A `https://host/` with a trailing slash is normalized so the
            // content-API `format!("{nest_url}/api/v1/…")` never double-slashes.
            nest_url: Arc::new(RwLock::new(nest_url.trim_end_matches('/').to_string())),
            actor_id,
            keypair: Some(keypair),
            // THE two-source constructor: `keypair` names the WS path actor and
            // `bearer` mints the token presented on that same connection, from
            // an identity this crate never sees. Disagreement here is a
            // guaranteed 403 loop — see `BearerIdentityMismatch`.
            identity_mismatch: check_bearer_identity(actor_id, &bearer),
            bearer,
            // Caller-supplied mint — see the field's note.
            superseded: None,
            locked: None,
            last_refusal: None,
            #[cfg(any(debug_assertions, feature = "e2e-agent"))]
            challenge_bearer: None,
        }
    }

    /// Bearer-only auth for a capability-scoped helper that holds a pre-minted
    /// bearer + the owner's public actor_id but NO identity keypair (the Windows
    /// on-demand hydration host). The data plane never signs with a keypair — the
    /// authenticated WS presents the bearer in `Sec-WebSocket-Protocol`. Register
    /// and pairing/caldav secret-export paths (which need the keypair) are
    /// unreachable for such a helper.
    pub fn bearer_only(
        nest_url: String,
        actor_id: [u8; 32],
        bearer: Arc<dyn BearerSource>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            http,
            // Own cell (see `with_bearer_source`): the bearer-only helper is the
            // same-box Windows hydration host — a local target, so SRV recovery
            // never fires.
            nest_url: Arc::new(RwLock::new(nest_url.trim_end_matches('/').to_string())),
            actor_id,
            keypair: None,
            // Two-source as well: the caller supplies the owner's public actor
            // id and, separately, a pre-minted bearer.
            identity_mismatch: check_bearer_identity(actor_id, &bearer),
            bearer,
            // Caller-supplied mint — see the field's note.
            superseded: None,
            locked: None,
            last_refusal: None,
            #[cfg(any(debug_assertions, feature = "e2e-agent"))]
            challenge_bearer: None,
        }
    }

    /// Share a caller-built bearer's superseded latch with this client, so
    /// [`is_superseded`](Self::is_superseded) — the reconnect supervisor's
    /// terminal test — reads it. For a caller-supplied bearer this crate built
    /// itself (`device_principal_nest_client`'s `WsDeviceHandshakeBearer`),
    /// where the field's "cannot see the mint" note does not hold.
    pub(crate) fn with_superseded_latch(mut self, latch: SupersededLatch) -> Self {
        self.superseded = Some(latch);
        self
    }

    /// Share a caller-built bearer's locked latch with this client, so
    /// [`locked_hold`](Self::locked_hold) — the reconnect supervisor's hold —
    /// reads it. Same caller as [`with_superseded_latch`](Self::with_superseded_latch).
    pub(crate) fn with_locked_latch(mut self, latch: LockedLatch) -> Self {
        self.locked = Some(latch);
        self
    }

    /// The latch [`with_locked_latch`](Self::with_locked_latch) shared, or the
    /// owned mint's, for a test standing in for the nest's refusal.
    #[cfg(test)]
    pub(crate) fn locked_latch_for_test(&self) -> LockedLatch {
        self.locked
            .clone()
            .expect("only a client that shares its mint's latch has one")
    }

    /// The latch [`with_superseded_latch`](Self::with_superseded_latch) shared,
    /// for a test standing in for the nest's refusal.
    #[cfg(test)]
    pub(crate) fn superseded_latch_for_test(&self) -> SupersededLatch {
        self.superseded
            .clone()
            .expect("only a client that shares its mint's latch has one")
    }

    /// The disagreement between this client's two identity sources, if there is
    /// one — see [`BearerIdentityMismatch`]. `None` on a healthy client, and on
    /// every client whose bearer source cannot name its actor.
    ///
    /// A caller that finds `Some` here should not bother connecting: every
    /// authenticated WS upgrade will be refused `403`.
    pub fn identity_mismatch(&self) -> Option<BearerIdentityMismatch> {
        self.identity_mismatch
    }

    /// Shared HTTP client (connection pool).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The shared bearer source backing this client's authenticated requests.
    /// Hand it (alongside [`http`](Self::http) + [`nest_url`](Self::nest_url)) to
    /// a second [`ReqwestNestContentApi`](fauna_nest_http::ReqwestNestContentApi)
    /// — e.g. the Media page's blob uploader — so that content API shares this
    /// client's *one* bearer cache (one token mint, one refresh loop, one
    /// 401-reactive path) instead of minting a parallel token. Clones the `Arc`,
    /// not the token.
    pub fn bearer(&self) -> Arc<dyn BearerSource> {
        Arc::clone(&self.bearer)
    }

    /// Nest base URL — the *current* value (the cell is swapped by
    /// [`try_srv_recover`](Self::try_srv_recover) on an SRV-reconnect serving-port
    /// change). Returns an owned `String` rather than a borrow because the value
    /// now lives behind a lock.
    pub fn nest_url(&self) -> String {
        self.nest_url
            .read()
            .expect("nest_url lock poisoned")
            .clone()
    }

    /// A [`ReqwestNestContentApi`](fauna_nest_http::ReqwestNestContentApi) built
    /// from this client's current [`nest_url`](Self::nest_url) +
    /// [`http`](Self::http) + [`bearer`](Self::bearer) — the exact triple every
    /// content-API caller otherwise hand-assembles, and doing so here means each
    /// call shares this client's one bearer cache rather than risking a
    /// caller that forgets to.
    pub fn content_api(&self) -> fauna_nest_http::ReqwestNestContentApi<Arc<dyn BearerSource>> {
        fauna_nest_http::ReqwestNestContentApi::new(
            self.nest_url(),
            self.http().clone(),
            self.bearer(),
        )
    }

    /// Atomically swap the live nest base URL (shared with the bearer's token
    /// mint + the HTTP content API). Returns `true` if it changed. Driven by
    /// [`try_srv_recover`](Self::try_srv_recover).
    pub fn set_nest_url(&self, new_url: String) -> bool {
        let mut guard = self.nest_url.write().expect("nest_url lock poisoned");
        if *guard == new_url {
            return false;
        }
        *guard = new_url;
        true
    }

    /// Re-resolve `_fauna._tcp.<host>` for the current nest URL and, if it now
    /// advertises a different client-facing port (an admin changed `serving_port`),
    /// atomically swap the live nest URL to it so the next reconnect **and** the
    /// next bearer mint target the new port. Returns `true` when the URL changed.
    ///
    /// A no-op (`false`) for a **local target** (loopback / IP / `.local` — no
    /// public SRV zone) or an unchanged port. This is the offline-client SRV
    /// self-heal of `docs/goal/architecture/nest/common.md` § Serving ports
    /// (path 2): the [`crate::reconnect::ClientChannel`]'s
    /// `SupervisedChannel::recover_endpoint` calls it on a transport
    /// connect-failure, so a serving-port change heals without a manual re-enter.
    /// Native uses hickory ([`fauna_core::resolve::resolve_node_url`], already a
    /// dep); the wasm web app uses the DoH twin in `fauna-rpc-wasm`. The pure
    /// host-gate + URL-rewrite decision is shared
    /// ([`fauna_core::resolve::srv_recovery_host`] / `srv_recovered_url`).
    pub async fn try_srv_recover(&self) -> bool {
        let current = self.nest_url();
        let Some(host) = fauna_core::resolve::srv_recovery_host(&current) else {
            return false;
        };
        // `Option`-returning (None ⇒ absent / errored ⇒ leave the URL untouched),
        // NOT the 443-on-absent `resolve_node_url`, so a transient DNS failure
        // never rewrites a working explicit-port URL to the default.
        let srv_port = fauna_core::resolve::fauna_srv_port(&host).await;
        match fauna_core::resolve::srv_recovered_url(&current, srv_port) {
            Some(new_url) => self.set_nest_url(new_url),
            None => false,
        }
    }

    /// The actor this client authenticates as — for a bearer-only helper, the
    /// owner's public actor id it was built with.
    pub fn actor_id(&self) -> [u8; 32] {
        self.actor_id
    }

    /// Hex-encoded actor ID.
    pub fn actor_id_hex(&self) -> String {
        hex::encode(self.actor_id)
    }

    /// Reference to the identity keypair, or `None` for a bearer-only helper
    /// (the Windows on-demand hydration host) that holds no keypair.
    pub fn keypair(&self) -> Option<&ActorKeypair> {
        self.keypair.as_ref()
    }

    /// Ensure a valid auth token is cached (signing one if needed). A still-
    /// valid cached token satisfies this without a round trip.
    pub async fn authenticate(&self) -> Result<(), NestClientError> {
        self.bearer.bearer().await.map(|_| ()).map_err(map_api_err)
    }

    /// Get a valid auth token, refreshing if it's expired or near expiry.
    /// Double-checked locking inside the underlying [`BearerSource`]
    /// prevents a thundering herd.
    pub async fn ensure_auth(&self) -> Result<String, NestClientError> {
        self.bearer.bearer().await.map_err(map_api_err)
    }

    /// Clear the cached auth token; the next [`ensure_auth`](Self::ensure_auth)
    /// re-mints over the silent challenge (when the source is a
    /// [`WsChallengeBearer`]; a [`fauna_nest_http::StaticBearer`] no-ops
    /// per the trait default).
    pub async fn clear_token(&self) {
        self.bearer.notify_401().await;
    }
}

fn map_api_err(e: ApiError) -> NestClientError {
    match e {
        ApiError::Status { code, message } => NestClientError::Auth(format!("({code}) {message}")),
        // Transport failures and a malformed handshake reply both surface
        // as "couldn't obtain a token" — `WsChallengeBearer` doesn't keep them
        // apart, and no caller branches on `Http` vs `Decode` here.
        ApiError::Transport(msg) => NestClientError::Auth(msg),
        // The one verdict that must NOT land in the "couldn't obtain a token"
        // bucket: a re-mint is exactly what must not be retried here
        // (`security.md` § Post-auth surfacing).
        ApiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        } => NestClientError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        },
        // The same for a held identity the nest stopped signing in: as the
        // wire refusal it is, the one form `WsChallengeBearer`'s path reaches
        // too (the supervisor reads it off `last_auth_refusal`), so both
        // bearer shapes end the supervisor as one typed `Refused`.
        ApiError::SignInRefused => NestClientError::Rpc(fauna_protocol::RpcError::not_registered()),
        // And the succession refusal, as the wire refusal `WsChallengeBearer`'s
        // latch carries — so a `LaunchMachineBearer` client (linux, tui) stops
        // its supervisor as the same typed `Refused(superseded)` the FFI apps
        // do, and `session_ending_verdict` names it.
        ApiError::Superseded { new_actor_id_hex } => {
            NestClientError::Rpc(superseded_refusal(&new_actor_id_hex))
        }
    }
}

/// The `fauna.auth.superseded` refusal naming `new_actor_id_hex` — the nest's
/// own shape when the hex is a well-formed 32-byte id (the machine took it from
/// that very reply), else the bare code: the verdict must survive a malformed
/// claim, which the import flow verifies against the chain anyway.
fn superseded_refusal(new_actor_id_hex: &str) -> fauna_protocol::RpcError {
    use fauna_protocol::RpcError;
    match hex::decode(new_actor_id_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    {
        Some(id) => RpcError::superseded(&id),
        None => RpcError::new(RpcError::CODE_SUPERSEDED, "error.auth.superseded"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    /// The `ApiError` → `NestClientError` seam, the third of the four that used
    /// to stringify the verdict. Everything here lands in `Auth(String)` by
    /// design ("couldn't obtain a token"), which is precisely the bucket a
    /// possible-MITM verdict must not join: a re-mint is what must NOT be
    /// retried (`security.md` § Post-auth surfacing).
    #[test]
    fn the_identity_verdict_does_not_join_the_couldnt_obtain_a_token_bucket() {
        let e = map_api_err(ApiError::NestIdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: None,
        });
        let NestClientError::NestIdentityChanged { host, seen_hex, .. } = e else {
            panic!("expected the verdict to survive, got {e:?}");
        };
        assert_eq!(host, "nest.example");
        assert_eq!(
            seen_hex, None,
            "the withdrawn case must stay distinguishable"
        );
    }

    /// The succession refusal survives the seam as the wire refusal the
    /// supervisor stops on and `session_ending_verdict` names — a malformed
    /// claim included.
    #[test]
    fn the_succession_refusal_does_not_join_the_couldnt_obtain_a_token_bucket() {
        let successor = [0xab_u8; 32];
        let e = map_api_err(ApiError::Superseded {
            new_actor_id_hex: hex::encode(successor),
        });
        let NestClientError::Rpc(r) = &e else {
            panic!("expected the verdict to survive, got {e:?}");
        };
        assert_eq!(r, &fauna_protocol::RpcError::superseded(&successor));
        assert_eq!(
            e.session_ending_verdict(),
            Some(crate::SessionEndingVerdict::Superseded)
        );
        let malformed = map_api_err(ApiError::Superseded {
            new_actor_id_hex: "not hex".into(),
        });
        assert_eq!(
            malformed.session_ending_verdict(),
            Some(crate::SessionEndingVerdict::Superseded)
        );
    }

    /// The mid-session sign-in refusal survives the seam as the wire refusal
    /// — the typed form the supervisor and the apps' routing read — never the
    /// "couldn't obtain a token" string.
    #[test]
    fn the_sign_in_refusal_does_not_join_the_couldnt_obtain_a_token_bucket() {
        let e = map_api_err(ApiError::SignInRefused);
        assert!(
            matches!(&e, NestClientError::Rpc(r) if r.is_not_registered()),
            "expected the verdict to survive, got {e:?}"
        );
    }

    /// …and the ordinary failures still flatten exactly as before.
    #[test]
    fn ordinary_api_failures_still_flatten_to_auth() {
        assert!(matches!(
            map_api_err(ApiError::Transport("boom".into())),
            NestClientError::Auth(_)
        ));
        assert!(matches!(
            map_api_err(ApiError::Status {
                code: 423,
                message: "locked".into()
            }),
            NestClientError::Auth(_)
        ));
    }

    #[test]
    fn auth_client_accessors() {
        let kp = ActorKeypair::generate();
        let expected_id = hex::encode(kp.actor_id().0);
        let client = AuthClient::new("https://test.fauna.social".into(), kp);
        assert_eq!(client.nest_url(), "https://test.fauna.social");
        assert_eq!(client.actor_id_hex(), expected_id);
    }

    #[tokio::test]
    async fn clear_token_is_callable_and_idempotent() {
        let kp = ActorKeypair::generate();
        let client = AuthClient::new("https://test.fauna.social".into(), kp);
        // No network: just confirms the delegation to
        // `WsChallengeBearer::notify_401` compiles and is a safe no-op when
        // nothing is cached.
        client.clear_token().await;
        client.clear_token().await;
    }

    #[test]
    fn with_auth_shares_client() {
        let kp = ActorKeypair::generate();
        let auth = Arc::new(AuthClient::new("https://test.fauna.social".into(), kp));
        let nest = crate::NestClient::with_auth(auth.clone());
        // Verify they share the same AuthClient
        assert_eq!(nest.nest_url(), "https://test.fauna.social");
        assert!(Arc::ptr_eq(nest.auth(), &auth));
    }

    #[test]
    fn new_creates_auth_internally() {
        let kp = ActorKeypair::generate();
        let expected_id = hex::encode(kp.actor_id().0);
        let nest = crate::NestClient::new("https://test.fauna.social".into(), kp);
        assert_eq!(nest.actor_id_hex(), expected_id);
        assert_eq!(nest.auth().nest_url(), "https://test.fauna.social");
    }

    #[test]
    fn set_nest_url_swaps_and_reports_change() {
        // The SRV-reconnect swap mechanism: `set_nest_url` updates the live URL
        // and reports whether it changed (idempotent on the same value).
        let kp = ActorKeypair::generate();
        let client = AuthClient::new("https://nest.example".into(), kp);
        assert_eq!(client.nest_url(), "https://nest.example");
        assert!(client.set_nest_url("https://nest.example:8443".into()));
        assert_eq!(client.nest_url(), "https://nest.example:8443");
        // Same value ⇒ no change.
        assert!(!client.set_nest_url("https://nest.example:8443".into()));
    }

    #[tokio::test]
    async fn try_srv_recover_is_noop_for_local_targets() {
        // A loopback nest has no public `_fauna._tcp` zone — recovery must short-
        // circuit without a lookup and leave the URL untouched (no network hit).
        let kp = ActorKeypair::generate();
        let client = AuthClient::new("https://127.0.0.1:3000".into(), kp);
        assert!(!client.try_srv_recover().await);
        assert_eq!(client.nest_url(), "https://127.0.0.1:3000");
    }

    #[test]
    fn new_trims_trailing_slash_on_shared_url() {
        // The shared cell is normalized so the content-API `format!` never
        // double-slashes, and the bearer (sharing the cell) mints at the same URL.
        let kp = ActorKeypair::generate();
        let client = AuthClient::new("https://nest.example/".into(), kp);
        assert_eq!(client.nest_url(), "https://nest.example");
    }
}
