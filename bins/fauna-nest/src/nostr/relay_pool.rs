//! The Nostr sync worker's relay connections — and what a relay that never
//! answers may cost the worker.
//!
//! The worker is one loop for every account on the nest, and every relay it
//! dials is caller-supplied (a publish list, a follow's hints). A relay that
//! accepts and then stays silent is therefore something any user of a shared
//! nest can put in front of it. Four things keep that cost bounded, and the
//! ruling that they suffice is `nest/network-exposure.md` § Rulings F8:
//!
//! - each dial is bounded by [`RELAY_CONNECT_TIMEOUT`] inside the lib;
//! - a pass dials every relay it still lacks **at once** ([`RelayPool::connect_all`])
//!   and publishes to every relay **at once** ([`RelayPool::publish_all`]), so
//!   N silent relays cost one bound per pass, not N;
//! - every write on an open connection is bounded by `RELAY_SEND_TIMEOUT`
//!   inside the lib, and the worker's inbound drain gives each
//!   relay one `INBOUND_DRAIN_SLICE` per pass;
//! - a relay whose dial, publish or other write failed is not tried again until its
//!   backoff lapses — [`RELAY_BACKOFF_INITIAL`], doubling to
//!   [`RELAY_BACKOFF_MAX`] — so a tarpit that stays on a list costs at most
//!   one bound per backoff window, a window that keeps growing.

use std::collections::HashMap;
use std::time::Duration;

#[cfg(doc)]
use fauna_bridge_nostr::relay_client::RELAY_CONNECT_TIMEOUT;
use fauna_bridge_nostr::relay_client::{RelayClient, RelayDialPolicy};
use fauna_bridge_nostr::types::Event;
use futures_util::future::join_all;
use tokio::time::Instant;

/// How long a relay whose dial or publish failed first sits out.
pub const RELAY_BACKOFF_INITIAL: Duration = Duration::from_secs(30);
/// The longest a failing relay sits out; also how long a relay must go
/// without failing before its backoff starts again from the initial wait.
pub const RELAY_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);

/// One failing relay's backoff: not dialed before `retry_at`; the next
/// failure waits twice `wait`, capped at [`RELAY_BACKOFF_MAX`].
#[derive(Debug, Clone, Copy)]
struct Backoff {
    retry_at: Instant,
    wait: Duration,
}

/// The worker's open relay connections, keyed by URL, plus the backoff of
/// every relay that recently failed.
pub struct RelayPool {
    connections: HashMap<String, RelayClient>,
    backoff: HashMap<String, Backoff>,
    policy: RelayDialPolicy,
}

impl RelayPool {
    pub fn new(policy: RelayDialPolicy) -> Self {
        Self {
            connections: HashMap::new(),
            backoff: HashMap::new(),
            policy,
        }
    }

