//! Typed-call wrapper for the four bearer session kinds —
//! `fauna.sessions.{list,revoke,revoke_all,lockout}` — the Sessions page's act
//! half (`docs/goal/ui/sessions.md` § Where logic lives). Beside
//! [`crate::AccountClient`] because this crate is the account-management
//! surface Settings hits and is already in every app's graph, wasm included.
//! The pre-identity `fauna.account.lockout` (the signed-out door) is not here:
//! this crate scopes itself to bearer kinds.

use core::future::Future;

use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::auth::OwnSessionSource;
use fauna_protocol::sessions::{
    LockoutReply, LockoutRequest, RevokeAllReply, RevokeAllRequest, RevokeReply, RevokeRequest,
    SessionsListReply, SessionsListRequest,
};

use crate::sessions_view::{
    self, SessionRowView, SessionsFoldInput, SessionsRoster, SessionsSnapshot,
};

/// One act on the Sessions page (`ui/sessions.md` § User actions). The lock's
/// confirm word is checked before this is built
/// ([`sessions_view::lockout_word_refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsAct {
    Revoke(String),
    RevokeOthers,
    Lockout,
}

/// What one [`SessionsClient::pass`] brings back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionsPass {
    /// The re-folded rows, or `None` when the re-read failed (or the lock
    /// landed) — the previously painted rows then stay
    /// (`ui/sessions.md` § Errors & edge cases).
    pub rows: Option<Vec<SessionRowView>>,
    pub error: Option<LocalizedText>,
    /// The lock landed: the app leaves the shell for its launch surface, which
    /// meets `fauna.auth.account_locked` and paints the locked notice.
    pub locked: bool,
}

