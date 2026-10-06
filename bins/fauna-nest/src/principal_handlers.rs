//! The **principal** side of WS-RPC dispatch: what a third-party principal's
//! session may call, and the handlers that serve it
//! (`docs/goal/architecture/apps/bridges.md` § Capability-allowlist enforcement
//! → *How the class meets the connection*).
//!
//! A principal session carries a [`PrincipalBinding`] — the account, the
//! principal id and the scopes of the token the session was opened with — and
//! the zero actor. Every call it makes runs [`dispatch_principal`], which:
//!
//! 1. **resolves** the binding per call ([`resolve_principal`]): the principal
//!    row must still exist and the account must still have authority, or the
//!    call answers the every-kind refusal — so `fauna.principals.revoke`, a
//!    suspension or a lockout bites at the session's next call;
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
//! `bridge_blob_handlers`, `nostr::bunker_handlers`, `folder_deposit`); this
//! module owns the table and the gate.

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
/// when the principal row is gone (revoked), the account no longer has
/// authority (deleted, suspended, locked out), or the account's external-apps
/// switch is OFF (`atproto-pds-full.md` § F1 detail, the kill-switch bullet: a
/// principal is an external app, and OFF suspends them all).
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
        // The `events` arm's kind (`transport.md` § Push events →
        // *Third-party event doors*) — the HTTP long-poll dispatches here too.
        table.insert(
            fauna_protocol::push_events::KIND_EVENTS_POLL,
            crate::events_doors::principal_events_poll_handler(),
        );
        // The oracle's door (TP11). Gated with the bunker cluster it binds
        // into: without the `nostr` feature the actor router carries no
        // `fauna.nostr.bunker.*` kind, so the ceiling the sweep test reads
        // has no `bind` either.
        #[cfg(feature = "nostr")]
        table.insert(
            "fauna.nostr.bunker.bind",
            crate::nostr::bunker_handlers::principal_bind_handler(),
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
            ("fauna.feed.posts", "fauna.feed.permission_denied"),
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
}
