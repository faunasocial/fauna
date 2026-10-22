//! Ed25519 challenge-response authentication — the nonce store.
//!
//! Two-step silent sign-in: the client requests a nonce, signs the
//! domain-tagged `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` (built by the single
//! source `fauna_protocol::auth::challenge_verify_signed_message`, tagged-only
//! since 2026-08-17) with its Ed25519 secret, and submits the signature; the
//! nest verifies it, looks up the actor's registration, and issues a bearer
//! token. Unregistered actors get `fauna.auth.not_registered`, which drops the
//! client into the `nest_register` / `nest_claim` onboarding stage.
//!
//! The ceremony rides the pre-identity WS-RPC kinds `fauna.auth.challenge` /
//! `fauna.auth.verify` (`auth_handlers`, sharing the transport-agnostic
//! `auth_core::{issue_challenge_core, verify_core}` ceremony). The old
//! `POST /api/v1/auth/{challenge,verify}` HTTP twins were deleted in the
//! WS-RPC-everywhere rip-out once every consumer migrated; this module now
//! holds only the in-memory `ChallengeStore` the WS-RPC handlers depend on.

use std::collections::{BTreeSet, HashMap, VecDeque};

use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// How long a challenge nonce remains valid.
pub const CHALLENGE_TTL_SECS: u64 = 300;

/// The most challenges one source may hold outstanding (issued, neither
/// consumed nor expired) in one store. A resource bound nobody chooses, so a
/// constant (`principles.md` § One configuration surface).
///
/// Issuance mints an in-memory entry for any valid Ed25519 point, registered or
/// not, so without a store bound one source could fill memory at whatever rate
/// it reaches the handler (`deployment-threat-model.md` § Concern 1a, finding
/// H2). The dispatcher's per-source throttle bounds the issuance *rate*; this
/// bounds what a source can *hold*, whatever reaches the store — a source the
/// throttle admits (an authenticated caller's actor bucket, the shared NAT
/// bucket) included. A sign-in consumes its nonce within seconds, so only
/// abandoned challenges count, and 128 of those from one address inside a
/// five-minute TTL is far past any real client population.
#[cfg(not(feature = "test-hooks"))]
pub const CHALLENGE_PER_SOURCE_CAP: usize = 128;

/// The harness cap: every e2e client is `127.0.0.1` against one session-scoped
/// nest, so a per-source cap over that single source is a cap per run — the
/// argument `anonymous_rate_limit::DISCOVERY_MAX_EVENTS` makes for its own
/// harness budget. tier_4 runs the real image and meets the shipped 128; the
/// mechanism stays proven at a small cap by `tests/pre_identity_budget.rs`,
/// which builds its store `with_caps`.
#[cfg(feature = "test-hooks")]
pub const CHALLENGE_PER_SOURCE_CAP: usize = 100_000;

/// The most challenges one store holds outstanding across every source — the
/// memory bound a distributed flood (or an IPv6 holder of many /64s) meets
/// after the per-source cap. Each entry is under 200 bytes with its map and
/// queue overhead, so the cap holds a store under ~20 MiB.
///
/// It bounds memory, never admission: a full store evicts the oldest challenge
/// of the network holding the most ([`IssueSource::network`]) and issues, so a
/// flood spread over every /64 of one /48 (or any number of sources) costs a
/// light source neither a fresh challenge nor, while a heavier network holds
/// more, its outstanding one. Expired entries count against no cap:
/// `issue` sweeps them first.
pub const CHALLENGE_GLOBAL_CAP: usize = 100_000;

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Who a challenge is charged to for [`CHALLENGE_PER_SOURCE_CAP`] — the same
/// discriminant the dispatcher's anonymous throttle keys on
/// (`anonymous_rate_limit::check_conn`): the client address for an anonymous
/// connection (IPv6 masked to its /64, so rotating host bits buys nothing), the
/// actor for an authenticated one, which records no peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IssueSource {
    Peer([u8; 32]),
    Actor([u8; 32]),
    /// No peer and no actor: an anonymous connection without `ConnectInfo`,
    /// which only unit and integration harnesses build. One shared bucket.
    Unattributed,
}

