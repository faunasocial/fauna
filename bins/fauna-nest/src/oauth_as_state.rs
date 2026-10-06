//! The authorization server's runtime memory — the nest-side half of TP5's
//! endpoints move.
//!
//! [`behavior/authorization-server.md`] § As built rules the shape of every
//! store here, and its rulings bind through the move: PAR requests and auth
//! codes are **memory with TTLs**, transient by design, so a restart mid-flow
//! fails that flow cleanly and the user retries. That is also the whole
//! client-state-recoverability answer for this surface (`nest/common.md`
//! § Client-state recoverability): nothing here is persisted, so there is no
//! interior state a crash can strand.
//!
//! **Why the nest and not a shared crate.** The *decisions* an authorization
//! server makes are already shared Rust — `fauna_bridge_atproto::{dpop,
//! oauth_par, client_assertion, oauth_metadata, permission_set}`, which this
//! nest now depends on unconditionally. What lives here is the other half: the
//! server's own runtime memory, which has exactly one host by construction. An
//! app never runs an authorization server, so there is no second consumer to
//! share with, and the Go bridge's copies die with its endpoints rather than
//! becoming a second caller.
//!
//! **The clock is injected, and there are no timers.** Every entry point takes
//! `now` as unix seconds, the shape `oauth_issuer_key` already uses for the
//! key set's retirement horizon and for the same reason: a timer needs a
//! scheduler, fires on a nest nobody is asking, and would still have to be
//! re-derived on read for a nest that was asleep when it should have fired.
//! Expiry is therefore applied lazily, on the read that cares.
//!
//! **Every bound below is ported by value from the bridge's Go original**, and
//! the reasoning for each is § As built's, not this module's — a constant is
//! restated here only where the port changes what it applies to.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fauna_bridge_atproto::oauth_client::ResolvedClient;
use fauna_bridge_atproto::oauth_par::AcceptedParRequest;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ── Bounds ───────────────────────────────────────────────────────────────────

/// How long a `request_uri` may be redeemed at the authorization endpoint.
/// RFC 9126 wants a short lifetime, and this window is the user-agent latency
/// between a client's POST and the browser redirect it triggers — seconds, not
/// minutes.
pub const PAR_REQUEST_TTL_SECS: i64 = 90;

/// Bounds the PAR store. `/oauth/par` is unauthenticated, so the store must
/// have a ceiling; overflow evicts the nearest-to-expiry entry, which costs
/// that flow a retry and nothing else.
pub const PAR_STORE_CAPACITY: usize = 4096;

/// How long a resolved client stays cached — short enough that a client author
/// who fixes a broken document sees the fix within a coffee break, long enough
/// that a burst of authorizations for one client costs a single fetch.
pub const CLIENT_CACHE_POSITIVE_TTL_SECS: i64 = 15 * 60;

/// How long a **refused** resolution stays cached, deliberately far shorter
/// than the positive TTL. Without negative caching a hostile `client_id` costs
/// one outbound fetch per request — the amplification an unauthenticated
/// endpoint must not offer; with it, a transient outage at a good client's host
/// pins that client as broken. A minute collapses a flood to ~1 fetch/minute
/// while bounding a blip's damage to a minute.
pub const CLIENT_CACHE_NEGATIVE_TTL_SECS: i64 = 60;

/// Bounds the client-metadata cache. Eviction can only ever cost a re-fetch —
/// the cache is never authoritative — so this is a memory bound, not a
/// correctness one.
pub const CLIENT_CACHE_CAPACITY: usize = 512;

/// The granularity of a DPoP nonce. A nonce names the window it was minted in
/// and is accepted while that window or its predecessor is current, so the
/// widest a nonce's life ever gets is two windows.
pub const DPOP_NONCE_WINDOW_SECS: i64 = 2 * 60;

/// The current window plus its predecessor. One window alone would expire a
/// nonce the instant the clock ticked over, mid-flight, for a client that did
/// nothing wrong.
pub const DPOP_NONCE_ACCEPTED_WINDOWS: i64 = 2;

/// Both the nonce's maximum life and the `iat` window a proof is accepted in —
/// deliberately ONE constant. The spec's ceiling is five minutes; four leaves
/// headroom, so widening the accepted-window count later cannot silently cross
/// it.
pub const DPOP_PROOF_MAX_AGE_SECS: i64 = DPOP_NONCE_WINDOW_SECS * DPOP_NONCE_ACCEPTED_WINDOWS;

