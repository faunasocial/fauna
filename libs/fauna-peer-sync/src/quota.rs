//! Rule-8 quotas: per-peer, per-window connection and request bounds at the
//! shared listener (`p2p.md` § Wormability posture rule 8; the peer leg's
//! compliance record `account-data-plane.md` § Wormability walk rule 8).
//!
//! Both counters are **fixed-window** and keyed by the transport-proven peer
//! key, and both deliberately count *refused* work too: an admission-refused
//! attempt spends the same budget as an admitted one (the pre-auth DoS
//! bound), and all witness kinds meter alike. Time is injected
//! (epoch-seconds closure) so tests drive the windows without wall-clock
//! (e2e convention 14 applied at tier 1).
//!
//! The bounds are hard-coded Rust constants (defaults on [`QuotaConfig`]) —
//! no human ever chooses them (§ Product invariants: not a config surface).
//!
//! The connection half of the rule is also *applied* here, not just counted:
//! [`metered_handler_factory`] is the one listener door both peer planes hand
//! to `PeerNode::start_with`, so "metered by the same per-peer ledger" is a
//! property of the code rather than of two copies agreeing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fauna_peer_channel::{HandlerFactory, PeerHandlers};
use fauna_transport::EndpointKey;

/// Per-peer, per-window bounds. [`Default`] is the shipped constant set —
/// sized generously above any convergence workload (a replica's walk is a few
/// requests per sync round) while bounding a hostile peer's fan-out.
#[derive(Debug, Clone)]
pub struct QuotaConfig {
    /// New connections a single peer may open per window.
    pub conns_per_window: u32,
    /// Requests a single peer may issue per window, across all its
    /// connections, admission-refused attempts included.
    pub requests_per_window: u32,
    /// The fixed window, in seconds.
    pub window_secs: u64,
    /// The most distinct peer keys the ledger tracks at once — the memory
    /// bound on the map itself. The per-peer quotas bound one peer's
    /// fan-out; this bounds the *count* of peers a party minting cheap
    /// fresh keys can make the ledger remember (a handshake needs no
    /// witness, so insertion is pre-admission work).
    pub tracked_peers_cap: usize,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            conns_per_window: 32,
            requests_per_window: 4096,
            window_secs: 60,
            tracked_peers_cap: 4096,
        }
    }
}

/// One peer's counters inside the current window.
#[derive(Debug, Default, Clone, Copy)]
struct PeerWindow {
    window_start: u64,
    conns: u32,
    requests: u32,
}

/// The listener's shared ledger. Cheap interior mutability (one small map
/// under a mutex — every touch is O(1) and lock-scoped to a few loads).
#[derive(Debug)]
pub struct QuotaLedger {
    config: QuotaConfig,
    peers: Mutex<HashMap<[u8; 32], PeerWindow>>,
}

impl QuotaLedger {
    pub fn new(config: QuotaConfig) -> Self {
        Self {
            config,
            peers: Mutex::new(HashMap::new()),
        }
    }

    fn window_of<'a>(
        peers: &'a mut HashMap<[u8; 32], PeerWindow>,
        peer: &[u8; 32],
        now_secs: u64,
        config: &QuotaConfig,
    ) -> &'a mut PeerWindow {
        // The map is attacker-growable pre-admission (a QUIC handshake needs
        // no witness, and each fresh Ed25519 id is a fresh key), so a NEW
        // key arriving at the cap reclaims before inserting: fully-elapsed
        // windows go first — they carry no live quota state, so evicting one
        // is indistinguishable from the reset the peer's next touch would
        // apply anyway — and if the map is genuinely full of live windows,
        // the oldest (closest to its natural reset) goes. The O(n) scans are
        // paid only under cap pressure, i.e. only while being flooded.
        let cap = config.tracked_peers_cap.max(1);
        if !peers.contains_key(peer) && peers.len() >= cap {
            peers.retain(|_, w| now_secs.saturating_sub(w.window_start) < config.window_secs);
            while peers.len() >= cap {
                let Some(oldest) = peers
                    .iter()
                    .min_by_key(|(_, w)| w.window_start)
                    .map(|(k, _)| *k)
                else {
                    break;
                };
                peers.remove(&oldest);
            }
        }
        let w = peers.entry(*peer).or_default();
        if now_secs.saturating_sub(w.window_start) >= config.window_secs {
            *w = PeerWindow {
                window_start: now_secs,
                ..PeerWindow::default()
            };
        }
        w
    }

    /// Meter a new connection from `peer`. `true` = within bounds (counted);
    /// `false` = over quota — refuse the connection before any stream work.
    pub fn try_conn(&self, peer: &[u8; 32], now_secs: u64) -> bool {
        let mut peers = self.peers.lock().unwrap();
        let w = Self::window_of(&mut peers, peer, now_secs, &self.config);
        if w.conns >= self.config.conns_per_window {
            return false;
        }
        w.conns += 1;
        true
    }

    /// Meter one request from `peer` — called for **every** served kind, the
    /// admission exchange included (refused admissions spend budget too).
    pub fn try_request(&self, peer: &[u8; 32], now_secs: u64) -> bool {
        let mut peers = self.peers.lock().unwrap();
        let w = Self::window_of(&mut peers, peer, now_secs, &self.config);
        if w.requests >= self.config.requests_per_window {
            return false;
        }
        w.requests += 1;
        true
    }
}

