//! The consent ceremony's runtime memory — the browser flow, the authorization
//! code it releases, and the wake-ups that make a resolution immediate.
//!
//! A sibling of [`crate::oauth_as_state`] rather than more of it: that module is
//! the memory every AS endpoint shares (the PAR store, the nonce minter, the
//! replay sets), while everything here belongs to one sub-machine — the browser
//! ceremony between `/oauth/authorize` and the redirect that ends it. Ported
//! from the bridge's since-retired `oauth_authorize.go` (TP5 S2), same bounds, same clock
//! discipline: `now` is passed in as unix seconds and there are no timers.
//!
//! # Three things this port changes on purpose, all forced by Rust
//!
//! 1. **Every operation takes a token; nothing hands out a reference into the
//!    store.** Go passes `*authorizeFlow` around and mutates it under the store
//!    lock. Here the poll loop `await`s between reads, so a borrow into the map
//!    could not survive — and holding a lock across an await is the deadlock
//!    this shape makes unrepresentable rather than merely avoided. Callers get
//!    a [`FlowSnapshot`] instead: a cheap clone of exactly what the loop needs.
//! 2. **The wake registry is a `watch` channel, not a closed channel.** Go's
//!    close-once broadcast has one property the poll loop depends on and a
//!    `Notify` does not have: a wake that lands *between* the caller's fetch and
//!    its wait is still seen, because a closed channel stays ready forever. A
//!    `watch::Receiver` taken before the fetch has the same property — it
//!    remembers the version it started at — so the race closes the same way.
//! 3. **`release` takes a closure that must not `await`.** It runs under the
//!    store lock, exactly as Go's does, because that is what makes "exactly one
//!    authorization code per ceremony" structural rather than a property of
//!    polite callers. The closure mints a code and files it; neither needs I/O.
//!
//! ⚠ **Lock ordering, where both stores are held: flows, then codes.** Only
//! [`AuthorizeFlowStore::release`]'s closure does this, and it is the only place
//! that ever may.

use std::collections::HashMap;
use std::sync::Mutex;

use fauna_protocol::atproto_pds::ConsentSetInfo;

use crate::oauth_as_state::StoredParRequest;

// ── Bounds, ported by value ──────────────────────────────────────────────────

/// Bounds the browser-flow store. Same posture as the PAR store's ceiling:
/// overflow evicts nearest-to-expiry, which costs that flow a retry and nothing
/// else.
pub const AUTHORIZE_FLOW_CAPACITY: usize = 4096;

/// Extends a flow entry past its consent row's expiry, so the page's next poll
/// is answered with the honest "expired" redirect read off the consent row
/// rather than the flow entry vanishing first and answering a generic "gone".
pub const AUTHORIZE_FLOW_SLACK_SECS: i64 = 2 * 60;

/// How long an authorization code may sit between release and redemption at
/// `/oauth/token` — one immediate programmatic exchange, so it is short the way
/// RFC 6749 §4.1.2 expects.
///
/// Deliberately its OWN constant: the consent window bounds a *human* and the
/// DPoP window bounds a *proof*, and sharing a number with either would let a
/// change to one silently move this.
pub const AUTH_CODE_TTL_SECS: i64 = 60;

/// Bounds the code store. Eviction costs a flow a retry — nothing here is worth
/// failing closed over, since a code is single-use and bound to its flow's DPoP
/// key and redirect target either way.
pub const AUTH_CODE_CAPACITY: usize = 4096;

/// The most consents that may have a live wake registration at once.
///
/// ⚠ **The third store's bound, added 2026-09-09 after it was found missing**.
/// The other two here carry a cap and an eviction; this one
/// carried neither, and its ONLY removal path is [`ConsentWakes::wake`] —
/// which fires solely when a user explicitly answers. A ceremony that is
/// long-polled once and then abandoned (tab closed, client stopped, the row
/// left to expire) therefore left an entry behind **forever**: an expiry is
/// not a wake, so nothing ever swept it.
///
/// 4096, the siblings' number, because the thing being bounded is the same
/// thing — concurrent ceremonies on one nest.
pub const CONSENT_WAKE_CAPACITY: usize = 4096;

/// Caps concurrently HELD polls per flow; a poll over the cap answers
/// immediately instead of holding. One page polls serially, so the cap only
/// bites someone multiplying tabs against their own token.
pub const MAX_POLL_WAITERS_PER_FLOW: u32 = 3;

/// Entropy for the browser's poll credential and for the authorization code —
/// 256 bits each, the `request_uri`'s size and for the same reason: whoever
/// holds one collects the flow's one answer, so it must be unguessable.
pub const FLOW_TOKEN_ENTROPY: usize = 32;

// ── The browser flow ─────────────────────────────────────────────────────────