/// How often the in-memory nonce secret is replaced. Freshness is bounded by
/// the window index inside the MAC, not by this: rotation limits how long one
/// secret is used, and the one-generation overlap only prevents a seam at the
/// rotation instant.
pub const DPOP_NONCE_SECRET_ROTATION_SECS: i64 = 60 * 60;

/// How much of the MAC becomes the nonce. A nonce is an unforgeability token,
/// not a key: 144 bits is far past any forgery budget, and the shorter string
/// keeps the header small.
const DPOP_NONCE_BYTES: usize = 18;

/// Separates this MAC from any other use of the same secret — the same
/// discipline the sealed-blob AADs use.
///
/// It deliberately does **not** match the bridge's `fauna-atproto-dpop-nonce-v1`
/// spelling: this secret is the nest's own, minted here and shared with nothing,
/// and the issuer it protects is deployment-wide rather than one bridge's. A
/// nonce from the two servers was never interchangeable — different secrets —
/// so matching the string would assert a kinship that does not exist.
const DPOP_NONCE_DOMAIN: &[u8] = b"fauna-oauth-dpop-nonce-v1\x00";

/// Bounds the authorization-server plane's seen-`jti` set (`/oauth/par`,
/// `/oauth/token`, `/oauth/revoke`). An entry lives only
/// [`DPOP_PROOF_MAX_AGE_SECS`], so this binds only under a flood the per-IP
/// limit is already refusing: all three endpoints spend one class budget, so a
/// single source can deliver a handful of entries per window and 8192 is
/// therefore roughly a thousand sources held at their ceiling for a whole
/// window.
pub const DPOP_REPLAY_CAPACITY_AS: usize = 8192;

/// Bounds the `private_key_jwt` assertion replay set. An entry lives until the
/// assertion's own capped `exp`, which is why [`ReplaySet::record`] takes the
/// expiry from the caller rather than a store-wide constant.
pub const ASSERTION_REPLAY_CAPACITY: usize = 2048;

// ── The DPoP nonce minter ────────────────────────────────────────────────────

/// Issues and recognises server nonces.
///
/// A nonce is a MAC over the window it belongs to — `HMAC(secret, domain ||
/// window)` — and **not** a random token recorded in a set. `/oauth/par` is
/// anonymous, so a per-request nonce store would be an unauthenticated write
/// surface whose overflow policy has to choose between refusing to issue nonces
/// and issuing ones it cannot recognise. A MAC has no set to overflow, and
/// freshness comes from the window index inside it, so no amount of traffic can
/// make a nonce live longer.
///
/// ⚠ **Minting allocates nothing, and that is load-bearing rather than
/// incidental.** Every endpoint here answers with a fresh nonce before it has
/// decided anything about the request — that is what lets a client recover from
/// a refusal — so minting is reachable anonymously ahead of the request being
/// judged. Replacing this with a random-token-in-a-set minter would silently
/// turn that into an anonymous memory-growth vector; anything stored per nonce
/// needs the rate limit moved ahead of the mint first.
#[derive(Debug)]
pub struct NonceMinter {
    state: Mutex<MinterState>,
}

#[derive(Debug)]
struct MinterState {
    /// Signs every nonce issued from now on.
    current: [u8; 32],
    /// Kept for one rotation so a nonce minted a moment before rotation still
    /// verifies.
    previous: Option<[u8; 32]>,
    rotated_at: i64,
}

impl NonceMinter {
    /// A minter with a fresh secret, installed as of `now`.
    pub fn new(now: i64) -> Self {
        Self {
            state: Mutex::new(MinterState {
                current: rand::random(),
                previous: None,
                rotated_at: now,
            }),
        }
    }

    /// Issue a nonce for the current window.
    pub fn mint(&self, now: i64) -> String {
        let mut state = self.state.lock().expect("nonce minter poisoned");
        Self::rotate_locked(&mut state, now);
        nonce_for(&state.current, window_index(now))
    }

    /// Whether `nonce` is one this server issued recently.
    ///
    /// Re-derives rather than looks up, over the (at most two) secrets and the
    /// (at most two) live windows. The comparison is constant-time throughout
    /// and deliberately does not short-circuit on the first match: a nonce is an
    /// unforgeability token, and a byte-at-a-time comparison would leak the
    /// prefix an attacker had got right.
    pub fn accepts(&self, nonce: &str, now: i64) -> bool {
        if nonce.is_empty() {
            return false;
        }
        let Ok(presented) = URL_SAFE_NO_PAD.decode(nonce) else {
            return false;
        };
        if presented.len() != DPOP_NONCE_BYTES {
            return false;
        }
        let mut state = self.state.lock().expect("nonce minter poisoned");
        Self::rotate_locked(&mut state, now);
        let current_window = window_index(now);
        let mut secrets = vec![state.current];
        if let Some(previous) = state.previous {
            secrets.push(previous);
        }
        let mut found = false;
        for secret in &secrets {
            for step in 0..DPOP_NONCE_ACCEPTED_WINDOWS {
                let mut mac =
                    HmacSha256::new_from_slice(secret).expect("hmac takes any key length");
                mac.update(DPOP_NONCE_DOMAIN);
                mac.update(&(current_window - step).to_be_bytes());
                found |= mac.verify_truncated_left(&presented).is_ok();
            }
        }
        found
    }