/// A peer plane's serve side, as the shared connection-metering door needs to
/// see it.
///
/// The two planes — [`crate::server::PeerSyncServer`] and `fauna-peer-share`'s
/// `ShareServer` — present the listener the *same* behaviour (rule 8's
/// connection meter, then that connection's serve set) and differ only in
/// which kinds the set carries and which plane a refusal names. This trait is
/// that difference; [`metered_handler_factory`] is the shared rule.
///
/// Deliberately **not** a home for the serve-set builder: that stays a private
/// inherent fn on each server and is handed to the door as a function item, so
/// no consumer gains an *unmetered* way to build a plane's handlers.
pub trait MeteredPlane: Send + Sync + 'static {
    /// The plane's name on a refusal log line (`"peer-sync"`, `"peer-share"`).
    const PLANE: &'static str;

    /// The plane's injected clock, epoch-seconds — the same one its request
    /// meter and witness-validity checks read, so a test that drives one
    /// drives all of them (e2e convention 14 applied at tier 1).
    fn now_secs(&self) -> u64;

    /// The rule-8 ledger this plane meters against.
    fn ledger(&self) -> &QuotaLedger;
}

/// The per-connection handler factory `PeerNode::start_with` consumes, for any
/// [`MeteredPlane`]: meter the connection (rule 8 — **refused before any
/// stream work**, so a peer over quota never reaches a serve set at all), then
/// build that connection's handlers with `handlers`.
///
/// **Sides admit independently** (the admission seam): the verdict slot inside
/// those handlers is this side's verdict about the remote, whatever the remote
/// decided about us — which is why the slot is minted per connection, in
/// `handlers`, and never here.
///
/// One owner for both planes on purpose. `p2p.md` § *Wormability walk* rule 8
/// says every kind is metered by **the same** per-peer ledger, refused
/// attempts included; a second copy of this door is exactly how one plane's
/// meter stops matching the other's without any test noticing.
pub fn metered_handler_factory<P, F>(plane: &Arc<P>, handlers: F) -> HandlerFactory
where
    P: MeteredPlane,
    F: Fn(&Arc<P>, EndpointKey) -> PeerHandlers + Send + Sync + 'static,
{
    let plane = Arc::clone(plane);
    Arc::new(move |peer, _path| {
        let now = plane.now_secs();
        if !plane.ledger().try_conn(peer.as_bytes(), now) {
            tracing::warn!(peer = ?peer, plane = P::PLANE, "connection quota refused a peer");
            return None;
        }
        Some(handlers(&plane, peer))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(conns: u32, requests: u32, window: u64) -> QuotaLedger {
        QuotaLedger::new(QuotaConfig {
            conns_per_window: conns,
            requests_per_window: requests,
            window_secs: window,
            ..QuotaConfig::default()
        })
    }

    const PEER_A: [u8; 32] = [1; 32];
    const PEER_B: [u8; 32] = [2; 32];

    #[test]
    fn conns_are_bounded_per_peer_per_window_and_reset_on_window_roll() {
        let l = ledger(2, 100, 60);
        assert!(l.try_conn(&PEER_A, 0));
        assert!(l.try_conn(&PEER_A, 10));
        assert!(!l.try_conn(&PEER_A, 20), "third conn in-window refused");
        // Another peer's budget is its own.
        assert!(l.try_conn(&PEER_B, 20));
        // The window rolls on injected time — no wall clock anywhere.
        assert!(l.try_conn(&PEER_A, 60));
    }

    #[test]
    fn the_ledger_does_not_grow_without_bound_across_fresh_keys_and_window_rolls() {
        // The rule-8 map is keyed by the transport-proven peer key, and a
        // QUIC handshake needs no witness — so a hostile party can insert
        // entries from ever-fresh Ed25519 ids for free. The ledger must stay
        // O(active-window), never O(every-key-ever-seen).
        let l = QuotaLedger::new(QuotaConfig {
            conns_per_window: 2,
            requests_per_window: 100,
            window_secs: 60,
            tracked_peers_cap: 8,
        });
        let fresh = |n: u64| -> [u8; 32] {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&n.to_be_bytes());
            key
        };
        for round in 0u64..4 {
            for i in 0..1_000 {
                l.try_conn(&fresh(round * 1_000_000 + i), round * 60);
            }
        }
        let tracked = l.peers.lock().unwrap().len();
        assert!(
            tracked <= 8,
            "4,000 ever-seen keys must leave a capped ledger, got {tracked} entries"
        );

        // Eviction prefers fully-elapsed windows: an entry the roll already
        // reset carries no live quota state, so dropping it changes nothing
        // a peer could observe — while a still-active window survives cap
        // pressure from fresh keys where possible.
        let l = QuotaLedger::new(QuotaConfig {
            conns_per_window: 2,
            requests_per_window: 100,
            window_secs: 60,
            tracked_peers_cap: 4,
        });
        assert!(l.try_conn(&PEER_A, 0));
        assert!(l.try_conn(&PEER_A, 0));
        // PEER_A's window elapses; a flood of fresh keys fills the map.
        for i in 0..10 {
            l.try_conn(&fresh(100 + i), 61);
        }
        // A's elapsed window was evicted (not a live one) and its quota is
        // simply the fresh-window reset it would have gotten anyway.
        assert!(l.try_conn(&PEER_A, 62));
    }

    /// The connection meter's **door**, not just its counter. Two properties
    /// the ledger's own tests cannot see, because they are about what the
    /// listener does with the answer: a peer over the window is refused with
    /// `None` (so `PeerNode` never accepts it), and — rule 8's actual claim,
    /// *"refused before any stream work"* — the plane's serve set is never
    /// built for that connection, so a flooding peer buys one map touch and
    /// nothing more.
    #[test]
    fn the_door_refuses_an_over_quota_peer_without_building_its_serve_set() {
        use fauna_peer_channel::base_peer_handlers;
        use fauna_transport::PathKind;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct TestPlane {
            ledger: QuotaLedger,
            built: AtomicUsize,
        }

        impl MeteredPlane for TestPlane {
            const PLANE: &'static str = "test-plane";

            fn now_secs(&self) -> u64 {
                1_000
            }

            fn ledger(&self) -> &QuotaLedger {
                &self.ledger
            }
        }

        let plane = Arc::new(TestPlane {
            ledger: ledger(2, 8, 60),
            built: AtomicUsize::new(0),
        });
        let factory = metered_handler_factory(&plane, |p: &Arc<TestPlane>, _peer| {
            p.built.fetch_add(1, Ordering::SeqCst);
            base_peer_handlers("test-plane".into())
        });

        let a = EndpointKey::from_bytes(PEER_A);
        assert!(factory(a, PathKind::Lan).is_some());
        assert!(factory(a, PathKind::Lan).is_some());
        assert!(
            factory(a, PathKind::Lan).is_none(),
            "the third connection in-window is refused"
        );
        assert_eq!(
            plane.built.load(Ordering::SeqCst),
            2,
            "the refused connection must never have built a serve set"
        );

        // Per-peer, not global: a second peer arrives with its own window.
        assert!(factory(EndpointKey::from_bytes(PEER_B), PathKind::Lan).is_some());
        assert_eq!(plane.built.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn requests_are_bounded_per_peer_per_window() {
        let l = ledger(100, 3, 60);
        for t in 0..3 {
            assert!(l.try_request(&PEER_A, t));
        }
        assert!(!l.try_request(&PEER_A, 3));
        assert!(l.try_request(&PEER_B, 3), "peer budgets are independent");
        assert!(l.try_request(&PEER_A, 61), "window roll restores budget");
    }
}
