//! The **principal** side of WS-RPC dispatch: what a third-party principal's
//! session may call, and the handlers that serve it
//! (`docs/goal/architecture/apps/bridges.md` § Capability-allowlist enforcement
//! → *How the class meets the connection*).
//!
//! A principal session carries a [`PrincipalBinding`] — the account, the
//! principal id, the scopes of the token the session was opened with and that
//! token's grant family — and the zero actor. Every call it makes runs
//! [`dispatch_principal`], which:
//!
//! 1. **resolves** the binding per call ([`resolve_principal`]): the principal
//!    row must still exist, the token's grant family must still be live and
//!    the account must still have authority, or the call answers the
//!    every-kind refusal — so `fauna.principals.revoke` (door (a)), a
//!    suspension or a lockout (door (b)) and a grant family's ending (door
//!    (c)) bite at the session's next call;
//! 2. checks the **ceiling** — `is_permitted(ThirdParty, kind)`;
//! 3. checks the **scopes** — `scope_covers` over the row's scopes intersected
//!    with the token's, so a narrowing re-consent narrows every live session
//!    at once and an older grant family's session never picks up a later
//!    ceremony's wider set;
//! 4. runs the kind's **principal handler**, which receives the resolved
//!    [`PrincipalCaller`] and never an actor id: the account's own handlers,
//!    and with them its `User`/`Admin` reach, are unreachable from here by
//!    construction.
//!
//! Each handler lives beside its actor twin (`feed_handlers`,
//! `bridge_blob_handlers`, `nostr::bunker_handlers`, `folder_deposit`,
//! `subscription_handlers`); this module owns the table and the gate.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use bytes::Bytes;
use futures_util::future::BoxFuture;

use fauna_protocol::RpcError;

use crate::bridge_method_allowlist::{
    CallerClass, class_refusal_namespace, is_permitted, scope_covers,
};
use crate::routes::AppState;
use crate::rpc_errors::{central_permission_denied, internal, permission_denied_ns};

/// What a principal session was opened with — fixed for the session's life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalBinding {
    /// The account the principal belongs to (the token's subject).
    pub account: [u8; 32],
    /// The principal row's id.
    pub principal_id: Vec<u8>,
    /// The scopes of the access token the session was opened with.
    pub token_scopes: Vec<String>,
    /// The grant family the token was minted under — its `sid`, decoded: the
    /// grant id, the family's initial refresh `jti` and the
    /// `atproto_sessions` row whose `revoked_at` says whether the family is
    /// still live (door (c)). `Some` for every token-admitted binding: the
    /// gate refuses a token without one ([`crate::principal_session::admit_presentation`]).
    /// `None` only for a nest-hosted plugin's binding, which holds no token
    /// and so belongs to no family — its reach is the principal row alone.
    pub family_id: Option<Vec<u8>>,
}

/// A binding resolved for ONE call: the live row's reach, read just now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalCaller {
    pub account: [u8; 32],
    pub principal_id: Vec<u8>,
    /// The row's granted scopes ∩ the session token's — the set
    /// `scope_covers` reads.
    pub scopes: Vec<String>,
    /// The key the principal attested at consent; `None` when it attested
    /// none.
    pub holder_x25519: Option<[u8; 32]>,
    /// The writer key the principal attested at consent — the only
    /// `writer_id` its puts may carry; `None` when it attested none, which
    /// leaves it read-only over its kinds.
    pub writer_ed25519: Option<[u8; 32]>,
}

/// Resolve a session's binding for one call. `None` — the every-kind refusal —
/// when the principal row is gone (revoked), the token's grant family has
/// ended (door (c): the user's per-grant revoke, `/oauth/revoke`, the
/// bridge-class `end_session`, the reuse family-kill, the forced
/// session-secret rotation), the account no longer has authority (deleted,
/// suspended, locked out), or the account's external-apps switch is OFF
/// (`atproto-pds-full.md` § F1 detail, the kill-switch bullet: a principal is
/// an external app, and OFF suspends them all).
pub async fn resolve_principal(
    state: &AppState,
    binding: &PrincipalBinding,
) -> anyhow::Result<Option<PrincipalCaller>> {
    let Some((granted, keys)) = state
        .db
        .get_third_party_principal_reach(&binding.account, &binding.principal_id)
        .await?
    else {
        return Ok(None);
    };
    // The grant family the session's token was minted under: an ended family
    // bites at the next call, whether or not an ending's sweep has reached
    // this socket yet (`transport-connection.md` § *The principal session* →
    // *Revocation — three doors*, door (c)).
    if let Some(family_id) = &binding.family_id
        && !state
            .db
            .is_oauth_session_live(&binding.account, family_id)
            .await?
    {
        return Ok(None);
    }
    match state.db.actor_authority(&binding.account[..]).await? {
        Some(authority) if !authority.is_revoked_at(crate::db::now_epoch_secs()) => {}
        _ => return Ok(None),
    }
    // The account's external-apps switch: OFF suspends every external app,
    // this principal included, with the same every-kind refusal a revoked row
    // gets — the outside cannot tell the two apart.
    if !state
        .db
        .get_atproto_external_apps_enabled(&binding.account)
        .await?
    {
        return Ok(None);
    }
    let scopes = granted
        .split_whitespace()
        .filter(|s| binding.token_scopes.iter().any(|t| t == s))
        .map(str::to_string)
        .collect();
    Ok(Some(PrincipalCaller {
        account: binding.account,
        principal_id: binding.principal_id.clone(),
        scopes,
        holder_x25519: keys.holder_x25519,
        writer_ed25519: keys.writer_ed25519,
    }))
}

/// A principal handler: the resolved caller and the raw request payload, the
/// same encoding contract as [`crate::rpc_router::RpcHandler`].
pub type PrincipalHandler = Box<
    dyn Fn(Arc<AppState>, PrincipalCaller, Bytes) -> BoxFuture<'static, Result<Bytes, RpcError>>
        + Send
        + Sync,
>;