    /// Replace the secret when its period is up.
    ///
    /// The fresh secret is generated into a temporary and installed only as a
    /// whole: a minter must never end up serving under a zero secret.
    fn rotate_locked(state: &mut MinterState, now: i64) {
        if now.saturating_sub(state.rotated_at) < DPOP_NONCE_SECRET_ROTATION_SECS {
            return;
        }
        let fresh: [u8; 32] = rand::random();
        state.previous = Some(state.current);
        state.current = fresh;
        state.rotated_at = now;
    }
}

/// The window a moment belongs to. Floor division, so a negative clock (only
/// reachable in a test) still maps monotonically.
fn window_index(now: i64) -> i64 {
    now.div_euclid(DPOP_NONCE_WINDOW_SECS)
}

fn nonce_for(secret: &[u8; 32], window: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac takes any key length");
    mac.update(DPOP_NONCE_DOMAIN);
    mac.update(&window.to_be_bytes());
    let tag = mac.finalize().into_bytes();
    URL_SAFE_NO_PAD.encode(&tag[..DPOP_NONCE_BYTES])
}

// ── The replay set ───────────────────────────────────────────────────────────

/// A bounded set of caller-chosen identifiers already spent.
///
/// ⚠ **`scope` is a parameter, not a convention, and that is the whole point.**
/// Every identifier tracked here is chosen by the caller — a `jti` is an
/// arbitrary string a client puts in its own token — and neither spec makes one
/// globally unique: RFC 7523 §3 scopes an assertion's `jti` uniqueness to *the
/// issuer*, so a per-issuer counter is fully conformant. Keyed on the bare
/// string, any caller could spend an identifier an honest caller was about to
/// present, and the honest request would be refused as a replay — a targeted
/// availability denial. Taking the scope in the signature makes that
/// unrepresentable rather than remembered.
#[derive(Debug)]
pub struct ReplaySet {
    name: &'static str,
    capacity: usize,
    seen: Mutex<HashMap<String, i64>>,
}