/// One browser's live consent ceremony.
#[derive(Debug, Clone)]
pub struct AuthorizeFlow {
    pub consent_id: Vec<u8>,
    pub par: StoredParRequest,
    /// Memoizes the flow's one answer — the full redirect URL, error redirects
    /// included.
    ///
    /// A lost poll response then re-releases the SAME answer instead of minting
    /// a second code: the second read costs nothing (the code is single-use at
    /// the token endpoint, and the bearer is the same flow-token holder), while
    /// consuming the flow on first release would fail a ceremony the user
    /// already completed over one dropped response.
    pub released: Option<String>,
    pub expires: i64,
    waiters: u32,
}

impl AuthorizeFlow {
    pub fn new(consent_id: Vec<u8>, par: StoredParRequest, expires: i64) -> Self {
        Self {
            consent_id,
            par,
            released: None,
            expires,
            waiters: 0,
        }
    }
}

/// What a poll needs from a flow, cloned out from under the lock.
///
/// Deliberately not a borrow — see the module docs, rule 1.
#[derive(Debug, Clone)]
pub struct FlowSnapshot {
    pub consent_id: Vec<u8>,
    pub released: Option<String>,
    pub redirect_uri: String,
    pub state: String,
    /// The three PAR facts a released grant is built from. Carried in the
    /// snapshot rather than re-read inside [`AuthorizeFlowStore::release`],
    /// because that closure runs under this store's lock and must not take it
    /// again.
    pub client_id: String,
    pub code_challenge: String,
    pub dpop_jkt: String,
    /// The PAR's OIDC `nonce`, carried into the code for the ID token.
    pub nonce: Option<String>,
    /// The PAR's attested keys, carried to the code for the same reason the
    /// three above are.
    pub attested: crate::db::third_party_principals::AttestedKeys,
    /// When this flow dies. Carried so the poll can bound its wake
    /// registration by it — [`CONSENT_WAKE_CAPACITY`] says why that store
    /// needs an expiry at all.
    pub expires: i64,
}

#[derive(Debug, Default)]
pub struct AuthorizeFlowStore {
    entries: Mutex<HashMap<String, AuthorizeFlow>>,
}

impl AuthorizeFlowStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, token: String, flow: AuthorizeFlow) {
        let mut entries = self.entries.lock().expect("flow store poisoned");
        if entries.len() >= AUTHORIZE_FLOW_CAPACITY {
            evict_nearest_to_expiry(&mut entries, |f| f.expires);
        }
        entries.insert(token, flow);
    }

    /// The live flow for a token, expiring lazily.
    pub fn get(&self, token: &str, now: i64) -> Option<FlowSnapshot> {
        let mut entries = self.entries.lock().expect("flow store poisoned");
        let flow = entries.get(token)?;
        if now >= flow.expires {
            entries.remove(token);
            return None;
        }
        Some(FlowSnapshot {
            consent_id: flow.consent_id.clone(),
            released: flow.released.clone(),
            redirect_uri: flow.par.request.redirect_uri.clone(),
            state: flow.par.request.state.clone(),
            client_id: flow.par.request.client_id.clone(),
            code_challenge: flow.par.request.code_challenge.clone(),
            dpop_jkt: flow.par.dpop_jkt.clone(),
            nonce: flow.par.request.nonce.clone(),
            attested: flow.par.attested,
            expires: flow.expires,
        })
    }

    /// Reserve a held-poll slot. `false` means the per-flow cap is reached and
    /// the caller must answer without holding.
    pub fn add_waiter(&self, token: &str) -> bool {
        let mut entries = self.entries.lock().expect("flow store poisoned");
        let Some(flow) = entries.get_mut(token) else {
            return false;
        };
        if flow.waiters >= MAX_POLL_WAITERS_PER_FLOW {
            return false;
        }
        flow.waiters += 1;
        true
    }

    pub fn drop_waiter(&self, token: &str) {
        let mut entries = self.entries.lock().expect("flow store poisoned");
        if let Some(flow) = entries.get_mut(token) {
            flow.waiters = flow.waiters.saturating_sub(1);
        }
    }

    /// Memoize the flow's one answer.
    ///
    /// The first caller's `build` runs under the store lock and its result wins;
    /// every later caller — a concurrent poll that raced the same resolution, or
    /// a retry after a lost response — gets the memoized answer and its `build`
    /// is never run. **That is what makes "exactly one authorization code per
    /// ceremony" structural** rather than a property of polite callers.
    ///
    /// ⚠ `build` runs while the lock is held: it must not block or await. It may
    /// take the auth-code store's lock — that ordering (flows, then codes) is
    /// the module docs' rule, and this is its only site.
    ///
    /// `None` means the flow is gone (expired between the poll's read and here).
    pub fn release(&self, token: &str, build: impl FnOnce() -> String) -> Option<String> {
        let mut entries = self.entries.lock().expect("flow store poisoned");
        let flow = entries.get_mut(token)?;
        if let Some(answer) = &flow.released {
            return Some(answer.clone());
        }
        let answer = build();
        flow.released = Some(answer.clone());
        Some(answer)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("flow store poisoned").len()
    }
}

// ── The authorization-code store ─────────────────────────────────────────────