/// The principal handler table — one entry per kind in the `ThirdParty`
/// ceiling (a test holds the two sets equal). A kind's wire metadata
/// (deadline, replay) is the actor router's: one kind, one wire contract.
static PRINCIPAL_HANDLERS: LazyLock<BTreeMap<&'static str, PrincipalHandler>> =
    LazyLock::new(|| {
        let mut table: BTreeMap<&'static str, PrincipalHandler> = BTreeMap::new();
        table.insert(
            "fauna.capabilities.fetch",
            crate::bridge_blob_handlers::principal_fetch_grants_handler(),
        );
        // The fourth arm's three custom-feed reads (`authorization-server.md`
        // § Scope grammar → *The fourth arm*).
        table.insert(
            "fauna.feed.list",
            crate::feed_handlers::principal_feed_list_handler(),
        );
        table.insert(
            "fauna.feed.get",
            crate::feed_handlers::principal_feed_get_handler(),
        );
        table.insert(
            "fauna.feed.posts",
            crate::feed_handlers::principal_feed_posts_handler(),
        );
        table.insert(
            "fauna.feed.local.posts",
            crate::feed_handlers::principal_feed_local_posts_handler(),
        );
        table.insert(
            "fauna.feed.trending.posts",
            crate::feed_handlers::principal_feed_trending_posts_handler(),
        );
        // The `records` arm's two doors (`third-party-kinds.md` § The record
        // doors): a principal's own `ext:<kind>` scopes, written as its own
        // writer key and listed back.
        table.insert(
            "fauna.account.state.put",
            crate::sync_handlers::principal_account_state_put_handler(),
        );
        table.insert(
            "fauna.sync.changes.list",
            crate::sync_handlers::principal_changes_list_handler(),
        );
        // The `fauna:personalization:rw` arm's model doors
        // (`authorization-server.md` § Scope grammar → *The fifth arm*); its
        // `state` list rides `fauna.sync.changes.list` above.
        table.insert(
            fauna_protocol::personalization::KIND_MODEL_FETCH,
            crate::personalization_handlers::principal_model_fetch_handler(),
        );
        table.insert(
            fauna_protocol::personalization::KIND_MODEL_PUT,
            crate::personalization_handlers::principal_model_put_handler(),
        );
        // The bridged-conversation family's bridge half (`apps/bridges.md`
        // § Bridge-kind catalogue → Phase G), under the
        // `fauna:conversations:bridge` arm, and the recipient-key read its
        // deposits are sealed with.
        for (kind, handler) in crate::bridged_conversation_handlers::principal_handlers() {
            table.insert(kind, handler);
        }
        table.insert(
            "fauna.bridges.fetch_recipient_mls_pubkey",
            crate::bridge_routing_handlers::principal_fetch_recipient_mls_pubkey_handler(),
        );
        // The `folder_deposit` arm's door (`file-sync.md` § Third-party
        // deposit ingress) — the HTTP deposit door dispatches here too.
        table.insert(
            "fauna.folders.deposit",
            crate::folder_deposit::principal_deposit_handler(),
        );
        for (kind, handler) in crate::folder_deposit::principal_staged_deposit_handlers() {
            table.insert(kind, handler);
        }
        // The `events` arm's kind (`transport.md` § Push events →
        // *Third-party event doors*) — the HTTP long-poll dispatches here too.
        table.insert(
            fauna_protocol::push_events::KIND_EVENTS_POLL,
            crate::events_doors::principal_events_poll_handler(),
        );
        // The `follow:write` arm's three kinds (`authorization-server.md`
        // § Scope grammar → *The sixth arm*) — the followers tier only.
        for (kind, handler) in crate::subscription_handlers::principal_handlers() {
            table.insert(kind, handler);
        }
        // The oracle's door (TP11). Gated with the bunker cluster it binds
        // into: without the `nostr` feature the actor router carries no
        // `fauna.nostr.bunker.*` kind, so the ceiling the sweep test reads
        // has no `bind` either.
        #[cfg(feature = "nostr")]
        table.insert(
            "fauna.nostr.bunker.bind",
            crate::nostr::bunker_handlers::principal_bind_handler(),
        );
        // The oracle's ActivityPub door (TP11), gated with the bridge whose
        // actor key it signs with, for the same reason.
        #[cfg(feature = "activitypub")]
        table.insert(
            fauna_protocol::activitypub_oracle::KIND_HTTP_SIG,
            crate::activitypub::oracle::principal_http_sig_handler(),
        );
        // The oracle's posting door (TP11). Ungated: the authoring sub-key
        // and the own-write ingest it signs into are core, not a bridge's.
        table.insert(
            fauna_protocol::post_author::KIND_CREATE,
            crate::post_author_oracle::principal_create_handler(),
        );
        table.insert(
            fauna_protocol::post_author::KIND_DELETE,
            crate::post_author_oracle::principal_delete_handler(),
        );
        // The oracle's ATProto door (TP11). Ungated: its custodian is the
        // first-party `atproto.pds` bridge, a role every nest can enroll.
        table.insert(
            fauna_protocol::atproto_oracle::KIND_SERVICE_AUTH,
            crate::atproto_oracle::principal_service_auth_handler(),
        );
        table
    });

/// Every kind a principal handler serves.
pub fn principal_handler_kinds() -> impl Iterator<Item = &'static str> {
    PRINCIPAL_HANDLERS.keys().copied()
}

/// Dispatch one call on a principal session — the gate and the handler, in
/// the order the module doc gives.
pub async fn dispatch_principal(
    state: Arc<AppState>,
    binding: &PrincipalBinding,
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let Some(caller) = resolve_principal(&state, binding).await.map_err(internal)? else {
        tracing::warn!(
            target: "permission_gate",
            kind,
            "permission denied: principal revoked or account without authority"
        );
        return Err(central_permission_denied());
    };
    dispatch_resolved(state, caller, kind, payload).await
}