impl IssueSource {
    /// The source of the currently-running handler's caller, given the actor
    /// the dispatcher handed it (the zero placeholder on an anonymous
    /// connection).
    pub fn of_current_caller(actor: &[u8; 32]) -> Self {
        if let Some(ip) = crate::dispatch_core::current_caller().and_then(|c| c.peer_ip) {
            IssueSource::Peer(crate::anonymous_rate_limit::ip_key_bytes(ip))
        } else if actor != &[0u8; 32] {
            IssueSource::Actor(*actor)
        } else {
            IssueSource::Unattributed
        }
    }

    /// The network a source belongs to for the global cap's eviction: an IPv6
    /// peer's /48 (one end site's whole allocation — a tunnel broker hands
    /// one out for free), an IPv4 peer's own address (its key holds only the
    /// four octets), and any other source itself.
    pub fn network(self) -> Self {
        match self {
            IssueSource::Peer(mut key) => {
                key[6..].fill(0);
                IssueSource::Peer(key)
            }
            other => other,
        }
    }
}

/// [`ChallengeStore::issue`] refused: the source holds its cap. Surfaced as
/// `fauna.protocol.rate_limited` — the caller should retry later, when its
/// outstanding challenges have been consumed or expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChallengeStoreFull;

/// The WS-RPC refusal for a full ceremony nonce store — the dispatcher's own
/// throttle code. The refusal turns on the caller's own outstanding
/// challenges, never an account's, so it discloses nothing an unconditional
/// issue keeps private.
pub fn store_full(_: ChallengeStoreFull) -> fauna_protocol::RpcError {
    fauna_protocol::RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited")
}

struct ChallengeEntry {
    actor_id: [u8; 32],
    expires_at: u64,
    source: IssueSource,
}

/// One network's outstanding challenges (see [`IssueSource::network`]).
#[derive(Default)]
struct NetworkEntries {
    live: usize,
    /// Its nonces in issue order, oldest first. Removal is lazy: a consumed or
    /// swept nonce stays here until it reaches the front or a compaction drops
    /// it, so every insert and removal is O(1) amortized.
    order: VecDeque<[u8; 32]>,
}

#[derive(Default)]
struct Entries {
    /// nonce -> the challenge it was issued for (owning actor + expiry).
    ///
    /// Keyed by the random nonce, **not** the actor: two clients of one actor
    /// (the same identity launching in two tabs / on two devices) that request
    /// a challenge concurrently each get their own entry, so neither clobbers
    /// the other's outstanding nonce. The nonce is 256 random bits, so a
    /// cross-actor key collision is not a practical concern; `consume`
    /// re-checks the owning actor regardless.
    by_nonce: HashMap<[u8; 32], ChallengeEntry>,
    /// Outstanding entries per source, kept in step with `by_nonce` by every
    /// insert and removal so the cap check is O(1) under a flood.
    per_source: HashMap<IssueSource, usize>,
    /// Outstanding entries per network, and the networks ordered by that count,
    /// so the heaviest is found in O(log n) when the store is full.
    per_network: HashMap<IssueSource, NetworkEntries>,
    by_weight: BTreeSet<(usize, IssueSource)>,
    /// Every nonce in issue order — expiry order, the TTL being one constant —
    /// with the same lazy removal as [`NetworkEntries::order`].
    order: VecDeque<[u8; 32]>,
}

/// A lazily-pruned queue is compacted once its stale nonces outnumber its live
/// ones by this slack.
fn needs_compaction(queued: usize, live: usize) -> bool {
    queued > 2 * live + 64
}

impl Entries {
    fn insert(&mut self, nonce: [u8; 32], entry: ChallengeEntry) {
        let source = entry.source;
        let network = source.network();
        self.by_nonce.insert(nonce, entry);
        *self.per_source.entry(source).or_insert(0) += 1;
        let net = self.per_network.entry(network).or_default();
        self.by_weight.remove(&(net.live, network));
        net.live += 1;
        net.order.push_back(nonce);
        self.by_weight.insert((net.live, network));
        self.order.push_back(nonce);
    }

