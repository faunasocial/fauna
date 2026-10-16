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
//! - the sign-in update look (`newest_release`, read once per sign-in).
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
    let mut slot = memo.slot.lock().await;
    let ticket = nest.live_connection_id();
    if let (Some(live), Some((connection, reply))) = (ticket, slot.as_ref())
        && *connection == live
    {
        return Ok(reply.clone());
    }
    let reply: NestInfoReply = nest
        .request("fauna.nest.info", NestInfoRequest::default())
        .await?;
    match ticket {
        Some(ticket) if nest.live_connection_id() == Some(ticket) => {
            *slot = Some((ticket, reply.clone()));
        }
        _ => *slot = None,
    }
    Ok(reply)
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

        fn nest_info_memo(&self) -> Option<&NestInfoMemo> {
            self.memo.as_ref()
        }
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