impl ReplaySet {
    pub fn new(name: &'static str, capacity: usize) -> Self {
        Self {
            name,
            capacity,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Note `id` within `scope` and report whether that pair had already been
    /// seen. Test-and-insert under one lock, so two concurrent replays of the
    /// same identifier cannot both find the set empty.
    ///
    /// `expires` is when the entry may be forgotten, and the **caller** supplies
    /// it because only the caller knows how long replaying its artifact could
    /// achieve anything: a DPoP proof is bounded by the window this server
    /// mints, a client assertion by its own capped `exp`. Deriving it from a
    /// store-wide constant would make one of the two wrong.
    pub fn record(&self, scope: &str, id: &str, expires: i64, now: i64) -> bool {
        let key = replay_key(scope, id);
        let mut seen = self.seen.lock().expect("replay set poisoned");
        if let Some(&entry_expires) = seen.get(&key)
            && now < entry_expires
        {
            return true;
        }
        if seen.len() >= self.capacity {
            self.evict_locked(&mut seen, now);
        }
        seen.insert(key, expires);
        false
    }

    fn evict_locked(&self, seen: &mut HashMap<String, i64>, now: i64) {
        let before = seen.len();
        seen.retain(|_, expires| now < *expires);
        if seen.len() < before {
            return;
        }
        let oldest = seen
            .iter()
            .min_by_key(|(_, expires)| **expires)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest {
            seen.remove(&key);
            // Every entry was still live, so this dropped an identifier that
            // could now be replayed. Under the per-IP limit this should not be
            // reachable; seeing it means either that assumption or the capacity
            // is wrong.
            tracing::warn!(
                set = self.name,
                capacity = self.capacity,
                "oauth: replay set evicted a live entry — replay window open"
            );
        }
    }
}

/// Join a scope and a caller-chosen identifier into one key.
///
/// Injective because **every scope this set is given is NUL-free by
/// construction** — a `client_id` is a validated absolute URL and a `jkt` is
/// base64url — so the first NUL always delimits. The identifier half needs no
/// such guarantee, which is the right way round: it is the half an attacker
/// chooses.
fn replay_key(scope: &str, id: &str) -> String {
    format!("{scope}\x00{id}")
}

// ── The PAR store ────────────────────────────────────────────────────────────

/// What a `request_uri` resolves to: the validated request plus the client
/// identity it was validated against.
///
/// The client is stored whole rather than flattened into the request, so the
/// consent card has ONE owner of what it renders instead of a copy that can
/// differ from the resolution it came from.
#[derive(Debug, Clone)]
pub struct StoredParRequest {
    pub request: AcceptedParRequest,
    pub client: ResolvedClient,
    /// The RFC-7638 thumbprint of the key the pushing client proved possession
    /// of. The token minted against this request carries it as `cnf.jkt`, which
    /// is what makes a stolen access token useless without the key.
    pub dpop_jkt: String,
    /// The keys the client attested: its capability-grant holder
    /// (`fauna_holder_x25519`, `third-party.md` § The principal model, rule 2)
    /// and its writer (`fauna_writer_ed25519`, `third-party-kinds.md`
    /// § Principal write authority). Pushed in the same DPoP-proven request as
    /// everything above, so they reach the principal row only through a code
    /// redeemed by the SAME key. Empty for a client that presented none.
    pub attested: crate::db::third_party_principals::AttestedKeys,
    pub expires: i64,
}

/// Pushed authorization requests, in memory with a TTL.
#[derive(Debug, Default)]
pub struct ParStore {
    entries: Mutex<HashMap<String, StoredParRequest>>,
}

impl ParStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a validated request under `request_uri`, evicting first if the
    /// store is at its ceiling.
    pub fn put(&self, request_uri: String, stored: StoredParRequest, now: i64) {
        let mut entries = self.entries.lock().expect("par store poisoned");
        if entries.len() >= PAR_STORE_CAPACITY {
            evict_nearest_to_expiry(&mut entries, now, |e| e.expires);
        }
        entries.insert(request_uri, stored);
    }

    /// Redeem a `request_uri`. **Single-use, and the lookup is what consumes
    /// it** — a redemption path cannot forget a rule its only lookup enforces.
    /// An expired handle answers `None` and is dropped in the same call.
    pub fn take(&self, request_uri: &str, now: i64) -> Option<StoredParRequest> {
        let mut entries = self.entries.lock().expect("par store poisoned");
        let stored = entries.remove(request_uri)?;
        (now < stored.expires).then_some(stored)
    }

    /// Look a `request_uri` up **without consuming it** — the same-device
    /// handoff's poll alone (`authorization-server.md` § Consent → *How the
    /// same-device handoff is built*), which asks whether the user's app has
    /// opened the handle yet and must leave it for that app to spend. An
    /// expired handle is reported once and dropped in the same call, so it
    /// answers `expired_token` once and is unknown after.
    pub fn peek(&self, request_uri: &str, now: i64) -> ParPeek {
        let mut entries = self.entries.lock().expect("par store poisoned");
        let Some(stored) = entries.get(request_uri) else {
            return ParPeek::Unknown;
        };
        if now >= stored.expires {
            entries.remove(request_uri);
            return ParPeek::Expired;
        }
        ParPeek::Live(Box::new(stored.clone()))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("par store poisoned").len()
    }
}

/// What a non-consuming [`ParStore::peek`] found.
#[derive(Debug, Clone)]
pub enum ParPeek {
    /// Never pushed, already spent by either door, or evicted.
    Unknown,
    /// Pushed, and past its lifetime. Removed by the peek that found it.
    Expired,
    /// A snapshot of the live request — the store still holds it.
    Live(Box<StoredParRequest>),
}

// ── The client-metadata cache ────────────────────────────────────────────────

/// A resolution outcome worth remembering — the success and the refusal alike.
///
/// The refusal is cached deliberately and separately: see
/// [`CLIENT_CACHE_NEGATIVE_TTL_SECS`] for the amplification it exists to bound.
#[derive(Debug, Clone)]
pub enum CachedResolution {
    Resolved(Box<ResolvedClient>),
    Refused { error: String, description: String },
}

#[derive(Debug, Clone)]
struct CacheEntry {
    resolution: CachedResolution,
    expires: i64,
}

/// Resolved `client_id` documents, by URL.
///
/// Never authoritative: every entry can be re-derived by fetching the document
/// again, which is what licenses the crude eviction policy — the cost of
/// evicting the wrong entry is one re-fetch.
#[derive(Debug, Default)]
pub struct ClientMetadataCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

impl ClientMetadataCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, client_id: &str, now: i64) -> Option<CachedResolution> {
        let mut entries = self.entries.lock().expect("client cache poisoned");
        let entry = entries.get(client_id)?;
        if now < entry.expires {
            return Some(entry.resolution.clone());
        }
        entries.remove(client_id);
        None
    }

    /// Remember a resolution. The TTL follows the outcome, not the caller.
    pub fn put(&self, client_id: String, resolution: CachedResolution, now: i64) {
        let ttl = match resolution {
            CachedResolution::Resolved(_) => CLIENT_CACHE_POSITIVE_TTL_SECS,
            CachedResolution::Refused { .. } => CLIENT_CACHE_NEGATIVE_TTL_SECS,
        };
        let mut entries = self.entries.lock().expect("client cache poisoned");
        if entries.len() >= CLIENT_CACHE_CAPACITY {
            evict_nearest_to_expiry(&mut entries, now, |e| e.expires);
        }
        entries.insert(
            client_id,
            CacheEntry {
                resolution,
                expires: now.saturating_add(ttl),
            },
        );
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("client cache poisoned").len()
    }
}