    fn remove(&mut self, nonce: &[u8; 32]) {
        let Some(e) = self.by_nonce.remove(nonce) else {
            return;
        };
        if let Some(n) = self.per_source.get_mut(&e.source) {
            *n -= 1;
            if *n == 0 {
                self.per_source.remove(&e.source);
            }
        }
        let network = e.source.network();
        if let Some(net) = self.per_network.get_mut(&network) {
            self.by_weight.remove(&(net.live, network));
            net.live -= 1;
            if net.live == 0 {
                self.per_network.remove(&network);
            } else {
                self.by_weight.insert((net.live, network));
                if needs_compaction(net.order.len(), net.live) {
                    let by_nonce = &self.by_nonce;
                    net.order.retain(|n| by_nonce.contains_key(n));
                }
            }
        }
        if needs_compaction(self.order.len(), self.by_nonce.len()) {
            let by_nonce = &self.by_nonce;
            self.order.retain(|n| by_nonce.contains_key(n));
        }
    }

    /// Drop every expired entry at the front of the issue order — dead to
    /// `consume` already, so nothing they hold counts against a cap.
    fn sweep_expired(&mut self, now: u64) {
        while let Some(front) = self.order.front().copied() {
            match self.by_nonce.get(&front) {
                Some(e) if e.expires_at > now => break,
                Some(_) => {
                    self.order.pop_front();
                    self.remove(&front);
                }
                None => {
                    self.order.pop_front();
                }
            }
        }
    }

    /// Evict the oldest outstanding challenge of the network holding the most.
    fn evict_from_heaviest(&mut self) {
        let Some(&(_, network)) = self.by_weight.last() else {
            return;
        };
        let Some(net) = self.per_network.get_mut(&network) else {
            return;
        };
        let by_nonce = &self.by_nonce;
        let victim = loop {
            match net.order.pop_front() {
                Some(front) if by_nonce.contains_key(&front) => break front,
                Some(_) => {}
                None => return,
            }
        };
        self.remove(&victim);
    }
}

