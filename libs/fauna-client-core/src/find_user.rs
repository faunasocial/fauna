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

/// A cross-nest Find User hit: the [`FoundUser`], plus the nest it resolved
/// **on** when that was a peer. `nest_url` is `Some` only for a foreign typed
/// domain; the knock send passes it as `fauna.inbox.send`'s
/// `recipient_nest_url` (`None` delivers on the home nest). Carrying the URL
/// from the lookup, rather than re-deriving it at send time, is the seam
/// `contacts.md` § Cross-nest knocks pins: a second derivation can drift from
/// the one that found the actor, and the failure is silent — the knock lands on
/// a *local* actor of the same localpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossNestFound {
    pub found: FoundUser,
    pub nest_url: Option<String>,
}

/// The Find User lookup for a typed `localpart[@domain]`, discovering the peer
/// nest itself when the typed domain is foreign — the one entry point every app
/// calls (UniFFI `find_user`, wasm `findUser`, linux directly), so none of them
/// re-derives the decision or the URL (priority #2).
///
/// - No typed domain → one hop on `home` ([`find_user_by_handle`]), `nest_url`
///   `None`.
/// - Typed domain → probe `home` with the domain as the multi-domain qualifier
///   and let [`fauna_core::resolve::is_foreign_handle_domain`] judge the echoed
///   domain (a refusal or not-found volunteers none, which the rule reads as
///   "go ask the peer" rather than silently resolving `bob@other.test` to a
///   local `bob`). Not foreign → that probe is the answer. Foreign → derive the
///   peer's base URL with `peer_url` (callers pass
///   `fauna_provisioning::probe::peer_nest_url`; a closure keeps that
///   dependency out of this wasm-clean crate), dial it with `connect_peer`, and
///   run [`find_user_by_handle`] **directly against that authority** — not
///   through the home nest's `fauna.nest.resolve` proxy, which refuses
///   loopback / IP-literal / `.local` authorities by design.
///
/// Errors are the failing hop's message; every app maps any `Err` to its one
/// find-error ("check the address").
pub async fn find_user_across_nests<H, P, C, Fut, E>(
    home: &H,
    handle: &str,
    typed_domain: Option<&str>,
    peer_url: impl FnOnce(&str) -> Option<String>,
    connect_peer: C,
) -> Result<CrossNestFound, String>
where
    H: RpcRequester,
    H::Error: std::fmt::Display,
    P: RpcRequester,
    P::Error: std::fmt::Display,
    C: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<P, E>>,
    E: std::fmt::Display,
{
    let Some(domain) = typed_domain else {
        let found = find_user_by_handle(home, handle, None)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(CrossNestFound {
            found,
            nest_url: None,
        });
    };
    let probe: Result<ActorByHandleReply, _> = home
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: handle.to_string(),
                domain: Some(domain.to_string()),
                extra: Default::default(),
            },
        )
        .await;
    let home_domain = probe.as_ref().ok().map(|r| r.domain.as_str());
    if !fauna_core::resolve::is_foreign_handle_domain(Some(domain), home_domain) {
        let reply = probe.map_err(|e| e.to_string())?;
        return Ok(CrossNestFound {
            found: FoundUser {
                actor_id: reply.actor_id,
                handle: handle.to_string(),
                domain: domain.to_string(),
            },
            nest_url: None,
        });
    }
    let url = peer_url(domain).ok_or_else(|| format!("no nest URL for {domain}"))?;
    let peer = connect_peer(url.clone())
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    // No qualifier on the peer hop: we are already talking to the nest that
    // serves the typed authority, so the multi-domain qualifier has nothing left
    // to disambiguate — and the authority a peer is *dialed* by (`host:port`, a
    // loopback test topology) is not necessarily a handle domain the peer serves,
    // so sending it would be refused as `fauna.actor.domain_not_local`. The
    // result still names the TYPED address (the dial names the peer), never the
    // peer's echo.
    let reply: ActorByHandleReply = peer
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: handle.to_string(),
                domain: None,
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(CrossNestFound {
        found: FoundUser {
            actor_id: reply.actor_id,
            handle: handle.to_string(),
            domain: domain.to_string(),
        },
        nest_url: Some(url),
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

    /// Serves one domain: answers a matching (or absent) qualifier, refuses any
    /// other the way a nest does (`fauna.actor.domain_not_local`).
    struct Serving {
        domain: Option<&'static str>,
    }

    impl RpcRequester for Serving {
        type Error = String;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let asked = serde_json::to_value(payload).unwrap();
            match self.domain {
                Some(d)
                    if asked["domain"].is_null()
                        || asked["domain"]
                            .as_str()
                            .is_some_and(|a| a.eq_ignore_ascii_case(d)) =>
                {
                    Ok(serde_json::from_value(serde_json::json!({
                        "actor_id": "cd".repeat(32),
                        "handle": "bob",
                        "domain": d,
                        "addresses": [],
                        "addressable": true,
                    }))
                    .unwrap())
                }
                _ => Err("fauna.actor.domain_not_local".to_string()),
            }
        }
    }

    #[test]
    fn a_foreign_typed_domain_is_resolved_on_the_peer_and_carries_its_url() {
        let home = Serving {
            domain: Some("home.test"),
        };
        let r = block_on(find_user_across_nests(
            &home,
            "bob",
            Some("127.0.0.1:8443"),
            |d| Some(format!("https://{d}")),
            |url| async move {
                assert_eq!(url, "https://127.0.0.1:8443");
                // The peer serves "peer.test" but is dialed by `host:port` in a
                // two-nest topology; a qualifier would be refused, so none is sent.
                Ok::<_, String>(Serving {
                    domain: Some("peer.test"),
                })
            },
        ))
        .unwrap();
        assert_eq!(r.nest_url.as_deref(), Some("https://127.0.0.1:8443"));
        assert_eq!(r.found.actor_id, "cd".repeat(32));
        assert_eq!(
            r.found.domain, "127.0.0.1:8443",
            "named by the typed address"
        );
    }

    #[test]
    fn the_home_domain_and_a_bare_handle_never_dial_a_peer() {
        let home = Serving {
            domain: Some("home.test"),
        };
        for typed in [Some("HOME.test"), None] {
            let r = block_on(find_user_across_nests(
                &home,
                "bob",
                typed,
                |_| panic!("no peer URL for a home lookup"),
                |_| async { Err::<Serving, String>("must not connect".into()) },
            ))
            .unwrap();
            assert_eq!(r.nest_url, None);
        }
    }

    #[test]
    fn an_unreachable_peer_is_an_error_not_a_local_fallback() {
        let home = Serving { domain: None };
        let r = block_on(find_user_across_nests(
            &home,
            "bob",
            Some("peer.test"),
            |d| Some(format!("https://{d}")),
            |_| async { Err::<Serving, String>("refused".into()) },
        ));
        assert!(r.unwrap_err().contains("refused"));
    }
}