// ── Shared eviction ──────────────────────────────────────────────────────────

/// Drop every expired entry, and if that freed nothing, the entry nearest to
/// expiry.
///
/// Nearest-to-expiry rather than least-recently-used because neither store
/// holds an access record, and adding one would buy nothing: in both, the cost
/// of evicting the wrong entry is one retry.
fn evict_nearest_to_expiry<V>(
    entries: &mut HashMap<String, V>,
    now: i64,
    expires_of: impl Fn(&V) -> i64,
) {
    let before = entries.len();
    entries.retain(|_, value| now < expires_of(value));
    if entries.len() < before {
        return;
    }
    let oldest = entries
        .iter()
        .min_by_key(|(_, value)| expires_of(value))
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest {
        entries.remove(&key);
    }
}

// ── The runtime ──────────────────────────────────────────────────────────────

/// Everything the authorization server carries between requests: the four
/// stores above, and the one outbound seam it dials with.
///
/// The seam sits here rather than beside them in [`AppState`] because it is
/// where the Go original kept it — one `Server` holding `oauthFetch` next to
/// `parStore` — and because every caller that needs one needs the other: a
/// request that resolves a client both fetches and caches.
///
/// **There is exactly ONE replay set here, and that is the port being honest.**
/// The bridge keeps two — an authorization-server plane and a resource-server
/// plane — because it hosts both, and deriving the set from the `ath` rule is
/// what stops a caller silently picking the wrong one. The nest hosts only the
/// AS plane: the PDS bridge stays the resource server for OAuth XRPC calls, so
/// every proof this server sees has `expected_ath: None` by construction.
/// Carrying a second set for a plane with no endpoint would be dead memory
/// pretending to be a safety property.
pub struct OAuthAsRuntime {
    /// Pushed authorization requests awaiting redemption at `/oauth/authorize`.
    pub par: ParStore,
    /// Resolved `client_id` documents.
    pub clients: ClientMetadataCache,
    /// Issues and recognises the `DPoP-Nonce` every endpoint here answers with.
    pub nonces: NonceMinter,
    /// `jti`s already spent on this server's authorization-server endpoints.
    pub dpop_replays: ReplaySet,
    /// `jti`s already spent by `private_key_jwt` client assertions.
    pub assertion_replays: ReplaySet,
    /// The consent ceremony's own memory — the browser flow, the code it
    /// releases, and the wake-ups that make a resolution immediate. A sibling
    /// module because it belongs to one sub-machine rather than to every
    /// endpoint ([`crate::oauth_as_ceremony`]).
    pub flows: crate::oauth_as_ceremony::AuthorizeFlowStore,
    pub auth_codes: crate::oauth_as_ceremony::AuthCodeStore,
    pub consent_wakes: crate::oauth_as_ceremony::ConsentWakes,
    /// The two polled consent starts' handles — `device_code` and `auth_req_id` —
    /// between the start and their one token answer.
    pub backchannel: crate::oauth_as_ceremony::BackchannelStore,
    /// The permission-set requests in flight — `/oauth/par` asking the PDS
    /// bridge to resolve an `include:` and waiting on the delivery
    /// ([`crate::oauth_as_permission_sets`]).
    pub permission_sets: crate::oauth_as_permission_sets::PermissionSetRequests,
    /// The guarded outbound fetcher client resolution dials through.
    pub fetcher: std::sync::Arc<dyn crate::oauth_as_client::ClientMetadataFetcher>,
}