    /// Dial, concurrently, every URL in `urls` that is neither connected nor
    /// cooling down. A failed dial starts (or extends) that relay's backoff.
    pub async fn connect_all<'u>(&mut self, urls: impl IntoIterator<Item = &'u String>) {
        let now = Instant::now();
        self.backoff
            .retain(|_, b| b.retry_at + RELAY_BACKOFF_MAX > now);
        let mut wanted: Vec<&String> = urls
            .into_iter()
            .filter(|u| !self.connections.contains_key(*u) && !self.cooling_down(u, now))
            .collect();
        wanted.sort();
        wanted.dedup();
        let policy = self.policy;
        let dials = wanted
            .into_iter()
            .map(|url| async move { (url.clone(), RelayClient::connect(url, policy).await) });
        for (url, result) in join_all(dials).await {
            match result {
                Ok(client) => {
                    tracing::info!("nostr sync: connected to {url}");
                    self.connections.insert(url, client);
                }
                Err(e) => {
                    tracing::warn!("nostr sync: failed to connect to {url}: {e}");
                    self.note_failure(&url);
                }
            }
        }
    }

    /// Publish `event` to every connected relay among `urls`, concurrently —
    /// each publish already bounded by [`RelayClient::publish`]'s own wait for
    /// an `OK`. A publish that errs (a timeout included) drops the connection
    /// and starts the relay's backoff; any `OK` reply clears it. Returns each
    /// relay's outcome for the caller to report.
    pub async fn publish_all(
        &mut self,
        urls: &[String],
        event: &Event,
    ) -> Vec<(String, anyhow::Result<bool>)> {
        let publishes = self
            .connections
            .iter_mut()
            .filter(|(url, _)| urls.contains(url))
            .map(|(url, client)| async move { (url.clone(), client.publish(event.clone()).await) });
        let outcomes = join_all(publishes).await;
        for (url, outcome) in &outcomes {
            match outcome {
                Ok(_) => {
                    self.backoff.remove(url);
                }
                Err(_) => {
                    self.connections.remove(url);
                    self.note_failure(url);
                }
            }
        }
        outcomes
    }

    pub fn get_mut(&mut self, url: &str) -> Option<&mut RelayClient> {
        self.connections.get_mut(url)
    }

    /// Drop a connection that closed or errored; the next pass may redial it.
    pub fn remove(&mut self, url: &str) {
        self.connections.remove(url);
    }

    /// Drop a connection whose write failed — a relay that stopped reading
    /// times out in `RelayClient::send` — and start its backoff,
    /// as a failed publish does, so it is not redialed until that lapses.
    pub fn drop_failed(&mut self, url: &str) {
        self.connections.remove(url);
        self.note_failure(url);
    }

    /// The URLs of every open connection.
    pub fn urls(&self) -> Vec<String> {
        self.connections.keys().cloned().collect()
    }

    /// Is `url` sitting out a backoff right now?
    pub fn cooling_down(&self, url: &str, now: Instant) -> bool {
        self.backoff.get(url).is_some_and(|b| now < b.retry_at)
    }

    fn note_failure(&mut self, url: &str) {
        let now = Instant::now();
        let wait = match self.backoff.get(url) {
            // Failing again within the window it was last allowed back:
            // double the wait.
            Some(b) if now < b.retry_at + RELAY_BACKOFF_MAX => (b.wait * 2).min(RELAY_BACKOFF_MAX),
            _ => RELAY_BACKOFF_INITIAL,
        };
        self.backoff.insert(
            url.to_string(),
            Backoff {
                retry_at: now + wait,
                wait,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_bridge_nostr::relay_client::RELAY_CONNECT_TIMEOUT;
    use fauna_bridge_nostr::relay_client::test_support::ProbeListener;

    fn pool() -> RelayPool {
        RelayPool::new(RelayDialPolicy::PublicOrLoopback)
    }

    /// The backoff doubles while a relay keeps failing, stops at the cap, and
    /// starts over once the relay has gone a full cap without failing.
    #[tokio::test(start_paused = true)]
    async fn a_failing_relay_backs_off_doubling_to_the_cap() {
        let mut p = pool();
        let url = "ws://relay.example/";
        let mut want = RELAY_BACKOFF_INITIAL;
        for _ in 0..8 {
            p.note_failure(url);
            let now = Instant::now();
            assert!(p.cooling_down(url, now));
            assert!(p.cooling_down(url, now + want - Duration::from_millis(1)));
            assert!(!p.cooling_down(url, now + want), "want {want:?}");
            tokio::time::advance(want).await;
            want = (want * 2).min(RELAY_BACKOFF_MAX);
        }
        assert_eq!(p.backoff[url].wait, RELAY_BACKOFF_MAX);
        // A full cap with no failure: the next one starts from the beginning.
        tokio::time::advance(RELAY_BACKOFF_MAX).await;
        p.note_failure(url);
        assert_eq!(p.backoff[url].wait, RELAY_BACKOFF_INITIAL);
    }

    /// A connection dropped for a failed write sits out its backoff;
    /// one dropped for a close does not.
    #[tokio::test]
    async fn a_failed_write_starts_the_backoff_and_a_close_does_not() {
        let mut p = pool();
        p.drop_failed("ws://deaf.example/");
        p.remove("ws://closed.example/");
        let now = Instant::now();
        assert!(p.cooling_down("ws://deaf.example/", now));
        assert!(!p.cooling_down("ws://closed.example/", now));
    }

    /// **Two silent relays cost one bound, then nothing.** Each probe
    /// completes TCP from its backlog and never answers the upgrade. One pass
    /// dials both at once, so it returns in about one [`RELAY_CONNECT_TIMEOUT`]
    /// (a serial dial would take two); both relays then sit out their backoff,
    /// so the next pass returns at once and opens no socket to either.
    #[tokio::test]
    async fn silent_relays_are_dialed_together_then_sat_out() {
        let a = ProbeListener::bind().await;
        let b = ProbeListener::bind().await;
        let urls = vec![a.url.clone(), b.url.clone()];
        let mut p = pool();

        let started = std::time::Instant::now();
        p.connect_all(&urls).await;
        let took = started.elapsed();
        assert!(
            took < RELAY_CONNECT_TIMEOUT + RELAY_CONNECT_TIMEOUT / 2,
            "the dials must run together: {took:?}"
        );
        assert!(p.urls().is_empty(), "a silent relay yields no connection");
        // Beside-control: both dials really reached their listener.
        let short = Duration::from_millis(300);
        assert!(a.saw_a_connection_within(short).await);
        assert!(b.saw_a_connection_within(short).await);

        let started = std::time::Instant::now();
        p.connect_all(&urls).await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!a.saw_a_connection_within(short).await, "a is sitting out");
        assert!(!b.saw_a_connection_within(short).await, "b is sitting out");
    }

    /// A loopback WebSocket relay that completes the handshake and then never
    /// says anything — no `OK` to a publish.
    async fn mute_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        // spawn-ok(test)
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            std::future::pending::<()>().await;
        });
        url
    }

    /// **Two relays that never `OK` cost one publish wait, then sit out.** The
    /// publishes run together, each times out, and each relay is dropped into
    /// its backoff so the next event does not wait on it again.
    #[tokio::test]
    async fn mute_relays_are_published_to_together_then_sat_out() {
        let urls = vec![mute_relay().await, mute_relay().await];
        let mut p = pool();
        p.connect_all(&urls).await;
        assert_eq!(p.urls().len(), 2, "both handshakes complete");

        let keypair = fauna_bridge_nostr::signing::Keypair::generate();
        let event = keypair.sign_event(fauna_bridge_nostr::types::UnsignedEvent {
            pubkey: keypair.public_key_bytes(),
            created_at: 1,
            kind: 1,
            tags: vec![],
            content: "hello".to_string(),
        });
        let started = std::time::Instant::now();
        let outcomes = p.publish_all(&urls, &event).await;
        let took = started.elapsed();
        // `publish`'s own OK wait is the same 10 s as the connect bound.
        assert!(
            took < RELAY_CONNECT_TIMEOUT + RELAY_CONNECT_TIMEOUT / 2,
            "the publishes must run together: {took:?}"
        );
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|(_, o)| o.is_err()));
        assert!(p.urls().is_empty(), "a mute relay's connection is dropped");
        let now = Instant::now();
        assert!(urls.iter().all(|u| p.cooling_down(u, now)));
    }
}
