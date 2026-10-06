//! Typed-call wrapper for the four bearer session kinds —
//! `fauna.sessions.{list,revoke,revoke_all,lockout}` — the Sessions page's act
//! half (`docs/goal/ui/sessions.md` § Where logic lives). Beside
//! [`crate::AccountClient`] because this crate is the account-management
//! surface Settings hits and is already in every app's graph, wasm included.
//! The pre-identity `fauna.account.lockout` (the signed-out door) is not here:
//! this crate scopes itself to bearer kinds.

use fauna_protocol::RpcRequester;
use fauna_protocol::auth::OwnSessionSource;
use fauna_protocol::sessions::{
    LockoutReply, LockoutRequest, RevokeAllReply, RevokeAllRequest, RevokeReply, RevokeRequest,
    SessionsListReply, SessionsListRequest,
};

/// Why [`SessionsClient::revoke_others`] did not reach the nest, or what the
/// nest said when it did.
#[derive(Debug)]
pub enum SessionsError<E> {
    /// The transport's own error.
    Rpc(E),
    /// The bearer holder cannot name this app's current session, so there is
    /// no `keep_token_id` to send — refused rather than sending one that
    /// matches nothing, which would sign this app out too.
    NoOwnSession,
}

impl<E: core::fmt::Display> core::fmt::Display for SessionsError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rpc(e) => e.fmt(f),
            Self::NoOwnSession => f.write_str("this app's own session is not known yet"),
        }
    }
}

/// The session kinds, generic over the WS-RPC transport (`R`) and the bearer
/// holder that knows this app's own session ids (`S`). Native passes
/// `Arc<NestClient>` for both.
pub struct SessionsClient<R: RpcRequester, S: OwnSessionSource> {
    nest: R,
    own: S,
}

impl<R: RpcRequester, S: OwnSessionSource> SessionsClient<R, S> {
    pub fn new(nest: R, own: S) -> Self {
        Self { nest, own }
    }

    /// `fauna.sessions.list` — every unexpired session of the calling actor.
    pub async fn list(&self) -> Result<SessionsListReply, R::Error> {
        self.nest
            .request(
                "fauna.sessions.list",
                SessionsListRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// This app's own live session ids, for the fold's "This app" row.
    pub async fn own_token_ids(&self) -> Vec<String> {
        self.own.own_token_ids().await
    }

    /// `fauna.sessions.revoke` — end one session (it must be the caller's).
    pub async fn revoke(&self, token_id: impl Into<String>) -> Result<RevokeReply, R::Error> {
        self.nest
            .request(
                "fauna.sessions.revoke",
                RevokeRequest {
                    token_id: token_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sessions.revoke_all` — end every session but this app's. The
    /// keep-id is read from the bearer holder **now**, never from a painted
    /// list: a renewal between paint and press must not name a dead token
    /// (`behavior/devices.md` § The client's own session).
    pub async fn revoke_others(&self) -> Result<RevokeAllReply, SessionsError<R::Error>> {
        let keep_token_id = self
            .own
            .current_token_id()
            .await
            .ok_or(SessionsError::NoOwnSession)?;
        self.nest
            .request(
                "fauna.sessions.revoke_all",
                RevokeAllRequest {
                    keep_token_id,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(SessionsError::Rpc)
    }

    /// `fauna.sessions.lockout` — lock the account for the nest's fixed 24
    /// hours. No duration crosses the wire (`behavior/devices.md` § Emergency
    /// lockout). The caller gates it on
    /// `fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD` in its action
    /// arm, not only in the render.
    pub async fn lockout(&self) -> Result<LockoutReply, R::Error> {
        self.nest
            .request(
                "fauna.sessions.lockout",
                LockoutRequest {
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use std::sync::Arc;

    struct Own(Option<&'static str>);

    impl OwnSessionSource for Own {
        async fn own_token_ids(&self) -> Vec<String> {
            self.0.into_iter().map(str::to_string).collect()
        }
        async fn current_token_id(&self) -> Option<String> {
            self.0.map(str::to_string)
        }
    }

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.sessions.list" => fauna_protocol::encode_canonical(&SessionsListReply {
                sessions: Vec::new(),
                extra: Default::default(),
            }),
            "fauna.sessions.revoke" => fauna_protocol::encode_canonical(&RevokeReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.sessions.revoke_all" => fauna_protocol::encode_canonical(&RevokeAllReply {
                ok: true,
                revoked: 2,
                extra: Default::default(),
            }),
            "fauna.sessions.lockout" => fauna_protocol::encode_canonical(&LockoutReply {
                ok: true,
                locked_until: 1,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn client(
        own: Option<&'static str>,
    ) -> (
        Arc<RecordingRequester>,
        SessionsClient<Arc<RecordingRequester>, Own>,
    ) {
        let rec = Arc::new(RecordingRequester::new(reply));
        (rec.clone(), SessionsClient::new(rec, Own(own)))
    }

    #[test]
    fn list_sends_its_kind() {
        let (rec, c) = client(None);
        block_on(c.list()).expect("infallible mock");
        let (kind, bytes) = rec.recorded();
        assert_eq!(kind, "fauna.sessions.list");
        let _: SessionsListRequest = fauna_protocol::decode_strict(&bytes).unwrap();
    }

    #[test]
    fn revoke_names_the_token() {
        let (rec, c) = client(None);
        block_on(c.revoke("0011223344556677")).expect("infallible mock");
        let (kind, bytes) = rec.recorded();
        assert_eq!(kind, "fauna.sessions.revoke");
        let req: RevokeRequest = fauna_protocol::decode_strict(&bytes).unwrap();
        assert_eq!(req.token_id, "0011223344556677");
    }

    #[test]
    fn revoke_others_keeps_the_holders_current_id() {
        let (rec, c) = client(Some("aabbccddeeff0011"));
        let reply = block_on(c.revoke_others()).expect("infallible mock");
        assert_eq!(reply.revoked, 2);
        let (kind, bytes) = rec.recorded();
        assert_eq!(kind, "fauna.sessions.revoke_all");
        let req: RevokeAllRequest = fauna_protocol::decode_strict(&bytes).unwrap();
        assert_eq!(req.keep_token_id, "aabbccddeeff0011");
    }

    #[test]
    fn revoke_others_refuses_without_an_own_session() {
        let (rec, c) = client(None);
        let err = block_on(c.revoke_others()).unwrap_err();
        assert!(matches!(err, SessionsError::NoOwnSession));
        assert!(rec.kinds().is_empty(), "nothing may reach the nest");
    }

    #[test]
    fn lockout_sends_no_duration() {
        let (rec, c) = client(None);
        block_on(c.lockout()).expect("infallible mock");
        let (kind, bytes) = rec.recorded();
        assert_eq!(kind, "fauna.sessions.lockout");
        let req: LockoutRequest = fauna_protocol::decode_strict(&bytes).unwrap();
        assert!(req.extra.is_empty());
    }
}