impl std::fmt::Debug for OAuthAsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The seam is a trait object with no useful rendering, and the stores
        // hold caller-chosen identifiers that have no business in a log line.
        f.debug_struct("OAuthAsRuntime").finish_non_exhaustive()
    }
}

impl OAuthAsRuntime {
    /// A runtime with fresh, empty stores as of `now`, dialling through
    /// `fetcher`.
    pub fn new(
        now: i64,
        fetcher: std::sync::Arc<dyn crate::oauth_as_client::ClientMetadataFetcher>,
    ) -> Self {
        Self {
            par: ParStore::new(),
            clients: ClientMetadataCache::new(),
            nonces: NonceMinter::new(now),
            dpop_replays: ReplaySet::new("oauth-as-dpop", DPOP_REPLAY_CAPACITY_AS),
            assertion_replays: ReplaySet::new("oauth-assertion", ASSERTION_REPLAY_CAPACITY),
            flows: crate::oauth_as_ceremony::AuthorizeFlowStore::new(),
            auth_codes: crate::oauth_as_ceremony::AuthCodeStore::new(),
            consent_wakes: crate::oauth_as_ceremony::ConsentWakes::new(),
            backchannel: crate::oauth_as_ceremony::BackchannelStore::new(),
            permission_sets: crate::oauth_as_permission_sets::PermissionSetRequests::new(),
            fetcher,
        }
    }