/// Everything `/oauth/token` needs to finish the flow.
///
/// ⚠ `scopes` and `sets` are the set read back from the **consent row the user
/// was shown** — never the flow's own PAR copy. That is what stops the recorded
/// grant ever being wider than the card.
#[derive(Debug, Clone)]
pub struct StoredAuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub sets: Vec<ConsentSetInfo>,
    pub actor_id: Vec<u8>,
    /// The tokens' `sub` — [`crate::oauth_as_token::grant_subject`] decided it
    /// at release, from the scopes above.
    pub subject: String,
    pub code_challenge: String,
    /// The thumbprint the PAR was pushed under — the eventual token's `cnf.jkt`.
    pub dpop_jkt: String,
    /// The PAR's OIDC `nonce`, for the ID token this code's redemption mints.
    pub nonce: Option<String>,
    /// The keys the PAR attested — what the principal row records.
    pub attested: crate::db::third_party_principals::AttestedKeys,
    pub expires: i64,
}

#[derive(Debug, Default)]
pub struct AuthCodeStore {
    entries: Mutex<HashMap<String, StoredAuthCode>>,
}

impl AuthCodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, code: String, entry: StoredAuthCode) {
        let mut entries = self.entries.lock().expect("auth code store poisoned");
        if entries.len() >= AUTH_CODE_CAPACITY {
            evict_nearest_to_expiry(&mut entries, |e| e.expires);
        }
        entries.insert(code, entry);
    }

    /// Resolve a code and **remove** it — single use lives with the store, the
    /// same rule the PAR store's `take` enforces: a redemption path cannot
    /// forget a rule its only lookup enforces. The token endpoint is its caller.
    pub fn take(&self, code: &str, now: i64) -> Option<StoredAuthCode> {
        let mut entries = self.entries.lock().expect("auth code store poisoned");
        let entry = entries.remove(code)?;
        (now < entry.expires).then_some(entry)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("auth code store poisoned").len()
    }
}

// ── The consent-resolved wake registry ───────────────────────────────────────

/// One consent's wake registration, and the instant past which it is certainly
/// useless.
#[derive(Debug)]
struct WakeEntry {
    sender: tokio::sync::watch::Sender<u64>,
    /// The **flow's** expiry, not the consent row's — the flow outlives the
    /// consent by [`AUTHORIZE_FLOW_SLACK_SECS`], and no poll can subscribe
    /// once its flow is gone. Past it, nothing will ever hold on this entry
    /// again.
    expires: i64,
}

/// Turns a consent resolution into wake-ups for the polls holding on it.
///
/// One-shot per consent: waking drops the entry, so a later poll for the same
/// consent registers afresh and learns the answer from its own read instead.
/// A wake for a consent nobody holds is a no-op, which is the whole point of
/// the nudge being best-effort — a lost one costs a fallback interval, never a
/// stuck ceremony.
#[derive(Debug, Default)]
pub struct ConsentWakes {
    senders: Mutex<HashMap<Vec<u8>, WakeEntry>>,
}

impl ConsentWakes {
    pub fn new() -> Self {
        Self::default()
    }

    /// A receiver that will see any wake from **now** on — including one that
    /// lands before the caller gets around to awaiting it.
    ///
    /// That ordering property is the reason this is a `watch` rather than a
    /// `Notify`: subscribe, then read the consent, then wait. A resolution in
    /// the gap bumps the version, and the wait returns immediately rather than
    /// blocking until the fallback interval.
    ///
    /// `expires` is the caller's flow expiry, and it exists so this store can
    /// be bounded like its two siblings — see [`CONSENT_WAKE_CAPACITY`] for the
    /// leak it closes. A repeat subscribe on a live key refreshes nothing and
    /// allocates nothing: the entry is already there, and its expiry was
    /// already the flow's.
    pub fn subscribe(&self, consent_id: &[u8], expires: i64) -> tokio::sync::watch::Receiver<u64> {
        let mut senders = self.senders.lock().expect("consent wakes poisoned");
        if !senders.contains_key(consent_id) && senders.len() >= CONSENT_WAKE_CAPACITY {
            // Nearest-to-expiry first, the direction both siblings evict in and
            // for the same reason: a flood's victim is mid-ceremony in the
            // NEWEST request, so shedding the oldest is what keeps the person
            // actually waiting from being the one who loses.
            //
            // ⚠ Evicting a LIVE registration is safe by this module's own
            // contract — the wake is a nudge, so losing one costs its holder a
            // fallback interval and never a stuck ceremony (see the type docs).
            // That is what makes a hard cap the right shape here where it would
            // not be on a store whose entries are authoritative.
            evict_nearest_to_expiry(&mut senders, |e| e.expires);
        }
        senders
            .entry(consent_id.to_vec())
            .or_insert_with(|| WakeEntry {
                sender: tokio::sync::watch::channel(0).0,
                expires,
            })
            .sender
            .subscribe()
    }