/// Dispatch one call a hosted plugin makes as its own INSTALL row
/// (`third-party.md` § The principal model → *Hosted principals*; the WIT
/// `nest-api.call` with no account) — the plugin runner's door, never a
/// session's. The row is the nest owner's, not an account's, so there is no
/// account authority or external-apps switch to resolve: the install row
/// existing IS the reach, and its `granted_scopes` (the ceiling the admin
/// approved) are the scopes. The ceiling, the scope check and the handler
/// are the same three steps every principal call takes.
pub async fn dispatch_hosted_install(
    state: Arc<AppState>,
    principal_id: &[u8],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let owner = crate::db::third_party_principals::NEST_OWNER_ACTOR;
    let Some((granted, keys)) = state
        .db
        .get_third_party_principal_reach(&owner, principal_id)
        .await
        .map_err(internal)?
    else {
        tracing::warn!(
            target: "permission_gate",
            kind,
            "permission denied: hosted plugin uninstalled"
        );
        return Err(central_permission_denied());
    };
    let caller = PrincipalCaller {
        account: owner,
        principal_id: principal_id.to_vec(),
        scopes: granted.split_whitespace().map(str::to_string).collect(),
        holder_x25519: keys.holder_x25519,
        writer_ed25519: keys.writer_ed25519,
    };
    dispatch_resolved(state, caller, kind, payload).await
}

