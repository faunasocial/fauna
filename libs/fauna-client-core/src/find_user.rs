//! The Contacts Find User handle resolve — one `fauna.actor.by_handle` hop and
//! the rule for which address the find result names, shared by every app's
//! find-user surface (the UniFFI `resolve_handle` face for apple / windows /
//! android, the wasm `actorByHandle` twin for web, and linux directly).
//!
//! **The dial names the peer** (`foreign-handle-resolution.md` § Peer-auth
//! model). A find-user input typed with a `@domain` qualifier is dialed at that
//! domain, so the address the result names is the typed `localpart@domain` —
//! the reply's own `handle` and `domain` are that nest's assertion about itself,
//! vouched for by nobody, and are never read for identity: a nest serving
//! `attacker.test` that echoes `trusted.test` must not get a find card reading
//! `bob@trusted.test`. An honest nest asked with a qualifier either echoes it or
//! refuses (`fauna.actor.domain_not_local`), so taking the typed pair costs an
//! honest multi-domain nest nothing. A bare handle (no qualifier) is by
//! definition resolved on the client's own home nest, whose echo is how the
//! client learns the handle's live identity domain — the one arm that reads it,
//! as `FaunaMlsBackend::resolve_address`'s same-nest arm does.

use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{ActorByHandleReply, ActorByHandleRequest};
use serde::Serialize;

/// A Find User hit: the actor the dialed nest resolved, and the address the
/// result renders (see the module doc for which fields that address is built
/// from). Serializes as the `{ actor_id, handle, domain }` shape the apps'
/// find-result consumers already read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundUser {
    pub actor_id: String,
    pub handle: String,
    pub domain: String,
}

/// Resolve `handle` (the typed localpart) on the nest behind `rpc` — the typed
/// `@domain`'s nest for a qualified input, the home nest for a bare one — and
/// name the result by the dial rule. `typed_domain` is also sent as the
/// multi-domain qualifier (`mail-multidomain.md` § Multi-domain handles
/// § Resolution), so a nest that does not serve it refuses rather than answering
/// for some other domain.
pub async fn find_user_by_handle<R: RpcRequester>(
    rpc: &R,
    handle: &str,
    typed_domain: Option<&str>,
) -> Result<FoundUser, R::Error> {
    let reply: ActorByHandleReply = rpc
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: handle.to_string(),
                domain: typed_domain.map(str::to_string),
                extra: Default::default(),
            },
        )
        .await?;
    Ok(match typed_domain {
        Some(domain) => FoundUser {
            actor_id: reply.actor_id,
            handle: handle.to_string(),
            domain: domain.to_string(),
        },
        None => FoundUser {
            actor_id: reply.actor_id,
            handle: reply.handle,
            domain: reply.domain,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A peer that answers every `by_handle` with a fixed identity of its own
    /// choosing, whatever it was asked — the hostile echo.
    struct EchoingPeer {
        handle: &'static str,
        domain: &'static str,
        asked: RefCell<Vec<serde_json::Value>>,
    }

    impl RpcRequester for EchoingPeer {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.actor.by_handle");
            self.asked
                .borrow_mut()
                .push(serde_json::to_value(payload).unwrap());
            Ok(serde_json::from_value(serde_json::json!({
                "actor_id": "ab".repeat(32),
                "handle": self.handle,
                "domain": self.domain,
                "addresses": [],
                "addressable": true,
            }))
            .unwrap())
        }
    }

    fn peer(handle: &'static str, domain: &'static str) -> EchoingPeer {
        EchoingPeer {
            handle,
            domain,
            asked: RefCell::new(Vec::new()),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn a_peer_echoing_a_domain_it_was_not_dialed_at_is_named_by_the_dial() {
        let attacker = peer("alice", "trusted.test");
        let found = block_on(find_user_by_handle(&attacker, "bob", Some("attacker.test"))).unwrap();
        assert_eq!(found.actor_id, "ab".repeat(32));
        assert_eq!(
            (found.handle.as_str(), found.domain.as_str()),
            ("bob", "attacker.test"),
            "the find result names the typed address, never the peer's echo"
        );
        assert_eq!(
            attacker.asked.borrow()[0]["domain"],
            "attacker.test",
            "the typed domain still travels as the multi-domain qualifier"
        );
    }

    #[test]
    fn a_bare_handle_takes_the_home_nests_identity_domain() {
        let home = peer("bob", "primary.test");
        let found = block_on(find_user_by_handle(&home, "bob", None)).unwrap();
        assert_eq!(
            (found.handle.as_str(), found.domain.as_str()),
            ("bob", "primary.test")
        );
        assert!(home.asked.borrow()[0]["domain"].is_null());
    }
}