    /// Wake every poll held on this consent, and forget it.
    pub fn wake(&self, consent_id: &[u8]) {
        let mut senders = self.senders.lock().expect("consent wakes poisoned");
        if let Some(entry) = senders.remove(consent_id) {
            // A send with no receivers is an error, and an expected one: the
            // holders may all have timed out. Nothing to do about it.
            let _ = entry.sender.send(1);
        }
    }

    /// Drop every registration whose flow has expired.
    ///
    /// The cap above bounds the store absolutely; this is what keeps it near
    /// EMPTY in the ordinary case, where nothing is flooding and abandoned
    /// ceremonies simply accumulate one at a time. Called on the poll path,
    /// which is the only path that ever grows the map — so the sweep runs
    /// exactly as often as the growth it answers, and never on a nest nobody
    /// is asking.
    pub fn sweep_expired(&self, now: i64) {
        let mut senders = self.senders.lock().expect("consent wakes poisoned");
        senders.retain(|_, entry| now < entry.expires);
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.senders.lock().expect("consent wakes poisoned").len()
    }

    /// Whether one specific consent currently holds a live registration —
    /// `pub(crate)` (rather than `#[cfg(test)] fn`, like its sibling
    /// [`Self::tracked`]) because the route-level tests in
    /// `oauth_as_routes` need to synchronize on ONE id's presence, not just
    /// the store's total size, to pace a repeated eviction race without
    /// racing on wall-clock time.
    #[cfg(test)]
    pub(crate) fn contains(&self, consent_id: &[u8]) -> bool {
        self.senders
            .lock()
            .expect("consent wakes poisoned")
            .contains_key(consent_id)
    }
}

// ── The polled starts: typed code, quiet push and same-device handoff ────────

/// Bounds the polled-start store. Same posture and number as its siblings:
/// overflow evicts nearest-to-expiry, costing that flow a retry.
pub const BACKCHANNEL_CAPACITY: usize = 4096;

/// The polling interval both polled starts advertise — RFC 8628 §3.2's default
/// and the value CIBA Core §7.3 recommends.
pub const BACKCHANNEL_POLL_INTERVAL_SECS: i64 = 5;

/// How much a `slow_down` answer widens the interval, for that poll and every
/// later one (RFC 8628 §3.5; CIBA Core §11 asks for at least this much).
pub const BACKCHANNEL_SLOW_DOWN_STEP_SECS: i64 = 5;

/// Which polled start a handle belongs to. A `device_code` is never an
/// `auth_req_id` nor a handoff's `request_uri`: the store answers a handle
/// presented under another start's grant type as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackchannelStart {
    TypedCode,
    Push,
    /// The same-device handoff, keyed by the PAR's own `request_uri` once the
    /// user's app has opened it (`fauna.oauth.consent.open_handoff`).
    Handoff,
}

/// One polled consent start — a device's `device_code`, a CIBA client's
/// `auth_req_id` or a handoff's `request_uri` — between the start and its one
/// token answer.
///
/// Bridge-free runtime memory like the browser flow (C6: a restart fails the
/// flow cleanly and the client starts again); the consent **row** it points at
/// is nest state, which is what the user's app reads and answers.
#[derive(Debug, Clone)]
pub struct BackchannelFlow {
    pub start: BackchannelStart,
    /// The consent row the user answers — `None` for a quiet push that opened
    /// nothing (an unresolved hint, a blocked client). Such a flow polls
    /// `authorization_pending` until it expires, which is exactly what a flow
    /// nobody answers does: the one uniform reply rule (c) and the no-oracle
    /// rule both require.
    pub consent_id: Option<Vec<u8>>,
    pub client_id: String,
    /// The thumbprint the start was proved under. The token exchange must be
    /// proved under the same key — what makes a stolen handle useless, and the
    /// property PKCE buys a browser flow's code.
    pub dpop_jkt: String,
    /// The keys the client attested at the start, carried to the grant
    /// the approval records (`third-party.md` § The principal model).
    pub attested: crate::db::third_party_principals::AttestedKeys,
    /// The S256 challenge the start's pushed request carried — the handoff's
    /// alone (it begins with PAR, and PAR demands one); `None` for the two
    /// starts that push no challenge. A poll of a flow carrying one must
    /// present its `code_verifier`.
    pub code_challenge: Option<String>,
    pub expires: i64,
    interval: i64,
    last_poll: Option<i64>,
}

impl BackchannelFlow {
    pub fn new(
        start: BackchannelStart,
        consent_id: Option<Vec<u8>>,
        client_id: String,
        dpop_jkt: String,
        attested: crate::db::third_party_principals::AttestedKeys,
        expires: i64,
    ) -> Self {
        Self {
            start,
            consent_id,
            client_id,
            dpop_jkt,
            attested,
            code_challenge: None,
            expires,
            interval: BACKCHANNEL_POLL_INTERVAL_SECS,
            last_poll: None,
        }
    }

    /// The flow with the S256 challenge its pushed request carried.
    pub fn with_code_challenge(mut self, code_challenge: String) -> Self {
        self.code_challenge = Some(code_challenge);
        self
    }
}