pub struct ChallengeStore {
    entries: RwLock<Entries>,
    per_source_cap: usize,
    global_cap: usize,
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self::with_caps(CHALLENGE_PER_SOURCE_CAP, CHALLENGE_GLOBAL_CAP)
    }
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store with explicit caps — the shipped ones are [`Self::new`]'s; tests
    /// build small ones to reach the bound in a handful of calls.
    pub fn with_caps(per_source_cap: usize, global_cap: usize) -> Self {
        Self {
            entries: RwLock::new(Entries::default()),
            per_source_cap,
            global_cap,
        }
    }

    /// Issue a fresh nonce for `actor_id`, charged to `source`. Returns
    /// `(nonce, expires_at)`, or [`ChallengeStoreFull`] when `source` already
    /// holds [`CHALLENGE_PER_SOURCE_CAP`] unexpired challenges — a refusal
    /// writes nothing. A store at [`CHALLENGE_GLOBAL_CAP`] evicts the heaviest
    /// network's oldest challenge to make room.
    ///
    /// Concurrent issues for the same actor coexist (the store keys by the
    /// random nonce), each valid until consumed or TTL-expired.
    pub async fn issue(
        &self,
        actor_id: [u8; 32],
        source: IssueSource,
    ) -> Result<([u8; 32], u64), ChallengeStoreFull> {
        let now = now_secs();
        let mut map = self.entries.write().await;
        map.sweep_expired(now);
        if map.per_source.get(&source).copied().unwrap_or(0) >= self.per_source_cap {
            return Err(ChallengeStoreFull);
        }
        if map.by_nonce.len() >= self.global_cap {
            map.evict_from_heaviest();
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).expect("getrandom failed");
        let expires_at = now + CHALLENGE_TTL_SECS;
        map.insert(
            nonce,
            ChallengeEntry {
                actor_id,
                expires_at,
                source,
            },
        );
        Ok((nonce, expires_at))
    }

    /// Consume the `expected` nonce, removing it. Returns `true` only if it is
    /// still valid (unexpired) and was issued to `actor_id`.
    ///
    /// A mismatched actor or an unknown/expired nonce returns `false` **without
    /// burning** a valid entry — only a full match removes the nonce, keeping
    /// it single-use. (In production `consume` is only reached after `verify_core`
    /// has checked the Ed25519 signature over the tagged
    /// `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`, so the actor re-check is
    /// defense-in-depth.)
    pub async fn consume(&self, actor_id: &[u8; 32], expected: &[u8; 32]) -> bool {
        let mut map = self.entries.write().await;
        let valid = matches!(
            map.by_nonce.get(expected),
            Some(e) if e.expires_at > now_secs() && &e.actor_id == actor_id
        );
        if valid {
            map.remove(expected);
        }
        valid
    }

    /// Drop nonces whose TTL expired more than 60 seconds ago.
    pub async fn gc(&self) -> usize {
        let cutoff = now_secs().saturating_sub(60);
        let mut map = self.entries.write().await;
        let expired: Vec<[u8; 32]> = map
            .by_nonce
            .iter()
            .filter(|(_, e)| e.expires_at <= cutoff)
            .map(|(nonce, _)| *nonce)
            .collect();
        for nonce in &expired {
            map.remove(nonce);
        }
        expired.len()
    }

    /// Outstanding entries — what the caps bound.
    pub async fn len(&self) -> usize {
        self.entries.read().await.by_nonce.len()
    }

    /// `true` when no challenge is outstanding.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // verify-ok(test): this module signs with a locally generated key and
    // checks its own signature back — no wire-supplied key reaches it, so the
    // permissive trait is harmless here. Production verification goes through
    // `fauna_core::identity::verify_detached`; the walk guard
    // `state::tests::nest_has_one_ed25519_verification_shape` enforces that and
    // reads this marker.
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

    const SRC: IssueSource = IssueSource::Peer([0x5e; 32]);

    #[tokio::test]
    async fn issue_then_consume_once() {
        let store = ChallengeStore::new();
        let actor = [0xaa; 32];
        let (nonce, _) = store.issue(actor, SRC).await.unwrap();
        assert!(store.consume(&actor, &nonce).await);
        // second consume fails — one-shot
        assert!(!store.consume(&actor, &nonce).await);
    }

    #[tokio::test]
    async fn consume_wrong_nonce_fails() {
        let store = ChallengeStore::new();
        let actor = [0xbb; 32];
        let (_real, _) = store.issue(actor, SRC).await.unwrap();
        let fake = [0u8; 32];
        assert!(!store.consume(&actor, &fake).await);
    }

    #[tokio::test]
    async fn concurrent_issue_same_actor_both_consume() {
        // Two clients of the SAME actor each request a challenge before either
        // verifies (e.g. the same identity launching in two tabs / on two
        // devices). Both nonces must remain independently consumable — neither
        // client's challenge may clobber the other's. The store keys by the
        // random nonce, not the actor, so concurrent issues coexist.
        let store = ChallengeStore::new();
        let actor = [0xdd; 32];
        let (n1, _) = store.issue(actor, SRC).await.unwrap();
        let (n2, _) = store.issue(actor, SRC).await.unwrap();
        assert_ne!(n1, n2);
        assert!(
            store.consume(&actor, &n1).await,
            "first client's nonce clobbered"
        );
        assert!(
            store.consume(&actor, &n2).await,
            "second client's nonce clobbered"
        );
    }

    #[tokio::test]
    async fn consume_rejects_mismatched_actor() {
        // A nonce issued to actor A cannot be consumed by a verify claiming
        // actor B (defense-in-depth: consume re-checks the owning actor now
        // that the store keys by nonce). The mismatch must NOT burn the nonce —
        // actor A can still use it.
        let store = ChallengeStore::new();
        let actor_a = [0xa1; 32];
        let actor_b = [0xb2; 32];
        let (nonce, _) = store.issue(actor_a, SRC).await.unwrap();
        assert!(!store.consume(&actor_b, &nonce).await);
        assert!(store.consume(&actor_a, &nonce).await);
    }

    #[test]
    fn signature_roundtrip() {
        let seed = [7u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let pk = sk.verifying_key().to_bytes();

        let nonce = [42u8; 32];
        let mut msg = Vec::with_capacity(64);
        msg.extend_from_slice(&pk);
        msg.extend_from_slice(&nonce);
        let sig = sk.sign(&msg);

        let vk = VerifyingKey::from_bytes(&pk).unwrap();
        assert!(vk.verify(&msg, &sig).is_ok());
    }

    #[tokio::test]
    async fn gc_respects_the_sixty_second_grace_period() {
        // No prior test called gc() directly — insert entries with a
        // controlled expires_at (rather than waiting out the real 300s TTL)
        // so this exercises the sweep, and its
        // grace-period cutoff specifically, without a wall-clock wait.
        let store = ChallengeStore::new();
        let actor = [0xee; 32];
        {
            let mut map = store.entries.write().await;
            // Expired 120s ago — past the 60s grace window.
            insert_raw(&mut map, [1u8; 32], actor, now_secs().saturating_sub(120));
            // Expired 10s ago — still inside the 60s grace window.
            insert_raw(&mut map, [2u8; 32], actor, now_secs().saturating_sub(10));
            insert_raw(&mut map, [3u8; 32], actor, now_secs() + 300);
        }
        assert_eq!(store.gc().await, 1);
        let map = store.entries.read().await;
        assert_eq!(map.by_nonce.len(), 2);
        assert!(!map.by_nonce.contains_key(&[1u8; 32]));
        // The per-source count follows the removal.
        assert_eq!(map.per_source.get(&SRC), Some(&2));
    }

    /// Insert an entry with a chosen expiry, through the bookkeeping `issue`
    /// uses.
    fn insert_raw(map: &mut Entries, nonce: [u8; 32], actor_id: [u8; 32], expires_at: u64) {
        map.insert(
            nonce,
            ChallengeEntry {
                actor_id,
                expires_at,
                source: SRC,
            },
        );
    }

    #[tokio::test]
    async fn one_source_is_refused_past_its_cap_and_others_are_not() {
        let store = ChallengeStore::with_caps(3, 100);
        let flooder = IssueSource::Peer([0xf1; 32]);
        for i in 0..3u8 {
            assert!(store.issue([i; 32], flooder).await.is_ok());
        }
        // Any key, registered or not — the cap is on the source, not the actor.
        assert_eq!(
            store.issue([0x99; 32], flooder).await,
            Err(ChallengeStoreFull)
        );
        assert_eq!(store.len().await, 3, "a refusal writes nothing");
        // A different source is unaffected by the flooder's cap.
        assert!(store.issue([0x42; 32], SRC).await.is_ok());
    }

    #[tokio::test]
    async fn the_global_cap_bounds_the_store_by_eviction() {
        let store = ChallengeStore::with_caps(10, 4);
        let heavy = IssueSource::Peer([0x11; 32]);
        let (oldest, _) = store.issue([0; 32], heavy).await.unwrap();
        let (newer, _) = store.issue([0; 32], heavy).await.unwrap();
        for i in 2..4u8 {
            assert!(
                store
                    .issue([i; 32], IssueSource::Peer([i; 32]))
                    .await
                    .is_ok()
            );
        }
        let (fresh, _) = store
            .issue([0x77; 32], IssueSource::Peer([0x77; 32]))
            .await
            .expect("a full store evicts rather than refuses");
        assert_eq!(store.len().await, 4);
        // The victim is the heaviest network's OLDEST challenge.
        assert!(!store.consume(&[0; 32], &oldest).await);
        assert!(store.consume(&[0; 32], &newer).await);
        assert!(store.consume(&[0x77; 32], &fresh).await);
    }

    #[tokio::test]
    async fn consuming_frees_the_sources_slot() {
        // The cap counts OUTSTANDING challenges: a sign-in that consumes its
        // nonce never accumulates against it.
        let store = ChallengeStore::with_caps(1, 100);
        let actor = [0xc1; 32];
        let (nonce, _) = store.issue(actor, SRC).await.unwrap();
        assert_eq!(store.issue(actor, SRC).await, Err(ChallengeStoreFull));
        assert!(store.consume(&actor, &nonce).await);
        assert!(store.issue(actor, SRC).await.is_ok());
        assert_eq!(store.entries.read().await.per_source.get(&SRC), Some(&1));
    }

    #[tokio::test]
    async fn a_many_slash64_flood_under_one_slash48_leaves_room_for_an_honest_source() {
        // One IPv6 /48 holds 65 536 /64s, each its own per-source
        // bucket, so the global cap must not be a pool one network can own.
        let store = ChallengeStore::with_caps(3, 12);
        let mut flood = Vec::new();
        for sub in 0..8u8 {
            let mut key = [0u8; 32];
            key[..6].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0x00, 0x01]);
            key[7] = sub; // distinct /64s under 2001:db8:1::/48
            for i in 0..3u8 {
                flood.push(store.issue([i; 32], IssueSource::Peer(key)).await);
            }
        }
        assert!(
            flood.iter().all(Result::is_ok),
            "the flood is evicted, not refused"
        );
        assert_eq!(store.len().await, 12);
        let honest = IssueSource::Peer([0x5e; 32]);
        let (nonce, _) = store
            .issue([0xab; 32], honest)
            .await
            .expect("an honest source still gets a challenge");
        assert_eq!(store.len().await, 12, "the store stays at its cap");
        assert!(store.consume(&[0xab; 32], &nonce).await);
    }

    #[tokio::test]
    async fn an_outstanding_honest_challenge_survives_a_flood_past_the_cap() {
        let store = ChallengeStore::with_caps(3, 12);
        let actor = [0xab; 32];
        let (nonce, _) = store.issue(actor, SRC).await.unwrap();
        for sub in 0..40u8 {
            let mut key = [0u8; 32];
            key[..6].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0x00, 0x02]);
            key[7] = sub;
            for i in 0..3u8 {
                let _ = store.issue([i; 32], IssueSource::Peer(key)).await;
            }
        }
        assert_eq!(store.len().await, 12);
        assert!(
            store.consume(&actor, &nonce).await,
            "the flood evicted the light source's challenge"
        );
    }

    #[tokio::test]
    async fn expired_entries_do_not_count_against_the_caps() {
        let store = ChallengeStore::with_caps(2, 4);
        {
            let mut map = store.entries.write().await;
            // Expired a second ago — dead to `consume`, still awaiting gc.
            for n in 0..4u8 {
                insert_raw(&mut map, [n; 32], [0xee; 32], now_secs().saturating_sub(1));
            }
        }
        // SRC holds two expired entries, at its per-source cap of 2, and the
        // store is at its global cap of 4 — both counts are dead entries.
        assert!(store.issue([0xab; 32], SRC).await.is_ok());
        assert!(store.issue([0xab; 32], SRC).await.is_ok());
        assert_eq!(store.len().await, 2, "the expired entries were swept");
        assert_eq!(store.entries.read().await.per_source.get(&SRC), Some(&2));
    }

    #[test]
    fn a_peers_network_is_its_slash48_or_its_ipv4_address() {
        let v6 = |b7: u8| {
            let mut k = [0u8; 32];
            k[..8].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 1, 0, b7]);
            IssueSource::Peer(k)
        };
        assert_eq!(v6(1).network(), v6(2).network());
        let v4 = |last: u8| {
            let mut k = [0u8; 32];
            k[..4].copy_from_slice(&[192, 0, 2, last]);
            IssueSource::Peer(k)
        };
        assert_ne!(v4(1).network(), v4(2).network());
        assert_eq!(v4(1).network(), v4(1));
        let actor = IssueSource::Actor([0x77; 32]);
        assert_eq!(actor.network(), actor);
    }
}
