//! The `fauna.nest.info` reply, read **once per connection** for the readers
//! that may share it (`security.md` § Transport trust → *The nest's
//! self-description is read once per connection*).
//!
//! A settings landing renders the node leg (domain + version), the
//! feature-limits rows and the own-limits surface, and an app checks for a
//! newer release once at sign-in; all four fold the same `fauna.nest.info`
//! reply. Read once per leg, a sign-in drew four of the actor's pre-identity
//! discovery budget (`anonymous_rate_limit::DISCOVERY_MAX_EVENTS`, 60 events
//! per 60 s on the nest) on one answer, on every app. The transport clients
//! (native `NestClient`, wasm `WsRpcClient`) each own one [`NestInfoMemo`] and
//! expose it through [`RpcRequester::nest_info_memo`]; [`read_nest_info`] is
//! the one read every sharing reader goes through, so the memo lives below the
//! apps and all seven inherit it.
//!
//! **Which readers may share it.** A reader whose answer is fixed for the life
//! of a connection, or is read once per connection anyway:
//! - the node leg (`domain`, `version` — a nest restart is a reconnect);
//! - the feature-limits capability set (`capabilities`; its one runtime-gated
//!   token, `relay`, gates no feature-limits row —
//!   `fauna_client_features::capability_token`);
//! - the sign-in update look (`newest_release`, read once per sign-in);
//! - the capability probes (`nest_supports` in `fauna-client-subscriptions`
//!   and the mail-settings seam) and the follow intent's home-nest id
//!   (`nest_id`).
//!
//! A reader that needs `newest_release` or `relay` as they are *now*, mid
//! connection (an admin surface polling for an update), reads
//! `fauna.nest.info` itself and does not go through here.

use crate::RpcRequester;
use crate::discovery::{NestInfoReply, NestInfoRequest};

/// One connection's `fauna.nest.info` reply, remembered for that connection
/// and no other — keyed on the connection's
/// [`crate::RpcDispatcher::connection_id`], minted per (re)connect, exactly as
/// [`crate::ProvenIdentityMemo`] is. A reply read over connection N is never
/// served on connection N+1, and a read that straddled a reconnect remembers
/// nothing.
///
/// The slot is an async lock held across the read, so concurrent readers on
/// one connection (a landing loading its legs in parallel) coalesce onto the
/// first one's round trip instead of each missing and reading.
#[derive(Debug, Default)]
pub struct NestInfoMemo {
    slot: futures_util::lock::Mutex<Option<(u64, NestInfoReply)>>,
}