/// What a presented handle names right now.
#[derive(Debug, Clone)]
pub enum BackchannelLookup {
    /// Never issued, already redeemed or answered, evicted, or issued under
    /// the other start — one answer for all of them.
    Unknown,
    /// Issued, and past its expiry. Removed by the lookup that found it.
    Expired,
    Live(BackchannelFlow),
}

/// A pending poll's pacing verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackchannelPace {
    /// Polled no faster than the interval: answer `authorization_pending`.
    InTime,
    /// Polled too soon: answer `slow_down`. The interval has already been
    /// widened for this poll and every later one.
    TooSoon,
}

#[derive(Debug, Default)]
pub struct BackchannelStore {
    entries: Mutex<HashMap<String, BackchannelFlow>>,
}

impl BackchannelStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, handle: String, flow: BackchannelFlow) {
        let mut entries = self.entries.lock().expect("backchannel store poisoned");
        if entries.len() >= BACKCHANNEL_CAPACITY {
            evict_nearest_to_expiry(&mut entries, |e| e.expires);
        }
        entries.insert(handle, flow);
    }

    /// A snapshot of the flow `handle` names under `start`. Never consumes a
    /// live flow — a poll that is still pending must leave it for the next —
    /// but an expired one is removed as it is reported, so it answers
    /// `expired_token` once and is unknown after.
    pub fn get(&self, handle: &str, start: BackchannelStart, now: i64) -> BackchannelLookup {
        let mut entries = self.entries.lock().expect("backchannel store poisoned");
        let Some(flow) = entries.get(handle) else {
            return BackchannelLookup::Unknown;
        };
        if flow.start != start {
            return BackchannelLookup::Unknown;
        }
        if now >= flow.expires {
            entries.remove(handle);
            return BackchannelLookup::Expired;
        }
        BackchannelLookup::Live(flow.clone())
    }

    /// Record a pending poll and judge its pacing.
    ///
    /// Called only for a poll that is **still pending** and that has already
    /// proved the flow's key: a caller without the key must not be able to
    /// push the honest client into `slow_down`, and a flow the user has
    /// answered is answered at once however soon it is asked — `slow_down` is
    /// RFC 8628's "variant of `authorization_pending`", and there is nothing
    /// pending left to pace.
    pub fn pace(&self, handle: &str, now: i64) -> BackchannelPace {
        let mut entries = self.entries.lock().expect("backchannel store poisoned");
        let Some(flow) = entries.get_mut(handle) else {
            return BackchannelPace::InTime;
        };
        let too_soon = flow
            .last_poll
            .is_some_and(|last| now.saturating_sub(last) < flow.interval);
        flow.last_poll = Some(now);
        if too_soon {
            flow.interval = flow
                .interval
                .saturating_add(BACKCHANNEL_SLOW_DOWN_STEP_SECS);
            BackchannelPace::TooSoon
        } else {
            BackchannelPace::InTime
        }
    }

    /// Remove and return the flow — its one terminal answer is being given.
    /// `None` means a concurrent poll got there first, and this one must not
    /// mint a second set of tokens from one approval.
    pub fn take(&self, handle: &str) -> Option<BackchannelFlow> {
        self.entries
            .lock()
            .expect("backchannel store poisoned")
            .remove(handle)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("backchannel store poisoned")
            .len()
    }
}

// ── Shared eviction ──────────────────────────────────────────────────────────

/// Drop the entry nearest to expiry.
///
/// Nearest-to-expiry rather than least-recently-used because neither store
/// holds an access record, and adding one would buy nothing: in both, the cost
/// of evicting the wrong entry is one retry.
fn evict_nearest_to_expiry<K: Clone + std::hash::Hash + Eq, V>(
    entries: &mut HashMap<K, V>,
    expires_of: impl Fn(&V) -> i64,
) {
    let oldest = entries
        .iter()
        .min_by_key(|(_, value)| expires_of(value))
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest {
        entries.remove(&key);
    }
}