    /// The production runtime: the guarded fetcher, and nothing else to choose.
    pub fn production(now: i64) -> Self {
        Self::new(
            now,
            std::sync::Arc::new(crate::oauth_as_client::GuardedMetadataFetcher),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_780_000_000;

    fn resolved_client(client_id: &str) -> ResolvedClient {
        ResolvedClient {
            client_id: client_id.to_string(),
            client_name: None,
            client_uri: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            redirect_uris: vec!["https://app.example/cb".to_string()],
            declared_scopes: vec!["atproto".to_string()],
            confidential: false,
            jwks: Vec::new(),
            jwks_uri: None,
            loopback: false,
            fauna_manifest: None,
        }
    }

    fn stored(client_id: &str, expires: i64) -> StoredParRequest {
        StoredParRequest {
            request: AcceptedParRequest {
                client_id: client_id.to_string(),
                redirect_uri: "https://app.example/cb".to_string(),
                scopes: vec!["atproto".to_string()],
                sets: Vec::new(),
                state: "state".to_string(),
                code_challenge: "challenge".to_string(),
                login_hint: None,
                nonce: None,
            },
            client: resolved_client(client_id),
            dpop_jkt: "jkt".to_string(),
            attested: crate::db::third_party_principals::AttestedKeys::default(),
            expires,
        }
    }

    // ── The nonce minter ─────────────────────────────────────────────────────

    #[test]
    fn a_nonce_this_server_minted_is_recognised() {
        let minter = NonceMinter::new(T0);
        let nonce = minter.mint(T0);
        assert!(minter.accepts(&nonce, T0));
    }

    #[test]
    fn a_nonce_nobody_minted_is_refused() {
        let minter = NonceMinter::new(T0);
        let forged = URL_SAFE_NO_PAD.encode([0u8; DPOP_NONCE_BYTES]);
        assert!(!minter.accepts(&forged, T0));
    }

    #[test]
    fn an_empty_or_unparsable_nonce_is_refused_without_touching_the_secret() {
        let minter = NonceMinter::new(T0);
        assert!(!minter.accepts("", T0));
        assert!(!minter.accepts("not base64url!!", T0));
        // Right alphabet, wrong length — a truncated nonce must not verify
        // against a truncated MAC comparison.
        assert!(!minter.accepts(&URL_SAFE_NO_PAD.encode([0u8; 8]), T0));
    }

    #[test]
    fn a_nonce_survives_the_window_tick_it_was_minted_under() {
        let minter = NonceMinter::new(T0);
        let nonce = minter.mint(T0);
        // One window later the predecessor is still accepted — the whole point
        // of accepting two windows is that a client mid-flight did nothing
        // wrong when the clock ticked over.
        assert!(minter.accepts(&nonce, T0 + DPOP_NONCE_WINDOW_SECS));
    }

    #[test]
    fn a_nonce_dies_once_its_two_windows_are_past() {
        let minter = NonceMinter::new(T0);
        let nonce = minter.mint(T0);
        assert!(
            !minter.accepts(&nonce, T0 + DPOP_NONCE_WINDOW_SECS * 2),
            "a nonce may never outlive DPOP_PROOF_MAX_AGE_SECS"
        );
    }

    #[test]
    fn a_nonce_minted_before_a_rotation_still_verifies_after_it() {
        let minter = NonceMinter::new(T0);
        let rotation = T0 + DPOP_NONCE_SECRET_ROTATION_SECS;
        // Minted under the outgoing secret, in the window the rotation lands in.
        let nonce = minter.mint(rotation - 1);
        // The mint below rotates the secret; the one-generation overlap is what
        // keeps the earlier nonce alive for its remaining window.
        let _ = minter.mint(rotation);
        assert!(minter.accepts(&nonce, rotation));
    }

    #[test]
    fn a_nonce_two_rotations_old_is_refused() {
        let minter = NonceMinter::new(T0);
        let nonce = minter.mint(T0);
        let far = T0 + DPOP_NONCE_SECRET_ROTATION_SECS * 2;
        let _ = minter.mint(T0 + DPOP_NONCE_SECRET_ROTATION_SECS);
        let _ = minter.mint(far);
        assert!(!minter.accepts(&nonce, far));
    }

    #[test]
    fn minting_allocates_no_per_nonce_state() {
        // The property the module docs call load-bearing: an anonymous caller
        // can mint without the server growing. Two thousand mints, and the only
        // state is the secret pair.
        let minter = NonceMinter::new(T0);
        for step in 0..2_000 {
            let _ = minter.mint(T0 + step);
        }
        let state = minter.state.lock().unwrap();
        assert!(state.previous.is_some() || state.rotated_at == T0);
    }

    // ── The replay set ───────────────────────────────────────────────────────

    #[test]
    fn a_fresh_identifier_is_not_a_replay_and_the_second_use_is() {
        let set = ReplaySet::new("test", 16);
        assert!(!set.record("jkt-a", "jti-1", T0 + 60, T0));
        assert!(set.record("jkt-a", "jti-1", T0 + 60, T0));
    }

    #[test]
    fn one_callers_identifier_cannot_burn_anothers() {
        // The targeted-availability attack the scope parameter exists to make
        // unrepresentable: `jti` uniqueness is per-issuer, so two callers may
        // legitimately choose the same string.
        let set = ReplaySet::new("test", 16);
        assert!(!set.record("jkt-attacker", "shared-jti", T0 + 60, T0));
        assert!(
            !set.record("jkt-honest", "shared-jti", T0 + 60, T0),
            "an honest caller's identifier must survive another caller spending the same string"
        );
    }

    #[test]
    fn an_expired_entry_stops_being_a_replay() {
        let set = ReplaySet::new("test", 16);
        assert!(!set.record("jkt-a", "jti-1", T0 + 60, T0));
        assert!(!set.record("jkt-a", "jti-1", T0 + 120, T0 + 61));
    }

    #[test]
    fn the_scope_and_id_join_is_injective() {
        // Without a NUL delimiter these two pairs would collide.
        assert_ne!(replay_key("ab", "c"), replay_key("a", "bc"));
    }

    #[test]
    fn overflow_drops_expired_entries_before_live_ones() {
        let set = ReplaySet::new("test", 4);
        for n in 0..4 {
            assert!(!set.record("jkt", &format!("expired-{n}"), T0 + 10, T0));
        }
        // Every entry above is now expired; the live one must survive.
        assert!(!set.record("jkt", "live", T0 + 1_000, T0 + 11));
        assert!(
            !set.record("jkt", "expired-0", T0 + 1_000, T0 + 11),
            "an expired identifier is forgotten, so re-presenting it is not a replay"
        );
        assert!(
            set.record("jkt", "live", T0 + 1_000, T0 + 12),
            "the live entry must have survived the eviction that made room"
        );
    }

    // ── The PAR store ────────────────────────────────────────────────────────

    #[test]
    fn a_request_uri_is_single_use() {
        let store = ParStore::new();
        store.put(
            "urn:req:1".to_string(),
            stored("https://app.example", T0 + PAR_REQUEST_TTL_SECS),
            T0,
        );
        assert!(store.take("urn:req:1", T0).is_some());
        assert!(
            store.take("urn:req:1", T0).is_none(),
            "the lookup is what consumes the handle"
        );
    }

    #[test]
    fn an_expired_request_uri_does_not_redeem() {
        let store = ParStore::new();
        store.put(
            "urn:req:1".to_string(),
            stored("https://app.example", T0 + PAR_REQUEST_TTL_SECS),
            T0,
        );
        assert!(
            store.take("urn:req:1", T0 + PAR_REQUEST_TTL_SECS).is_none(),
            "the TTL is exclusive at its own instant"
        );
        assert_eq!(store.len(), 0, "and the expired entry is dropped, not left");
    }

    /// The handoff poll's look-up leaves a live handle for the door that
    /// spends it, reports an expired one once, and finds a spent one unknown.
    #[test]
    fn a_peek_never_consumes_a_live_request_uri() {
        let store = ParStore::new();
        store.put(
            "urn:req:1".to_string(),
            stored("https://app.example", T0 + PAR_REQUEST_TTL_SECS),
            T0,
        );
        assert!(matches!(store.peek("urn:req:1", T0), ParPeek::Live(_)));
        assert!(matches!(store.peek("urn:req:1", T0), ParPeek::Live(_)));
        assert!(
            store.take("urn:req:1", T0).is_some(),
            "still there to spend"
        );
        assert!(matches!(store.peek("urn:req:1", T0), ParPeek::Unknown));

        store.put(
            "urn:req:2".to_string(),
            stored("https://app.example", T0 + PAR_REQUEST_TTL_SECS),
            T0,
        );
        let late = T0 + PAR_REQUEST_TTL_SECS;
        assert!(matches!(store.peek("urn:req:2", late), ParPeek::Expired));
        assert!(
            matches!(store.peek("urn:req:2", late), ParPeek::Unknown),
            "expired is answered once"
        );
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn an_unknown_request_uri_redeems_nothing() {
        let store = ParStore::new();
        assert!(store.take("urn:req:never-issued", T0).is_none());
    }

    #[test]
    fn the_store_never_exceeds_its_ceiling() {
        let store = ParStore::new();
        for n in 0..PAR_STORE_CAPACITY + 50 {
            store.put(
                format!("urn:req:{n}"),
                stored("https://app.example", T0 + PAR_REQUEST_TTL_SECS),
                T0,
            );
        }
        assert!(store.len() <= PAR_STORE_CAPACITY);
    }

    // ── The client-metadata cache ────────────────────────────────────────────

    #[test]
    fn a_resolved_client_is_cached_for_the_positive_ttl() {
        let cache = ClientMetadataCache::new();
        cache.put(
            "https://app.example".to_string(),
            CachedResolution::Resolved(Box::new(resolved_client("https://app.example"))),
            T0,
        );
        assert!(
            cache
                .get(
                    "https://app.example",
                    T0 + CLIENT_CACHE_POSITIVE_TTL_SECS - 1
                )
                .is_some()
        );
        assert!(
            cache
                .get("https://app.example", T0 + CLIENT_CACHE_POSITIVE_TTL_SECS)
                .is_none()
        );
    }

    #[test]
    fn a_refusal_is_cached_far_more_briefly_than_a_success() {
        let cache = ClientMetadataCache::new();
        cache.put(
            "https://hostile.example".to_string(),
            CachedResolution::Refused {
                error: "invalid_client".to_string(),
                description: "document did not resolve".to_string(),
            },
            T0,
        );
        assert!(
            cache
                .get(
                    "https://hostile.example",
                    T0 + CLIENT_CACHE_NEGATIVE_TTL_SECS - 1
                )
                .is_some(),
            "a refusal must be cached at all — that is the fetch amplification bound"
        );
        assert!(
            cache
                .get(
                    "https://hostile.example",
                    T0 + CLIENT_CACHE_NEGATIVE_TTL_SECS
                )
                .is_none(),
            "and must expire far sooner than a success, so a blip costs a minute"
        );
    }

    #[test]
    fn the_cache_never_exceeds_its_ceiling() {
        let cache = ClientMetadataCache::new();
        for n in 0..CLIENT_CACHE_CAPACITY + 50 {
            cache.put(
                format!("https://app{n}.example"),
                CachedResolution::Resolved(Box::new(resolved_client("https://app.example"))),
                T0,
            );
        }
        assert!(cache.len() <= CLIENT_CACHE_CAPACITY);
    }

    #[test]
    fn eviction_prefers_the_expired_entry_over_a_live_one() {
        let cache = ClientMetadataCache::new();
        // A refusal expires a minute out; a success fifteen. At T0+61 the
        // refusal is the expired one, so it is what makes room.
        cache.put(
            "https://refused.example".to_string(),
            CachedResolution::Refused {
                error: "invalid_client".to_string(),
                description: String::new(),
            },
            T0,
        );
        cache.put(
            "https://good.example".to_string(),
            CachedResolution::Resolved(Box::new(resolved_client("https://good.example"))),
            T0,
        );
        assert!(cache.get("https://refused.example", T0 + 61).is_none());
        assert!(cache.get("https://good.example", T0 + 61).is_some());
    }
}