impl NestInfoMemo {
    /// An empty memo — nothing read on any connection yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// `fauna.nest.info` over `nest`, served from the transport's
/// [`NestInfoMemo`] when the live connection has already read it (module docs
/// for which readers may). A transport without a memo (a fake, a port-built
/// web client, a wrapper that does not forward it) reads on every call, as
/// before. An error is never remembered.
pub async fn read_nest_info<R: RpcRequester + ?Sized>(nest: &R) -> Result<NestInfoReply, R::Error> {
    let Some(memo) = nest.nest_info_memo() else {
        return nest
            .request("fauna.nest.info", NestInfoRequest::default())
            .await;
    };
    // Ticketed once a connection is live: a reader issued in the gap before
    // the first connect waits for it, as its read would, so the reply is
    // remembered for the connection it rides. The wait is taken BEFORE the
    // slot: with the nest down, readers queued behind one another's bounded
    // waits would hold a landing for one wait per leg, where side by side
    // they settle within one. A reader that found no connection has nothing
    // to share or remember, so it reads (and fails) without the slot.
    let Some(ticket) = nest.ready_connection_id().await else {
        return nest
            .request("fauna.nest.info", NestInfoRequest::default())
            .await;
    };
    let mut slot = memo.slot.lock().await;
    if let Some((connection, reply)) = slot.as_ref()
        && *connection == ticket
    {
        return Ok(reply.clone());
    }
    let reply: NestInfoReply = nest
        .request("fauna.nest.info", NestInfoRequest::default())
        .await?;
    *slot = (nest.live_connection_id() == Some(ticket)).then(|| (ticket, reply.clone()));
    Ok(reply)
}

/// Why an act that may cross nests failed on the client: the nest's own
/// error, or a signed intent this client could not mint (its nest names no
/// well-formed id). The follow (`fauna-client-subscriptions`'s `FollowError`)
/// and the claim redeem (`fauna-client-payments`) share it.
#[derive(Debug)]
pub enum CrossNestError<E> {
    Nest(E),
    Intent(String),
}

impl<E: std::fmt::Display> std::fmt::Display for CrossNestError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nest(e) => e.fmt(f),
            Self::Intent(why) => write!(f, "signed intent: {why}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for CrossNestError<E> {}

/// An app-supplied home nest URL as the cross-nest wire carries it: trimmed,
/// without a trailing slash, `None` when blank (blank means this nest).
#[must_use]
pub fn normalize_home_nest_url(home: Option<String>) -> Option<String> {
    home.map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// The id of the nest this client is connected to, as `fauna.nest.info`
/// states it — the `home_nest_id` every cross-nest intent names (the follow's
/// `FollowIntent`, the redeem's `ClaimRedeemIntent`), which the nest compares
/// against its own key before relaying. Fixed for the life of a connection, so
/// it comes off the per-connection memo ([`read_nest_info`]).
pub async fn read_nest_id<R: RpcRequester + ?Sized>(
    nest: &R,
) -> Result<[u8; 32], CrossNestError<R::Error>> {
    let info = read_nest_info(nest).await.map_err(CrossNestError::Nest)?;
    fauna_core::hex32::decode(&info.nest_id).map_err(|e| {
        CrossNestError::Intent(format!("this nest's id is not a 32-byte hex key: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fake {
        memo: Option<NestInfoMemo>,
        connection: Mutex<Option<u64>>,
        reads: AtomicUsize,
        fail: AtomicBool,
        reconnect_during_read: AtomicBool,
        /// `ready_connection_id` brings connection 1 up, as a transport's
        /// in-gap wait does.
        connects_on_wait: AtomicBool,
        /// `ready_connection_id` parks this long and finds no connection — a
        /// nest that refuses every dial, as the transport's bounded in-gap
        /// wait sees it.
        down_wait: Option<std::time::Duration>,
    }

    impl Fake {
        fn with_memo(connection: u64) -> Self {
            Self {
                memo: Some(NestInfoMemo::new()),
                connection: Mutex::new(Some(connection)),
                ..Self::default()
            }
        }

        fn reconnect(&self) {
            let mut c = self.connection.lock().unwrap();
            *c = Some(c.map_or(1, |n| n + 1));
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl RpcRequester for Fake {
        type Error = String;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, String>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            assert_eq!(kind, "fauna.nest.info");
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            // Yield so a concurrent reader runs while this one is in flight.
            tokio::task::yield_now().await;
            if self.reconnect_during_read.load(Ordering::SeqCst) {
                self.reconnect();
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("down".into());
            }
            let reply = NestInfoReply {
                version: format!("read-{n}"),
                ..Default::default()
            };
            let bytes = crate::encode_canonical(&reply).map_err(|e| e.to_string())?;
            crate::decode_strict(&bytes).map_err(|e| e.to_string())
        }

        fn live_connection_id(&self) -> Option<u64> {
            *self.connection.lock().unwrap()
        }

        async fn ready_connection_id(&self) -> Option<u64> {
            if let Some(wait) = self.down_wait {
                tokio::time::sleep(wait).await;
                return None;
            }
            if self.connects_on_wait.load(Ordering::SeqCst) {
                let mut c = self.connection.lock().unwrap();
                c.get_or_insert(1);
            }
            *self.connection.lock().unwrap()
        }

        fn nest_info_memo(&self) -> Option<&NestInfoMemo> {
            self.memo.as_ref()
        }
    }

    /// A sign-in's readers start while the socket is still dialling: a read
    /// issued in that gap waits for the connection, as its request would, and
    /// is remembered for it — the readers after it are served, rather than
    /// each reading (measured 2026-11-02 on linux: the archive-import check's
    /// gap read was remembered for nothing, and the node leg read again).
    #[tokio::test]
    async fn a_read_issued_before_the_first_connect_is_remembered_for_it() {
        let nest = Fake {
            memo: Some(NestInfoMemo::new()),
            connects_on_wait: AtomicBool::new(true),
            ..Fake::default()
        };
        let (a, b) = tokio::join!(read_nest_info(&nest), read_nest_info(&nest));
        assert_eq!(a.unwrap(), b.unwrap());
        read_nest_info(&nest).await.unwrap();
        assert_eq!(nest.reads(), 1);
    }

    /// With the nest down, every reader's wait for a connection runs side by
    /// side, never queued behind the memo's slot: a settings landing whose
    /// legs each read `fauna.nest.info` settles within one in-gap wait, not
    /// one per leg (2026-11-03: tui's settings sub-page nav sat behind the
    /// queued waits and was never acknowledged).
    #[tokio::test(start_paused = true)]
    async fn readers_on_a_down_nest_wait_side_by_side() {
        let wait = std::time::Duration::from_secs(5);
        let nest = Fake {
            memo: Some(NestInfoMemo::new()),
            fail: AtomicBool::new(true),
            down_wait: Some(wait),
            ..Fake::default()
        };
        let start = tokio::time::Instant::now();
        let (a, b, c) = tokio::join!(
            read_nest_info(&nest),
            read_nest_info(&nest),
            read_nest_info(&nest)
        );
        assert!(a.is_err() && b.is_err() && c.is_err());
        let took = start.elapsed();
        assert!(
            took < wait * 2,
            "three readers on a down nest took {took:?}: their waits queued behind the memo"
        );
    }

    #[tokio::test]
    async fn one_connection_reads_once() {
        let nest = Fake::with_memo(1);
        let first = read_nest_info(&nest).await.unwrap();
        let second = read_nest_info(&nest).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(nest.reads(), 1);
    }

    #[tokio::test]
    async fn concurrent_readers_on_one_connection_share_one_read() {
        let nest = Fake::with_memo(1);
        let (a, b, c) = tokio::join!(
            read_nest_info(&nest),
            read_nest_info(&nest),
            read_nest_info(&nest)
        );
        assert_eq!(a.unwrap(), b.clone().unwrap());
        assert_eq!(b.unwrap(), c.unwrap());
        assert_eq!(nest.reads(), 1);
    }

    #[tokio::test]
    async fn a_reconnect_reads_afresh() {
        let nest = Fake::with_memo(1);
        let first = read_nest_info(&nest).await.unwrap();
        nest.reconnect();
        let second = read_nest_info(&nest).await.unwrap();
        assert_ne!(first, second);
        assert_eq!(nest.reads(), 2);
        // ... and the new connection's reply is the one remembered.
        assert_eq!(read_nest_info(&nest).await.unwrap(), second);
        assert_eq!(nest.reads(), 2);
    }

    #[tokio::test]
    async fn a_read_that_straddled_a_reconnect_is_not_remembered() {
        let nest = Fake::with_memo(1);
        nest.reconnect_during_read.store(true, Ordering::SeqCst);
        read_nest_info(&nest).await.unwrap();
        nest.reconnect_during_read.store(false, Ordering::SeqCst);
        read_nest_info(&nest).await.unwrap();
        assert_eq!(nest.reads(), 2);
    }

    #[tokio::test]
    async fn an_error_is_not_remembered() {
        let nest = Fake::with_memo(1);
        nest.fail.store(true, Ordering::SeqCst);
        assert!(read_nest_info(&nest).await.is_err());
        nest.fail.store(false, Ordering::SeqCst);
        assert!(read_nest_info(&nest).await.is_ok());
        assert_eq!(nest.reads(), 2);
    }

    #[tokio::test]
    async fn no_live_connection_reads_every_time() {
        let nest = Fake {
            memo: Some(NestInfoMemo::new()),
            ..Fake::default()
        };
        read_nest_info(&nest).await.unwrap();
        read_nest_info(&nest).await.unwrap();
        assert_eq!(nest.reads(), 2);
    }

    #[tokio::test]
    async fn a_transport_without_a_memo_reads_every_time() {
        let nest = Fake {
            connection: Mutex::new(Some(1)),
            ..Fake::default()
        };
        read_nest_info(&nest).await.unwrap();
        read_nest_info(&nest).await.unwrap();
        assert_eq!(nest.reads(), 2);
    }

    #[tokio::test]
    async fn the_memo_reaches_through_a_reference_and_an_arc() {
        let nest = std::sync::Arc::new(Fake::with_memo(1));
        read_nest_info(&nest).await.unwrap();
        read_nest_info(&&*nest).await.unwrap();
        assert_eq!(nest.reads(), 1);
    }
}