/// Mint a bearer handle — a flow token or an authorization code.
///
/// One function for both because they are the same artifact in two roles: 256
/// bits of entropy behind a store lookup, held by exactly one party.
pub fn mint_handle() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;

    let mut buf = [0u8; FLOW_TOKEN_ENTROPY];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    /// Far enough ahead that no test below is ever sweeping by accident.
    const FAR: i64 = i64::MAX;

    use super::*;
    use fauna_bridge_atproto::oauth_client::ResolvedClient;
    use fauna_bridge_atproto::oauth_par::AcceptedParRequest;

    fn par(redirect_uri: &str, state: &str) -> StoredParRequest {
        StoredParRequest {
            request: AcceptedParRequest {
                client_id: "http://localhost".into(),
                redirect_uri: redirect_uri.into(),
                scopes: vec!["atproto".into()],
                sets: vec![],
                state: state.into(),
                code_challenge: "challenge".into(),
                login_hint: None,
                nonce: None,
            },
            client: ResolvedClient {
                client_id: "http://localhost".into(),
                client_name: None,
                client_uri: None,
                logo_uri: None,
                tos_uri: None,
                policy_uri: None,
                redirect_uris: vec![redirect_uri.into()],
                declared_scopes: vec!["atproto".into()],
                confidential: false,
                jwks: vec![],
                jwks_uri: None,
                loopback: true,
                fauna_manifest: None,
            },
            dpop_jkt: "thumbprint".into(),
            attested: crate::db::third_party_principals::AttestedKeys::default(),
            expires: 2_000,
        }
    }

    fn flow(expires: i64) -> AuthorizeFlow {
        AuthorizeFlow::new(vec![1, 2, 3], par("http://127.0.0.1/cb", "csrf"), expires)
    }

    /// The property the whole ceremony rests on: however many polls race the
    /// same resolution, exactly one builds an answer and every other caller
    /// gets that same one back.
    #[test]
    fn a_flow_releases_exactly_one_answer() {
        let store = AuthorizeFlowStore::new();
        store.put("tok".into(), flow(2_000));

        let mut builds = 0;
        let first = store
            .release("tok", || {
                builds += 1;
                "https://client.example/cb?code=one".to_string()
            })
            .expect("flow present");
        let second = store
            .release("tok", || {
                builds += 1;
                "https://client.example/cb?code=two".to_string()
            })
            .expect("flow present");

        assert_eq!(first, second);
        assert_eq!(builds, 1, "a second answer was built for one ceremony");
        assert!(first.ends_with("code=one"));
    }

    /// A lost poll response must not cost the user their ceremony: the retry
    /// reads the memoized answer rather than finding the flow consumed.
    #[test]
    fn a_released_flow_still_answers_a_later_read() {
        let store = AuthorizeFlowStore::new();
        store.put("tok".into(), flow(2_000));
        store.release("tok", || "https://client.example/cb?code=x".into());

        let snapshot = store.get("tok", 1_000).expect("flow still live");
        assert_eq!(
            snapshot.released.as_deref(),
            Some("https://client.example/cb?code=x")
        );
    }

    /// Expiry is lazy and on the read that cares — and the read that finds it
    /// expired also drops it, so a dead flow cannot linger holding a slot.
    #[test]
    fn an_expired_flow_answers_nothing_and_is_dropped() {
        let store = AuthorizeFlowStore::new();
        store.put("tok".into(), flow(2_000));
        assert!(store.get("tok", 2_000).is_none());
        assert_eq!(store.len(), 0);
    }

    /// The held-poll cap bites the third concurrent holder, and releasing a
    /// slot lets the next one hold.
    #[test]
    fn the_waiter_cap_bounds_held_polls_per_flow() {
        let store = AuthorizeFlowStore::new();
        store.put("tok".into(), flow(2_000));
        for _ in 0..MAX_POLL_WAITERS_PER_FLOW {
            assert!(store.add_waiter("tok"));
        }
        assert!(!store.add_waiter("tok"), "the cap did not bite");
        store.drop_waiter("tok");
        assert!(store.add_waiter("tok"), "a freed slot was not reusable");
    }

    /// An unknown token holds nothing — the cap cannot be spent on a flow that
    /// does not exist.
    #[test]
    fn an_unknown_token_reserves_no_slot() {
        let store = AuthorizeFlowStore::new();
        assert!(!store.add_waiter("nope"));
        assert!(store.release("nope", || "unused".into()).is_none());
    }

    /// The store never exceeds its ceiling, and overflow takes the entry
    /// nearest to expiry.
    #[test]
    fn the_flow_store_never_exceeds_its_ceiling() {
        let store = AuthorizeFlowStore::new();
        for i in 0..AUTHORIZE_FLOW_CAPACITY + 10 {
            store.put(format!("tok-{i}"), flow(2_000 + i as i64));
        }
        assert!(store.len() <= AUTHORIZE_FLOW_CAPACITY);
        assert!(
            store.get("tok-0", 1_000).is_none(),
            "the nearest-to-expiry entry survived the overflow"
        );
    }

    fn code(expires: i64) -> StoredAuthCode {
        StoredAuthCode {
            client_id: "http://localhost".into(),
            redirect_uri: "http://127.0.0.1/cb".into(),
            scopes: vec!["atproto".into()],
            sets: vec![],
            actor_id: vec![7; 32],
            subject: "did:plc:example".into(),
            code_challenge: "challenge".into(),
            dpop_jkt: "thumbprint".into(),
            nonce: None,
            attested: crate::db::third_party_principals::AttestedKeys::default(),
            expires,
        }
    }

    /// Single use lives with the store: the lookup is what consumes it, so no
    /// redemption path can forget the rule.
    #[test]
    fn an_authorization_code_is_single_use() {
        let store = AuthCodeStore::new();
        store.put("c".into(), code(2_000));
        assert!(store.take("c", 1_000).is_some());
        assert!(store.take("c", 1_000).is_none());
    }

    /// An expired code redeems nothing — and is spent by the attempt, so it
    /// cannot be retried into a live window that never comes.
    #[test]
    fn an_expired_code_redeems_nothing() {
        let store = AuthCodeStore::new();
        store.put("c".into(), code(2_000));
        assert!(store.take("c", 2_000).is_none());
        assert_eq!(store.len(), 0);
    }

    /// The wake a poll cares about is the one that lands **between** its
    /// subscribe and its wait. This is the race the `watch` shape exists to
    /// close, and the reason a `Notify` would be wrong here.
    #[tokio::test]
    async fn a_wake_between_subscribing_and_waiting_is_still_seen() {
        let wakes = ConsentWakes::new();
        let mut rx = wakes.subscribe(&[1, 2, 3], FAR);
        // The resolution lands while the caller is off reading the consent row.
        wakes.wake(&[1, 2, 3]);
        // The wait must return immediately, not block until a fallback tick.
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed())
            .await
            .expect("the wake was missed")
            .expect("sender dropped without a send");
    }

    /// Waking is one-shot per consent: the entry is forgotten, so the registry
    /// does not grow by one channel per ceremony ever run.
    #[tokio::test]
    async fn waking_forgets_the_consent() {
        let wakes = ConsentWakes::new();
        let _rx = wakes.subscribe(&[9], FAR);
        assert_eq!(wakes.tracked(), 1);
        wakes.wake(&[9]);
        assert_eq!(wakes.tracked(), 0);
        // A wake for a consent nobody holds is a no-op, not a panic.
        wakes.wake(&[9]);
    }

    /// Two consents do not share a channel — one ceremony's resolution must
    /// never wake another's poll into re-reading for nothing.
    #[tokio::test]
    async fn one_consents_wake_does_not_disturb_another() {
        let wakes = ConsentWakes::new();
        let mut other = wakes.subscribe(&[2], FAR);
        wakes.wake(&[1]);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), other.changed())
                .await
                .is_err(),
            "an unrelated consent's wake was delivered"
        );
    }

    /// **The leak dliv named, asserted end to end.** An abandoned ceremony —
    /// long-polled once, never answered — used to leave an entry behind
    /// forever, because `wake` is the only remover and an expiry is not a wake.
    ///
    /// Note what this does NOT do: it never calls `wake`. That is the whole
    /// point — the pre-fix store had no other way to shrink, so the assertion
    /// is that time alone is now enough.
    #[tokio::test]
    async fn an_abandoned_ceremony_is_swept_by_its_own_expiry() {
        let wakes = ConsentWakes::new();
        for id in 0u8..8 {
            let _rx = wakes.subscribe(&[id], 1_000);
        }
        assert_eq!(wakes.tracked(), 8);

        // Still live: a sweep at any instant before the expiry must keep them,
        // or an in-flight poll would silently lose its nudge.
        wakes.sweep_expired(999);
        assert_eq!(
            wakes.tracked(),
            8,
            "a sweep before the expiry dropped live entries"
        );

        wakes.sweep_expired(1_000);
        assert_eq!(wakes.tracked(), 0, "expired registrations were not swept");
    }

    /// The cap is absolute, and — the half the sibling store's own test misses
    /// — it holds **with the clock standing still**, which is the only case a
    /// flood actually presents. A test that advances time past the window
    /// before its one over-threshold insert proves nothing about growth.
    #[tokio::test]
    async fn the_store_is_bounded_even_when_nothing_has_expired() {
        let wakes = ConsentWakes::new();
        // Every entry expires far in the future, so nothing is ever
        // sweepable and the cap is the only thing that can hold.
        for id in 0..(CONSENT_WAKE_CAPACITY + 64) {
            let _rx = wakes.subscribe(&id.to_le_bytes(), FAR);
        }
        assert!(
            wakes.tracked() <= CONSENT_WAKE_CAPACITY,
            "the store grew past its cap with no expired entry to shed: {}",
            wakes.tracked()
        );
    }

    /// Re-subscribing to a LIVE consent is free — one entry per ceremony, not
    /// one per poll iteration. `hold_for_resolution` re-subscribes on every
    /// turn of its loop, so if this were not true a single patient browser
    /// would fill the store by itself.
    #[tokio::test]
    async fn re_subscribing_to_one_consent_adds_no_second_entry() {
        let wakes = ConsentWakes::new();
        let _a = wakes.subscribe(&[7], FAR);
        let _b = wakes.subscribe(&[7], FAR);
        let _c = wakes.subscribe(&[7], FAR);
        assert_eq!(wakes.tracked(), 1);
    }

    /// An eviction costs its holder a fallback interval and never the
    /// ceremony: the receiver stays valid, the poll just stops being nudged
    /// and falls back to its own timer. That property is what makes a hard cap
    /// the right shape for this store — it would be wrong on one whose entries
    /// are authoritative.
    #[tokio::test]
    async fn an_evicted_registration_leaves_its_holder_functional() {
        let wakes = ConsentWakes::new();
        // The oldest by expiry, so the cap sheds this one first.
        let mut victim = wakes.subscribe(&[0], 10);
        for id in 1..=(CONSENT_WAKE_CAPACITY as u32) {
            let _rx = wakes.subscribe(&id.to_le_bytes(), FAR);
        }
        assert!(wakes.tracked() <= CONSENT_WAKE_CAPACITY);

        // ⚠ What an eviction actually does to a holder, asserted rather than
        // assumed — the first version of this test assumed the receiver would
        // go QUIET and was wrong, which is the whole reason this assertion is
        // worth its lines.
        //
        // Dropping the entry drops the sender, so `changed()` resolves to an
        // ERROR immediately. A caller that folds that into "timed out" spins as
        // fast as the CPU allows until its deadline — on the one endpoint an
        // anonymous caller can hold open. `hold_for_resolution` therefore
        // matches the closed arm explicitly and takes its fallback interval;
        // this is the property that arm exists for.
        let closed = tokio::time::timeout(std::time::Duration::from_millis(50), victim.changed())
            .await
            .expect("an evicted registration must resolve at once, not hang");
        assert!(
            closed.is_err(),
            "an evicted registration delivered a phantom WAKE rather than closing"
        );
    }

    /// Two mints never collide, and a handle is the full 256 bits.
    #[test]
    fn a_handle_carries_its_full_entropy() {
        let a = mint_handle();
        assert_ne!(a, mint_handle());
        assert_eq!(a.len(), 43, "32 bytes base64url-no-pad is 43 characters");
    }

    fn polled(start: BackchannelStart, expires: i64) -> BackchannelFlow {
        BackchannelFlow::new(
            start,
            Some(vec![1; 32]),
            "http://localhost".into(),
            "jkt".into(),
            Default::default(),
            expires,
        )
    }

    /// A handle names exactly one start: a `device_code` presented as an
    /// `auth_req_id` (or the reverse) is unknown, and a live lookup never
    /// consumes — a pending poll leaves the flow for the next one.
    #[test]
    fn a_handle_answers_only_under_its_own_start_and_a_lookup_never_consumes() {
        let store = BackchannelStore::new();
        store.put("h".into(), polled(BackchannelStart::TypedCode, 100));

        assert!(matches!(
            store.get("h", BackchannelStart::Push, 10),
            BackchannelLookup::Unknown
        ));
        for _ in 0..2 {
            assert!(matches!(
                store.get("h", BackchannelStart::TypedCode, 10),
                BackchannelLookup::Live(_)
            ));
        }
        assert!(store.take("h").is_some(), "the terminal answer takes it");
        assert!(
            store.take("h").is_none(),
            "and only once — one approval, one set of tokens"
        );
        assert!(matches!(
            store.get("h", BackchannelStart::TypedCode, 10),
            BackchannelLookup::Unknown
        ));
    }

    /// Expiry is reported once, then the handle is gone.
    #[test]
    fn an_expired_flow_answers_expired_once_and_is_unknown_after() {
        let store = BackchannelStore::new();
        store.put("h".into(), polled(BackchannelStart::Push, 100));
        assert!(matches!(
            store.get("h", BackchannelStart::Push, 100),
            BackchannelLookup::Expired
        ));
        assert!(matches!(
            store.get("h", BackchannelStart::Push, 100),
            BackchannelLookup::Unknown
        ));
    }

    /// RFC 8628 §3.5's pacing: the first poll is in time, a poll inside the
    /// interval is `slow_down` AND widens the interval for every later poll, and
    /// a poll that waits the widened interval is in time again.
    #[test]
    fn polling_too_soon_slows_down_and_widens_the_interval_for_good() {
        let store = BackchannelStore::new();
        store.put("h".into(), polled(BackchannelStart::TypedCode, FAR));
        let step = BACKCHANNEL_SLOW_DOWN_STEP_SECS;
        let base = BACKCHANNEL_POLL_INTERVAL_SECS;

        assert_eq!(store.pace("h", 0), BackchannelPace::InTime);
        assert_eq!(store.pace("h", base - 1), BackchannelPace::TooSoon);
        // The interval is now base + step, measured from the too-soon poll.
        let at = base - 1;
        assert_eq!(store.pace("h", at + base), BackchannelPace::TooSoon);
        let at = at + base;
        assert_eq!(
            store.pace("h", at + base + 2 * step),
            BackchannelPace::InTime
        );
    }

    /// Bounded like its siblings, evicting nearest-to-expiry.
    #[test]
    fn the_store_is_bounded_and_evicts_nearest_to_expiry() {
        let store = BackchannelStore::new();
        for i in 0..BACKCHANNEL_CAPACITY {
            store.put(
                format!("h{i}"),
                polled(BackchannelStart::Push, 1_000 + i as i64),
            );
        }
        store.put("newest".into(), polled(BackchannelStart::Push, 1_000_000));
        assert_eq!(store.len(), BACKCHANNEL_CAPACITY);
        assert!(matches!(
            store.get("h0", BackchannelStart::Push, 0),
            BackchannelLookup::Unknown
        ));
        assert!(matches!(
            store.get("newest", BackchannelStart::Push, 0),
            BackchannelLookup::Live(_)
        ));
    }
}