impl SessionsPass {
    /// The page after this pass: its rows, or `previous`'s when the re-read
    /// failed, under this pass's error — one rule for every app.
    pub fn onto(self, previous: Option<SessionsSnapshot>) -> SessionsSnapshot {
        let rows = match self.rows {
            Some(rows) => rows,
            None => previous.map(|s| s.rows).unwrap_or_default(),
        };
        SessionsSnapshot {
            rows,
            error: self.error,
        }
    }
}

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

    /// The page's one pass, shared by all seven apps: act (if asked), then
    /// re-read and fold. A nav-edge hydrate passes `act: None`.
    ///
    /// `roster` is awaited only when there is something to fold — a lock that
    /// lands returns at once (the nest has just revoked every token, this
    /// app's included, so nothing is left to list), dropping it unpolled.
    /// `now` (Unix seconds) is read after the list arrives. An act's error
    /// outranks the re-read's: it is the one the user just asked about, and is
    /// never dropped (e2e convention 11).
    pub async fn pass<F>(
        &self,
        act: Option<SessionsAct>,
        roster: F,
        now: impl FnOnce() -> u64,
    ) -> SessionsPass
    where
        F: Future<Output = SessionsRoster>,
    {
        let mut error = None;
        // A landed sign out everywhere else may have ended the session THIS
        // connection was upgraded with: the keep-id is the holder's current id,
        // and a connection opened before a renewal rides an older one. The
        // nest delivers the act's Reply first and then closes the socket, so the
        // re-read below can meet the close — one more read rides the requester's
        // reconnect on the current bearer (`devices.md` § The client's own
        // session). Never for a failed act: nothing was torn down.
        let mut reread_past_teardown = false;
        match act {
            None => {}
            Some(SessionsAct::Revoke(token_id)) => {
                if let Err(e) = self.revoke(token_id).await {
                    error = Some(sessions_view::act_failed(e));
                }
            }
            Some(SessionsAct::RevokeOthers) => match self.revoke_others().await {
                Ok(_) => reread_past_teardown = true,
                Err(SessionsError::NoOwnSession) => {
                    error = Some(LocalizedText::key(sessions_view::keys::NO_OWN_SESSION));
                }
                Err(SessionsError::Rpc(e)) => error = Some(sessions_view::act_failed(e)),
            },
            Some(SessionsAct::Lockout) => match self.lockout().await {
                Ok(_) => {
                    return SessionsPass {
                        rows: None,
                        error: None,
                        locked: true,
                    };
                }
                Err(e) => error = Some(sessions_view::act_failed(e)),
            },
        }

        let roster = roster.await;
        let listed = match self.list().await {
            Err(_) if reread_past_teardown => self.list().await,
            first => first,
        };
        match listed {
            Ok(reply) => {
                let own = self.own_token_ids().await;
                let snapshot = sessions_view::fold(
                    &SessionsFoldInput {
                        sessions: &reply.sessions,
                        own_token_ids: &own,
                        roster: &roster.devices,
                        this_device_principal: roster.this_device_principal.as_deref(),
                        now: now(),
                    },
                    error,
                );
                SessionsPass {
                    rows: Some(snapshot.rows),
                    error: snapshot.error,
                    locked: false,
                }
            }
            Err(e) => SessionsPass {
                rows: None,
                error: Some(error.unwrap_or_else(|| sessions_view::load_failed(e))),
                locked: false,
            },
        }
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
                sessions: vec![listed("aabbccddeeff0011"), listed("other")],
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

    fn listed(id: &str) -> fauna_protocol::sessions::SessionInfo {
        fauna_protocol::sessions::SessionInfo {
            token_id: id.into(),
            created_at: 10,
            expires_at: 1_000,
            ip_address: None,
            last_used_at: 20,
            minted_by_device: None,
            extra: Default::default(),
        }
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

    #[test]
    fn a_hydrate_folds_the_list_with_this_apps_row_first() {
        let (rec, c) = client(Some("aabbccddeeff0011"));
        let pass = block_on(c.pass(None, async { SessionsRoster::default() }, || 100));
        assert_eq!(rec.kinds(), ["fauna.sessions.list"]);
        let rows = pass.rows.expect("folded");
        let ids: Vec<_> = rows.iter().map(|r| r.token_id.as_str()).collect();
        assert_eq!(ids, ["aabbccddeeff0011", "other"]);
        assert!(!rows[0].can_revoke);
        assert_eq!(pass.error, None);
        assert!(!pass.locked);
    }

    #[test]
    fn an_act_then_refolds() {
        let (rec, c) = client(Some("aabbccddeeff0011"));
        let pass = block_on(c.pass(
            Some(SessionsAct::Revoke("other".into())),
            async { SessionsRoster::default() },
            || 100,
        ));
        assert_eq!(
            rec.kinds(),
            ["fauna.sessions.revoke", "fauna.sessions.list"]
        );
        assert!(pass.rows.is_some());
    }

    #[test]
    fn a_landed_lock_returns_at_once_without_reading_the_roster() {
        let (rec, c) = client(Some("aabbccddeeff0011"));
        let pass = block_on(c.pass(
            Some(SessionsAct::Lockout),
            async { panic!("the roster must not be read after a lock") },
            || panic!("nor the clock"),
        ));
        assert!(pass.locked);
        assert_eq!(pass.rows, None);
        assert_eq!(rec.kinds(), ["fauna.sessions.lockout"]);
    }

    #[test]
    fn revoke_others_without_an_own_session_keeps_its_error_through_the_refold() {
        let (rec, c) = client(None);
        let pass = block_on(c.pass(
            Some(SessionsAct::RevokeOthers),
            async { SessionsRoster::default() },
            || 100,
        ));
        assert_eq!(rec.kinds(), ["fauna.sessions.list"]);
        assert_eq!(
            pass.error,
            Some(LocalizedText::key(sessions_view::keys::NO_OWN_SESSION))
        );
    }

    #[test]
    fn a_failed_reread_keeps_the_painted_rows_under_the_error() {
        let failing = fauna_client_testkit::FailingRequester::new("socket closed");
        let c = SessionsClient::new(failing, Own(Some("x")));
        let pass = block_on(c.pass(None, async { SessionsRoster::default() }, || 100));
        assert_eq!(pass.rows, None);
        let err = pass.error.clone().expect("the read's error");
        assert_eq!(err.key, sessions_view::keys::LOAD_FAILED);
        let painted = SessionsSnapshot {
            rows: vec![SessionRowView {
                token_id: "kept".into(),
                kind: LocalizedText::key("k"),
                mark: None,
                created_at: 0,
                last_used_at: 0,
                expires_at: 0,
                ip_address: None,
                can_revoke: true,
            }],
            error: None,
        };
        let page = pass.onto(Some(painted));
        assert_eq!(page.rows[0].token_id, "kept");
        assert_eq!(page.error, Some(err));
    }

    /// Answers like [`reply`], except that the first `fauna.sessions.list`
    /// after a `revoke_all` fails — the read the nest's teardown of this
    /// connection's own (non-current) session cuts off.
    struct TornAfterRevokeAll {
        kinds: std::sync::Mutex<Vec<&'static str>>,
    }

    impl RpcRequester for TornAfterRevokeAll {
        type Error = fauna_client_testkit::FailedRequest;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let torn = {
                let mut kinds = self.kinds.lock().unwrap();
                let torn = kind == "fauna.sessions.list"
                    && kinds.last() == Some(&"fauna.sessions.revoke_all");
                kinds.push(kind);
                torn
            };
            if torn {
                return Err(fauna_client_testkit::FailedRequest(
                    "The connection to the nest was lost.".into(),
                ));
            }
            Ok(fauna_protocol::decode_strict(&reply(kind)).unwrap())
        }
    }

    #[test]
    fn sign_out_everywhere_else_rereads_past_the_teardown_of_this_connection() {
        // A connection upgraded with an older own session than the holder's
        // current one is ended by the act it carries — the nest delivers the
        // act's Reply, then closes the socket, so the pass's first re-read can
        // meet the close. The act landed; the page must not report a failure
        // and keep the rows it just ended. One more read rides the reconnect.
        let rec = TornAfterRevokeAll {
            kinds: Default::default(),
        };
        let c = SessionsClient::new(rec, Own(Some("aabbccddeeff0011")));
        let pass = block_on(c.pass(
            Some(SessionsAct::RevokeOthers),
            async { SessionsRoster::default() },
            || 100,
        ));
        assert_eq!(
            *c.nest.kinds.lock().unwrap(),
            [
                "fauna.sessions.revoke_all",
                "fauna.sessions.list",
                "fauna.sessions.list"
            ]
        );
        assert_eq!(pass.error, None, "a landed act must not paint a failure");
        assert!(pass.rows.is_some(), "the second read folds");
    }
}