/// The gate after resolution: the `ThirdParty` ceiling, the caller's scopes,
/// the kind's principal handler. Public to the crate for the HTTP record door
/// (`crate::records_door`), which resolves the caller once to read its
/// attested writer key and then dispatches here — the same three steps.
pub(crate) async fn dispatch_resolved(
    state: Arc<AppState>,
    caller: PrincipalCaller,
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let refuse = |why: &str| -> RpcError {
        tracing::warn!(target: "permission_gate", kind, why, "principal call refused");
        match class_refusal_namespace(kind) {
            Some(ns) => permission_denied_ns(ns, format!("{kind}: {why}")),
            None => central_permission_denied(),
        }
    };
    if !is_permitted(CallerClass::ThirdParty, kind) {
        return Err(refuse("not permitted for caller class ThirdParty"));
    }
    if !scope_covers(&caller.scopes, kind) {
        return Err(refuse("not covered by the principal's scopes"));
    }
    let Some(handler) = PRINCIPAL_HANDLERS.get(kind) else {
        // Unreachable while the sweep test holds the table equal to the
        // ceiling; refused rather than panicking if that ever slips.
        return Err(central_permission_denied());
    };
    handler(state, caller, payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_method_allowlist::principal_reach;

    /// Rule (d): the principal-handler set IS the `ThirdParty` ceiling — a
    /// matrix arm with no principal handler would refuse at dispatch, and a
    /// handler with no arm would be dead code a later widening could wake.
    #[test]
    fn principal_handler_kinds_equal_the_ceiling() {
        let router = crate::build_rpc_router();
        let ceiling: Vec<&str> = router
            .iter_kinds()
            .filter(|k| is_permitted(CallerClass::ThirdParty, k))
            .collect();
        let handlers: Vec<&str> = principal_handler_kinds().collect();
        assert_eq!(handlers, ceiling);
        for kind in &handlers {
            assert!(principal_reach(kind).is_some(), "{kind} names no reach");
        }
    }

    use crate::db::CacheDb;
    use crate::db::third_party_principals::{AttestedKeys, ExecutionForm, PrincipalAttestation};
    use fauna_protocol::feed::{FeedTrendingPostsReply, FeedTrendingPostsRequest};
    use fauna_protocol::wrapped_blob::{FetchGrantsReply, FetchGrantsRequest};

    const ACCOUNT: [u8; 32] = [0xA1; 32];
    const OTHER: [u8; 32] = [0xB2; 32];
    const CLIENT: &str = "https://app.example/client.json";
    const HOLDER: [u8; 32] = [0x77; 32];
    const NEVER: i64 = i64::MAX;

    async fn consent(db: &CacheDb, actor: &[u8; 32], family: &[u8], scopes: &str) {
        db.record_atproto_oauth_grant(
            actor,
            family,
            CLIENT,
            Some("Example App"),
            scopes,
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(HOLDER),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: None,
            },
        )
        .await
        .unwrap();
    }

    /// An account with a principal consented `fauna:feed:read`, and the
    /// binding a session opened under a token carrying `token_scopes`.
    async fn fixture(token_scopes: &[&str]) -> (Arc<AppState>, PrincipalBinding) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        consent(&db, &ACCOUNT, b"family-1", "fauna:feed:read openid").await;
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let state = Arc::new(AppState::for_test(db));
        let binding = PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: token_scopes.iter().map(|s| s.to_string()).collect(),
            family_id: None,
        };
        (state, binding)
    }

    fn trending() -> Bytes {
        fauna_protocol::encode_canonical(&FeedTrendingPostsRequest::default()).unwrap()
    }

    fn fetch() -> Bytes {
        fauna_protocol::encode_canonical(&FetchGrantsRequest::default()).unwrap()
    }

    async fn call(
        state: &Arc<AppState>,
        b: &PrincipalBinding,
        kind: &str,
        p: Bytes,
    ) -> Result<Bytes, RpcError> {
        dispatch_principal(state.clone(), b, kind, p).await
    }

    #[tokio::test]
    async fn a_kind_inside_the_scopes_is_served() {
        let (state, b) = fixture(&["fauna:feed:read", "openid"]).await;
        let reply = call(&state, &b, "fauna.feed.trending.posts", trending())
            .await
            .expect("in scope");
        let _: FeedTrendingPostsReply = fauna_protocol::decode_strict(&reply).unwrap();
    }

    /// The ceiling: the account's own kinds are refused on a principal
    /// session, with the kind's family code — never served by the actor
    /// handler.
    #[tokio::test]
    async fn a_kind_outside_the_ceiling_is_refused() {
        let (state, b) = fixture(&["fauna:feed:read"]).await;
        for (kind, code) in [
            ("fauna.feed.create", "fauna.feed.permission_denied"),
            (
                "fauna.capabilities.mint",
                "fauna.capabilities.permission_denied",
            ),
            (
                "fauna.principals.list",
                "fauna.principals.permission_denied",
            ),
        ] {
            let err = call(&state, &b, kind, Bytes::new()).await.unwrap_err();
            assert_eq!(err.code, code, "{kind}");
        }
    }

    /// The fourth arm (`authorization-server.md` § Scope grammar → *The
    /// fourth arm*): the bare arm alone never reaches the custom-feed reads;
    /// a session also holding `fauna:feed:read:account` lists the account's
    /// own feeds and no other account's, and reads only its own feed's posts.
    #[tokio::test]
    async fn the_account_arm_reads_the_accounts_own_feeds() {
        use fauna_protocol::feed::{
            FeedGetRequest, FeedListReply, FeedListRequest, FeedPostsReply, FeedPostsRequest,
        };
        let (state, bare) = fixture(&["fauna:feed:read", "openid"]).await;
        let enc = |v: &FeedPostsRequest| fauna_protocol::encode_canonical(v).unwrap();
        let list = fauna_protocol::encode_canonical(&FeedListRequest {
            extra: Default::default(),
        })
        .unwrap();
        let err = call(&state, &bare, "fauna.feed.list", list.clone())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.feed.permission_denied");

        consent(
            &state.db,
            &ACCOUNT,
            b"family-2",
            "fauna:feed:read fauna:feed:read:account openid",
        )
        .await;
        let b = PrincipalBinding {
            token_scopes: vec!["fauna:feed:read".into(), "fauna:feed:read:account".into()],
            family_id: None,
            ..bare
        };
        let rules = crate::feed_routes::validate_and_encode_rules("any", &[])
            .ok()
            .expect("rules");
        let mine = state
            .db
            .create_feed(&ACCOUNT, "Mine", &rules, "any", "local", "[]", None)
            .await
            .unwrap();
        let theirs = state
            .db
            .create_feed(&OTHER, "Theirs", &rules, "any", "local", "[]", None)
            .await
            .unwrap();

        let reply: FeedListReply = fauna_protocol::decode_strict(
            &call(&state, &b, "fauna.feed.list", list).await.unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = reply.feeds.iter().map(|f| f.feed_id.as_str()).collect();
        assert_eq!(ids, vec![mine.as_str()], "the account's own feeds only");

        let get = |id: &str| {
            fauna_protocol::encode_canonical(&FeedGetRequest {
                feed_id: id.to_string(),
                extra: Default::default(),
            })
            .unwrap()
        };
        call(&state, &b, "fauna.feed.get", get(&mine))
            .await
            .expect("own feed");
        call(&state, &b, "fauna.feed.get", get(&theirs))
            .await
            .expect_err("another account's feed answers as missing");

        let posts = |id: &str| FeedPostsRequest {
            feed_id: id.to_string(),
            cursor: None,
            limit: None,
            order: None,
            score_cursor: None,
            score_cursor_created_at: None,
            search: None,
            extra: Default::default(),
        };
        let _: FeedPostsReply = fauna_protocol::decode_strict(
            &call(&state, &b, "fauna.feed.posts", enc(&posts(&mine)))
                .await
                .expect("own feed's posts"),
        )
        .unwrap();
        call(&state, &b, "fauna.feed.posts", enc(&posts(&theirs)))
            .await
            .expect_err("another account's feed's posts");
    }

    /// The scope set is the row's ∩ the token's: a token without the arm
    /// cannot reach the scoped kind even though the row holds it — while the
    /// session kind still answers.
    #[tokio::test]
    async fn the_token_narrows_the_row() {
        let (state, b) = fixture(&["openid"]).await;
        let err = call(&state, &b, "fauna.feed.trending.posts", trending())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.feed.permission_denied");
        call(&state, &b, "fauna.capabilities.fetch", fetch())
            .await
            .expect("a session kind needs no scope");
    }

    /// …and the row narrows the token: a later ceremony consenting less
    /// narrows the live session at its next call.
    #[tokio::test]
    async fn a_narrowing_re_consent_narrows_the_live_session() {
        let (state, b) = fixture(&["fauna:feed:read", "openid"]).await;
        call(&state, &b, "fauna.feed.trending.posts", trending())
            .await
            .expect("before");
        consent(&state.db, &ACCOUNT, b"family-2", "openid").await;
        let err = call(&state, &b, "fauna.feed.trending.posts", trending())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.feed.permission_denied");
    }

    /// Revocation bites at the next call, with the every-kind refusal.
    #[tokio::test]
    async fn a_revoked_principal_is_refused_every_kind() {
        let (state, b) = fixture(&["fauna:feed:read"]).await;
        state
            .db
            .revoke_third_party_principal(&ACCOUNT, &b.principal_id)
            .await
            .unwrap()
            .expect("existed");
        for kind in ["fauna.feed.trending.posts", "fauna.capabilities.fetch"] {
            let err = call(&state, &b, kind, Bytes::new()).await.unwrap_err();
            assert_eq!(err.code, "fauna.bridges.permission_denied", "{kind}");
        }
    }

    /// The account's external-apps switch OFF refuses every kind — the same
    /// every-kind shape as a revoked row, so the outside cannot tell them
    /// apart — and ON serves again with nothing redone.
    #[tokio::test]
    async fn the_external_apps_switch_off_refuses_every_kind() {
        let (state, b) = fixture(&["fauna:feed:read"]).await;
        state
            .db
            .set_atproto_external_apps_enabled(&ACCOUNT, false)
            .await
            .unwrap();
        for kind in ["fauna.feed.trending.posts", "fauna.capabilities.fetch"] {
            let err = call(&state, &b, kind, Bytes::new()).await.unwrap_err();
            assert_eq!(err.code, "fauna.bridges.permission_denied", "{kind}");
        }
        state
            .db
            .set_atproto_external_apps_enabled(&ACCOUNT, true)
            .await
            .unwrap();
        call(&state, &b, "fauna.feed.trending.posts", trending())
            .await
            .expect("ON restores the session");
    }

    /// An account that lost its authority takes its principals with it.
    #[tokio::test]
    async fn an_account_without_authority_is_refused() {
        let (state, b) = fixture(&["fauna:feed:read"]).await;
        let gone = PrincipalBinding {
            account: OTHER,
            ..b.clone()
        };
        let err = call(&state, &gone, "fauna.feed.trending.posts", trending())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }

    /// `capabilities.fetch` serves only what THIS account wrapped to the
    /// row's own key — not another account's grant to the same key, not the
    /// account's grant to another key.
    #[tokio::test]
    async fn fetch_serves_only_this_accounts_grants_to_the_principals_key() {
        let (state, b) = fixture(&["fauna:feed:read"]).await;
        state
            .db
            .put_capability_grant(&ACCOUNT, &[1; 16], &HOLDER, NEVER, b"mine")
            .await
            .unwrap();
        state
            .db
            .put_capability_grant(&ACCOUNT, &[2; 16], &[0x55; 32], NEVER, b"other-key")
            .await
            .unwrap();
        state
            .db
            .put_capability_grant(&OTHER, &[3; 16], &HOLDER, NEVER, b"other-account")
            .await
            .unwrap();
        let reply = call(&state, &b, "fauna.capabilities.fetch", fetch())
            .await
            .unwrap();
        let reply: FetchGrantsReply = fauna_protocol::decode_strict(&reply).unwrap();
        let grants: Vec<&[u8]> = reply.grants.iter().map(|g| g.as_ref()).collect();
        assert_eq!(grants, vec![&b"mine"[..]]);
    }

    /// A principal that attested no key reads an empty list, not an error.
    #[tokio::test]
    async fn fetch_without_an_attested_key_is_an_empty_list() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        db.record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            CLIENT,
            None,
            "fauna:feed:read",
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: None,
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: None,
            },
        )
        .await
        .unwrap();
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let state = Arc::new(AppState::for_test(db));
        let b = PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: vec!["fauna:feed:read".into()],
            family_id: None,
        };
        let reply = call(&state, &b, "fauna.capabilities.fetch", fetch())
            .await
            .unwrap();
        let reply: FetchGrantsReply = fauna_protocol::decode_strict(&reply).unwrap();
        assert!(reply.grants.is_empty());
    }

    // ── The `records` arm's doors (`third-party-kinds.md` § The record doors) ──

    const WRITER: [u8; 32] = [0x57; 32];
    const NOTES: &str = "ext:ext.app.example.notes";
    const RECORDS: &str = "fauna:records:rw:ext.app.example.*";

    /// An account whose principal consented the `records` wildcard over its
    /// own publisher, attesting a writer key, with a manifest declaring one
    /// kind.
    async fn records_fixture(token_scopes: &[&str]) -> (Arc<AppState>, PrincipalBinding) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        db.record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            CLIENT,
            Some("Example App"),
            RECORDS,
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(HOLDER),
                    writer_ed25519: Some(WRITER),
                },
                execution_form: ExecutionForm::Device,
                manifest: Some(crate::db::third_party_principals::PrincipalManifest {
                    publisher_key: [0x9B; 32],
                    declared_kinds: vec!["ext.app.example.notes".into()],
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let state = Arc::new(AppState::for_test(db));
        let b = PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: token_scopes.iter().map(|s| s.to_string()).collect(),
            family_id: None,
        };
        (state, b)
    }

    /// A v1-shaped envelope: the version byte, a nonce's and a tag's worth of
    /// bytes. The nest never opens it, so its shape is all a door can check.
    fn v1_envelope() -> Vec<u8> {
        let mut envelope = vec![fauna_core::account_entry_crypto::SEALED_ENTRY_V1];
        envelope.extend_from_slice(&[0x5e; 12 + 16 + 8]);
        envelope
    }

    fn put_entry(scope: &str, writer: &[u8; 32], seq: i64, entry: Vec<u8>) -> Bytes {
        fauna_protocol::encode_canonical(&fauna_protocol::account_state::AccountStatePutRequest {
            scope: scope.into(),
            writer_id: hex::encode(writer),
            writer_seq: seq,
            item_key: fauna_protocol::ByteBuf::from(vec![0x11; 32]),
            op: fauna_protocol::account_state::OP_STATE_PUT.into(),
            entry: fauna_protocol::ByteBuf::from(entry),
            ..Default::default()
        })
        .unwrap()
    }

    fn put(scope: &str, writer: &[u8; 32], seq: i64) -> Bytes {
        put_entry(scope, writer, seq, v1_envelope())
    }

    /// The floor: a principal's row must be shaped as the gen-0 envelope its
    /// `ext.*` kind seals to — whichever door it came through, since both
    /// dispatch onto this handler.
    #[tokio::test]
    async fn a_principal_row_that_is_not_a_v1_envelope_is_refused() {
        let (state, b) = records_fixture(&[RECORDS]).await;
        let mut v2 = v1_envelope();
        v2[0] = fauna_core::account_entry_crypto::SEALED_ENTRY_V2;
        for (entry, why) in [(b"sealed".to_vec(), "too short"), (v2, "a v2 envelope")] {
            let err = call(
                &state,
                &b,
                "fauna.account.state.put",
                put_entry(NOTES, &WRITER, 1, entry),
            )
            .await
            .expect_err(why);
            assert!(err.code.ends_with("invalid_request"), "{why}: {}", err.code);
        }
        let reply = call(&state, &b, "fauna.sync.changes.list", listed(NOTES))
            .await
            .expect("its own scope lists");
        assert_eq!(rows(&reply), 0, "a refused row lands nowhere");
    }

    fn list(scope: &str) -> fauna_protocol::sync::SyncChangesListRequest {
        fauna_protocol::sync::SyncChangesListRequest {
            scope: Some(scope.into()),
            item_class: Some(
                fauna_protocol::account_state::ItemClass::StateEntry
                    .as_wire()
                    .into(),
            ),
            ..Default::default()
        }
    }

    fn listed(scope: &str) -> Bytes {
        fauna_protocol::encode_canonical(&list(scope)).unwrap()
    }

    fn rows(reply: &Bytes) -> usize {
        let reply: fauna_protocol::sync::SyncChangesListReply =
            fauna_protocol::decode_strict(reply).unwrap();
        reply.changes.len()
    }

    /// The headline: a principal writes a row of its own kind as its own
    /// writer and lists it back — and nothing else, by any spelling.
    #[tokio::test]
    async fn a_principal_writes_and_lists_exactly_its_own_kinds() {
        let (state, b) = records_fixture(&[RECORDS]).await;
        call(
            &state,
            &b,
            "fauna.account.state.put",
            put(NOTES, &WRITER, 1),
        )
        .await
        .expect("its own kind, as its own writer");
        let reply = call(&state, &b, "fauna.sync.changes.list", listed(NOTES))
            .await
            .expect("its own scope lists");
        assert_eq!(rows(&reply), 1);

        for (payload, why) in [
            (put(NOTES, &[0x58; 32], 2), "another writer"),
            (put("state", &WRITER, 2), "the account's own state"),
            (put("state-fleet", &WRITER, 2), "the fleet scope"),
            (
                put("ext:ext.other.org.thing", &WRITER, 2),
                "another publisher's kind",
            ),
            (
                put("ext:ext.example.app.notes", &WRITER, 2),
                "a publisher spelled the other way round",
            ),
        ] {
            let err = call(&state, &b, "fauna.account.state.put", payload)
                .await
                .expect_err(why);
            assert_eq!(err.code, "fauna.account.permission_denied", "{why}");
        }
        for scope in ["state", "state-fleet", "ext:ext.other.org.thing"] {
            let err = call(&state, &b, "fauna.sync.changes.list", listed(scope))
                .await
                .expect_err(scope);
            assert_eq!(err.code, "fauna.sync.permission_denied", "{scope}");
        }
        // The actor door's other selectors are not the principal's.
        let custody = fauna_protocol::sync::SyncChangesListRequest {
            of_owner: Some(hex::encode(OTHER)),
            ..list(NOTES)
        };
        let err = call(
            &state,
            &b,
            "fauna.sync.changes.list",
            fauna_protocol::encode_canonical(&custody).unwrap(),
        )
        .await
        .expect_err("no custody pull");
        assert_eq!(err.code, "fauna.sync.invalid_request");
    }

    /// A token whose family was consented without the arm reaches neither
    /// door — the gate's half, before any handler.
    #[tokio::test]
    async fn a_token_without_the_records_arm_reaches_neither_door() {
        let (state, b) = records_fixture(&["openid"]).await;
        let err = call(
            &state,
            &b,
            "fauna.account.state.put",
            put(NOTES, &WRITER, 1),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.account.permission_denied");
        let err = call(&state, &b, "fauna.sync.changes.list", listed(NOTES))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.sync.permission_denied");
    }

    /// Revoking the principal closes its doors at once, and the user's own
    /// replicas still read the rows it wrote: the scope stays served because
    /// it exists (`third-party-kinds.md` § The `ext` sub-scope).
    #[tokio::test]
    async fn after_revoke_the_owner_still_reads_the_principals_rows() {
        use crate::sync_handlers::serve_account_state_feed;
        use fauna_protocol::account_state::ItemClass;
        let (state, b) = records_fixture(&[RECORDS]).await;
        call(
            &state,
            &b,
            "fauna.account.state.put",
            put(NOTES, &WRITER, 1),
        )
        .await
        .unwrap();
        state
            .db
            .revoke_third_party_principal(&ACCOUNT, &b.principal_id)
            .await
            .unwrap()
            .unwrap();
        let err = call(
            &state,
            &b,
            "fauna.account.state.put",
            put(NOTES, &WRITER, 2),
        )
        .await
        .unwrap_err();
        assert_eq!(err, central_permission_denied());
        let state_feed = ItemClass::StateEntry.as_wire();
        let owner = serve_account_state_feed(&state, &ACCOUNT, state_feed, &list(NOTES))
            .await
            .expect("the owner's replica still reads the scope");
        assert_eq!(rows(&owner), 1);
        // A kind no row declares and no row ever created is not served.
        let tags = list("ext:ext.app.example.tags");
        let err = serve_account_state_feed(&state, &ACCOUNT, state_feed, &tags)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.sync.invalid_request");
    }

    // ── The fifth arm (`authorization-server.md` § Scope grammar → *The
    // fifth arm*) ──

    const PERSONALIZATION: &str = "fauna:personalization:rw";

    async fn personalization_fixture(
        scopes: &str,
        token_scopes: &[&str],
    ) -> (Arc<AppState>, PrincipalBinding) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        consent(&db, &ACCOUNT, b"family-1", scopes).await;
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let state = Arc::new(AppState::for_test(db));
        let binding = PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: token_scopes.iter().map(|s| s.to_string()).collect(),
            family_id: None,
        };
        (state, binding)
    }

    const TOPIC: &str = "topic:c4c4c4c4c4c4c4c4c4c4c4c4c4c4c4c4";

    fn model_put(factor: &str, blob: &[u8]) -> Bytes {
        fauna_protocol::encode_canonical(
            &fauna_protocol::personalization::PersonalizationModelPutRequest {
                factor: factor.into(),
                sealed_blob: fauna_protocol::ByteBuf::from(blob.to_vec()),
                sample_count: 3,
                extra: Default::default(),
            },
        )
        .unwrap()
    }

    fn model_fetch() -> Bytes {
        fauna_protocol::encode_canonical(
            &fauna_protocol::personalization::PersonalizationModelFetchRequest {
                factor: TOPIC.into(),
                extra: Default::default(),
            },
        )
        .unwrap()
    }

    /// The headline: a principal holding the arm trains and reads the
    /// account's model — the account's own fetch sees what it put — and lists
    /// `state` read only, while `delete`, the `state` put and every other
    /// list spelling refuse.
    #[tokio::test]
    async fn the_personalization_arm_reads_and_trains_the_accounts_models() {
        use fauna_protocol::personalization::{
            KIND_MODEL_DELETE, KIND_MODEL_FETCH, KIND_MODEL_PUT, PersonalizationModelDeleteRequest,
            PersonalizationModelFetchReply,
        };
        let (state, b) = personalization_fixture(PERSONALIZATION, &[PERSONALIZATION]).await;
        call(&state, &b, KIND_MODEL_PUT, model_put(TOPIC, b"sealed"))
            .await
            .expect("the arm trains");
        let reply = call(&state, &b, KIND_MODEL_FETCH, model_fetch())
            .await
            .expect("the arm reads");
        let reply: PersonalizationModelFetchReply = fauna_protocol::decode_strict(&reply).unwrap();
        assert_eq!(
            reply.sealed_blob.map(|v| v.into_vec()),
            Some(b"sealed".to_vec())
        );
        // It is the account's row, not a row of the principal's own.
        let owned = state
            .db
            .get_personalization_model(&ACCOUNT, TOPIC)
            .await
            .unwrap();
        assert_eq!(owned.map(|(blob, ..)| blob), Some(b"sealed".to_vec()));
        // The put's envelope checks are the account's own.
        let err = call(&state, &b, KIND_MODEL_PUT, model_put("label:x", b"sealed"))
            .await
            .unwrap_err();
        assert!(
            err.code.starts_with("fauna.personalization."),
            "{}",
            err.code
        );

        let delete = fauna_protocol::encode_canonical(&PersonalizationModelDeleteRequest {
            factor: TOPIC.into(),
            extra: Default::default(),
        })
        .unwrap();
        let err = call(&state, &b, KIND_MODEL_DELETE, delete)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.personalization.permission_denied");

        let reply = call(&state, &b, "fauna.sync.changes.list", listed("state"))
            .await
            .expect("the arm lists state");
        assert_eq!(rows(&reply), 0);
        let err = call(
            &state,
            &b,
            "fauna.account.state.put",
            put("state", &WRITER, 1),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.account.permission_denied");
        for scope in ["state-fleet", NOTES, "", "State"] {
            let err = call(&state, &b, "fauna.sync.changes.list", listed(scope))
                .await
                .expect_err(scope);
            assert_eq!(err.code, "fauna.sync.permission_denied", "{scope:?}");
        }
    }

    /// The dispatch both ways: a session holding only `records` lists its
    /// `ext:` scope but not `state`; a token without the fifth arm reaches
    /// none of the arm's doors even when the row holds it.
    #[tokio::test]
    async fn the_list_dispatches_on_the_scope_to_the_arm_that_names_it() {
        use fauna_protocol::personalization::KIND_MODEL_FETCH;
        let (state, b) =
            personalization_fixture(&format!("{PERSONALIZATION} {RECORDS}"), &[RECORDS]).await;
        let err = call(&state, &b, "fauna.sync.changes.list", listed("state"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.sync.permission_denied");
        let err = call(&state, &b, KIND_MODEL_FETCH, model_fetch())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.personalization.permission_denied");

        let (state, b) = personalization_fixture(PERSONALIZATION, &[PERSONALIZATION]).await;
        let err = call(&state, &b, "fauna.sync.changes.list", listed(NOTES))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.sync.permission_denied");
        call(&state, &b, KIND_MODEL_FETCH, model_fetch())
            .await
            .expect("an absent model is the empty shape");
    }

    // ── The `follow:write` arm (`authorization-server.md` § Scope grammar →
    // *The sixth arm*) ──

    use fauna_core::subscription::FOLLOWERS_TIER;
    use fauna_protocol::subscriptions::{
        StatusGetReply, StatusGetRequest, SubscribeReply, SubscribeRequest, UnsubscribeReply,
        UnsubscribeRequest,
    };

    const AUTHOR: [u8; 32] = [0xC3; 32];
    const EK: [u8; 1184] = [0x5E; 1184];

    /// An account whose principal consented `fauna:follow:write`, a hosted
    /// author, and the account's registry entry published when `published`.
    async fn follow_fixture(published: bool) -> (Arc<AppState>, PrincipalBinding) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&ACCOUNT, "free", "test").await.unwrap();
        db.create_user(&AUTHOR, "free", "test").await.unwrap();
        consent(&db, &ACCOUNT, b"family-1", "fauna:follow:write openid").await;
        if published {
            db.put_subscriber_encaps_key(&ACCOUNT, &EK).await.unwrap();
        }
        let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
            .principal_id
            .clone();
        let state = Arc::new(AppState::for_test(db));
        let binding = PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: vec!["fauna:follow:write".into(), "openid".into()],
            family_id: None,
        };
        (state, binding)
    }

    fn subscribe(tier: &str) -> Bytes {
        fauna_protocol::encode_canonical(&SubscribeRequest {
            author_id: fauna_core::identity::ActorId(AUTHOR),
            tier: tier.into(),
            mlkem_encaps_key: None,
            extra: Default::default(),
        })
        .unwrap()
    }

    fn unsubscribe() -> Bytes {
        fauna_protocol::encode_canonical(&UnsubscribeRequest {
            author_id: fauna_core::identity::ActorId(AUTHOR),
            extra: Default::default(),
        })
        .unwrap()
    }

    async fn status(state: &Arc<AppState>, b: &PrincipalBinding) -> StatusGetReply {
        let p = fauna_protocol::encode_canonical(&StatusGetRequest {
            author_id: fauna_core::identity::ActorId(AUTHOR),
            extra: Default::default(),
        })
        .unwrap();
        let reply = call(state, b, "fauna.subscriptions.status.get", p)
            .await
            .expect("status");
        fauna_protocol::decode_strict(&reply).unwrap()
    }

    async fn tier(state: &Arc<AppState>, name: &str, rank: i64) {
        state
            .db
            .create_subscription_tier(
                &AUTHOR, name, rank, None, None, None, true, None, None, false,
            )
            .await
            .unwrap();
    }

    /// The arm covers exactly the three subscription kinds — no `mine.list`,
    /// no author-side kind, and never the registry publish.
    #[test]
    fn the_follow_arm_covers_exactly_its_three_kinds() {
        use crate::bridge_method_allowlist::PrincipalReach;
        use fauna_bridge_atproto::fauna_scope::FaunaScopeArm;
        let covered: Vec<&str> = principal_handler_kinds()
            .filter(|k| {
                principal_reach(k) == Some(PrincipalReach::Scope(FaunaScopeArm::FollowWrite))
            })
            .collect();
        assert_eq!(
            covered,
            [
                "fauna.subscriptions.status.get",
                "fauna.subscriptions.subscribe",
                "fauna.subscriptions.unsubscribe",
            ]
        );
        for kind in [
            "fauna.subscriptions.mine.list",
            "fauna.subscriptions.encaps_key.publish",
            "fauna.subscriptions.requests.approve",
        ] {
            assert!(!is_permitted(CallerClass::ThirdParty, kind), "{kind}");
        }
    }

    /// A follow carries the account's published key, not the principal's, and
    /// reads back as pending; any tier but `followers` is refused.
    #[tokio::test]
    async fn a_principal_follows_with_the_published_key_and_only_the_followers_tier() {
        let (state, b) = follow_fixture(true).await;
        tier(&state, "gold", 1).await;
        let err = call(
            &state,
            &b,
            "fauna.subscriptions.subscribe",
            subscribe("gold"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.subscriptions.permission_denied");

        let reply = call(
            &state,
            &b,
            "fauna.subscriptions.subscribe",
            subscribe(FOLLOWERS_TIER),
        )
        .await
        .expect("a follow");
        let reply: SubscribeReply = fauna_protocol::decode_strict(&reply).unwrap();
        assert!(matches!(reply, SubscribeReply::Queued { .. }), "{reply:?}");
        let rows = state.db.list_subscribe_requests(&AUTHOR).await.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].tier_name, FOLLOWERS_TIER);
        assert_eq!(rows[0].mlkem_encaps_key.as_deref(), Some(&EK[..]));

        let s = status(&state, &b).await;
        assert_eq!((s.tier, s.follow_pending), (None, true));
    }

    /// No registry entry, no follow — never a classical degrade, nothing
    /// enqueued.
    #[tokio::test]
    async fn a_follow_without_a_published_key_is_refused() {
        let (state, b) = follow_fixture(false).await;
        let err = call(
            &state,
            &b,
            "fauna.subscriptions.subscribe",
            subscribe(FOLLOWERS_TIER),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.subscriptions.pq_key_unpublished");
        assert!(
            state
                .db
                .list_subscribe_requests(&AUTHOR)
                .await
                .unwrap()
                .is_empty()
        );
        let s = status(&state, &b).await;
        assert_eq!((s.tier, s.follow_pending), (None, false));
    }

    /// While a paid tier is held the principal's status names only
    /// `followers` and its unfollow is refused; with `followers` alone the
    /// unfollow enqueues the followers removal.
    #[tokio::test]
    async fn a_principal_unfollows_only_while_followers_is_the_sole_tier() {
        let (state, b) = follow_fixture(true).await;
        tier(&state, FOLLOWERS_TIER, 0).await;
        tier(&state, "gold", 1).await;
        for t in [FOLLOWERS_TIER, "gold"] {
            state
                .db
                .add_subscriber(&AUTHOR, &ACCOUNT, t, None)
                .await
                .unwrap();
        }

        let s = status(&state, &b).await;
        assert_eq!(
            s.tier.as_deref(),
            Some(FOLLOWERS_TIER),
            "never the paid tier"
        );
        assert_eq!((s.expires_at, s.follow_pending), (None, false));
        let err = call(&state, &b, "fauna.subscriptions.unsubscribe", unsubscribe())
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.subscriptions.permission_denied");
        assert!(
            state
                .db
                .list_subscribe_requests(&AUTHOR)
                .await
                .unwrap()
                .is_empty()
        );

        state
            .db
            .remove_subscriber(&AUTHOR, &ACCOUNT, "gold")
            .await
            .unwrap();
        let reply = call(&state, &b, "fauna.subscriptions.unsubscribe", unsubscribe())
            .await
            .expect("an unfollow");
        let reply: UnsubscribeReply = fauna_protocol::decode_strict(&reply).unwrap();
        assert!(
            matches!(reply, UnsubscribeReply::Queued { .. }),
            "{reply:?}"
        );
        let rows = state.db.list_subscribe_requests(&AUTHOR).await.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].kind.as_str(), rows[0].tier_name.as_str()),
            ("unsubscribe", FOLLOWERS_TIER)
        );
    }

    /// The two writes share the principal's `RateWindow`: past the oracle's
    /// constant the next is refused; the read is never ceilinged.
    #[tokio::test]
    async fn the_follow_writes_are_rate_ceilinged() {
        let (state, b) = follow_fixture(true).await;
        for _ in 0..fauna_core::identity_op::OPS_PER_WINDOW {
            call(
                &state,
                &b,
                "fauna.subscriptions.subscribe",
                subscribe(FOLLOWERS_TIER),
            )
            .await
            .expect("inside the window");
        }
        let err = call(
            &state,
            &b,
            "fauna.subscriptions.subscribe",
            subscribe(FOLLOWERS_TIER),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.subscriptions.rate_limited");
        status(&state, &b).await;
    }

    /// A session without the arm reaches none of the three kinds.
    #[tokio::test]
    async fn a_session_without_the_arm_cannot_follow() {
        let (state, b) = fixture(&["fauna:feed:read", "openid"]).await;
        let err = call(
            &state,
            &b,
            "fauna.subscriptions.subscribe",
            subscribe(FOLLOWERS_TIER),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.subscriptions.permission_denied");
    }
}
