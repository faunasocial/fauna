//! End-to-end conformance for the production recipient-routing path: a
//! member's own `fauna.bridges.create_account_alias` (User write) feeds
//! `fauna.bridges.validate_recipient` (MTA read) through the
//! `account_aliases` table — plus the member's revoke / enable / delete /
//! update doors and the canonical-address protection on them.
//!
//! `docs/goal/behavior/mail-aliases.md` § Storage owns the row shape;
//! § Five alias kinds owns the taxonomy.

mod common;
use common::register_user;

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::{bridge_routing_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    bridge_routing::{
        AliasControls, AliasRow, CreateAccountAliasReply, CreateAccountAliasRequest,
        DeleteAccountAliasReply, DeleteAccountAliasRequest, EnableAccountAliasReply,
        EnableAccountAliasRequest, ListAccountAliasesReply, ListAccountAliasesRequest,
        RevokeAccountAliasReply, RevokeAccountAliasRequest, UpdateAccountAliasReply,
        UpdateAccountAliasRequest, ValidateRecipientReply, ValidateRecipientRequest,
    },
    decode_strict as decode, encode_canonical,
};
use serde_bytes::ByteBuf;

async fn router_with_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

async fn approve_mta(state: &Arc<AppState>, bridge_actor: [u8; 32]) {
    state
        .db
        .create_pending_bridge_service_user(
            &bridge_actor,
            fauna_nest::db::bridge_service_users::BridgeRole::Mta,
            "mta-1",
        )
        .await
        .unwrap();
    state
        .db
        .upsert_bridge_x25519(&bridge_actor, &[1u8; 32])
        .await
        .unwrap();
    state
        .db
        .approve_bridge_service_user(&bridge_actor, None)
        .await
        .unwrap();
}

fn validate_payload(local_part: &str, domain: &str) -> Bytes {
    let req = ValidateRecipientRequest {
        local_part: local_part.into(),
        domain: domain.into(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// With an empty table the MTA's `validate_recipient` rejects every RCPT TO;
/// after the member creates the alias through their own write it resolves.
#[tokio::test]
async fn validate_recipient_rejects_then_resolves_after_member_creates_alias() {
    let (router, state) = router_with_state().await;
    let mta = [55u8; 32];
    approve_mta(&state, mta).await;
    let recipient = [77u8; 32];
    register_user(&state, recipient, "bob").await;

    // No alias rows yet → Reject.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        mta,
        "fauna.bridges.validate_recipient",
        validate_payload("alice", "example.com"),
    )
    .await
    .expect("validate_recipient with no rows must not error");
    let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
    assert!(
        matches!(reply, ValidateRecipientReply::Reject { .. }),
        "expected Reject before any alias exists, got {reply:?}",
    );

    // The member writes their own alias.
    create_user_alias(&router, state.clone(), recipient, "example.com", "alice").await;

    // The MTA now resolves the same RCPT TO.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        mta,
        "fauna.bridges.validate_recipient",
        validate_payload("alice", "example.com"),
    )
    .await
    .expect("validate_recipient after create_account_alias ok");
    let reply: ValidateRecipientReply = decode(&reply_bytes).unwrap();
    match reply {
        ValidateRecipientReply::Resolved { actor_id, .. } => {
            assert_eq!(actor_id, recipient.to_vec());
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
}

// ── User alias re-enable + canonical-address protection ─────────────
//
// `mail-aliases.md` § Disable: `revoke` is the *reversible* "soft
// intermediate" — flipping `disabled = true` MUST be undoable, else the
// nest can be driven into a client-causable unrecoverable state (a
// disabled `<handle>@<domain>` would 550-bounce all inbound forever with
// no client-side recovery — the live `test@example.com` manual-testing
// bug). So `fauna.bridges.enable_account_alias` is the reverse of revoke,
// and the canonical `<handle>@<domain>` exact alias (primary mailbox +
// AUTH identity) is protected from `revoke` / `delete` with
// `fauna.bridges.canonical_alias_protected`.
//
// These exercise the production WS-RPC handler path end-to-end: a User
// actor creates/revokes/enables/deletes its own rows, the disabled flag
// is read back over `list_account_aliases`, and the canonical guard
// resolves the protected row via the runtime primary mail domain.

/// Make `domain` the deployment's primary mail domain (the runtime
/// `local_domains` row `canonical_address_for_actor` reads — the only source now
/// that the legacy boot-time `state.email.domain` field is removed).
async fn add_primary_mail_domain(state: &Arc<AppState>, domain: &str) {
    state
        .db
        .add_mail_domain(domain, true, "testing", "none", None, None)
        .await
        .unwrap();
}

/// Create an owned `kind='exact'` alias via the User handler; returns its
/// 16-byte id.
async fn create_user_alias(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    local_domain: &str,
    pattern: &str,
) -> [u8; 16] {
    let req = CreateAccountAliasRequest {
        kind: "exact".into(),
        local_domain: local_domain.into(),
        pattern: pattern.into(),
        controls: AliasControls::default(),
    };
    let bytes = dispatch(
        router,
        state,
        actor,
        "fauna.bridges.create_account_alias",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("create_account_alias ok");
    let reply: CreateAccountAliasReply = decode(&bytes).unwrap();
    reply
        .alias_id
        .as_ref()
        .try_into()
        .expect("alias_id is 16 bytes")
}

fn alias_id_buf(alias_id: [u8; 16]) -> ByteBuf {
    ByteBuf::from(alias_id.to_vec())
}

/// Read back the `disabled` flag of `alias_id` over the User
/// `list_account_aliases` RPC (the production read path).
async fn alias_disabled(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    alias_id: [u8; 16],
) -> bool {
    let bytes = dispatch(
        router,
        state,
        actor,
        "fauna.bridges.list_account_aliases",
        Bytes::from(
            encode_canonical(&ListAccountAliasesRequest {})
                .unwrap()
                .to_vec(),
        ),
    )
    .await
    .expect("list_account_aliases ok");
    let reply: ListAccountAliasesReply = decode(&bytes).unwrap();
    reply
        .aliases
        .iter()
        .find(|r| r.alias_id.as_ref() == alias_id)
        .unwrap_or_else(|| panic!("alias {alias_id:?} present in list"))
        .disabled
}

/// Fetch the full `AliasRow` for `alias_id` over the production
/// `list_account_aliases` read path (panics if absent).
async fn alias_row(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    alias_id: [u8; 16],
) -> AliasRow {
    let bytes = dispatch(
        router,
        state,
        actor,
        "fauna.bridges.list_account_aliases",
        Bytes::from(
            encode_canonical(&ListAccountAliasesRequest {})
                .unwrap()
                .to_vec(),
        ),
    )
    .await
    .expect("list_account_aliases ok");
    let reply: ListAccountAliasesReply = decode(&bytes).unwrap();
    reply
        .aliases
        .into_iter()
        .find(|r| r.alias_id.as_ref() == alias_id)
        .unwrap_or_else(|| panic!("alias {alias_id:?} present in list"))
}

/// Update an owned alias over the production handler; returns the wire error
/// (if any) so the caller can assert ok-vs-rejected.
async fn update_user_alias(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    alias_id: [u8; 16],
    pattern: &str,
    label: &str,
) -> Result<(), fauna_protocol::RpcError> {
    let req = UpdateAccountAliasRequest {
        alias_id: alias_id_buf(alias_id),
        pattern: pattern.into(),
        controls: AliasControls {
            label: label.into(),
            ..Default::default()
        },
    };
    let bytes = dispatch(
        router,
        state,
        actor,
        "fauna.bridges.update_account_alias",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await?;
    assert!(decode::<UpdateAccountAliasReply>(&bytes).unwrap().ok);
    Ok(())
}

/// Whether `alias_id` is still present in the actor's alias list (a
/// `delete` removes the row).
async fn alias_present(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    alias_id: [u8; 16],
) -> bool {
    let bytes = dispatch(
        router,
        state,
        actor,
        "fauna.bridges.list_account_aliases",
        Bytes::from(
            encode_canonical(&ListAccountAliasesRequest {})
                .unwrap()
                .to_vec(),
        ),
    )
    .await
    .expect("list_account_aliases ok");
    let reply: ListAccountAliasesReply = decode(&bytes).unwrap();
    reply
        .aliases
        .iter()
        .any(|r| r.alias_id.as_ref() == alias_id)
}

/// (a) `revoke` then `enable` round-trips `disabled` on an owned
/// non-canonical alias — the reversibility the goal doc promises.
#[tokio::test]
async fn revoke_then_enable_round_trips_disabled() {
    let (router, state) = router_with_state().await;
    let user = [5u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    // A non-canonical alias (pattern != the handle localpart "alice").
    let alias = create_user_alias(&router, state.clone(), user, "example.com", "sales").await;
    assert!(
        !alias_disabled(&router, state.clone(), user, alias).await,
        "freshly created alias starts enabled",
    );

    // revoke → disabled = true.
    let bytes = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.revoke_account_alias",
        Bytes::from(
            encode_canonical(&RevokeAccountAliasRequest {
                alias_id: alias_id_buf(alias),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("revoke ok");
    assert!(decode::<RevokeAccountAliasReply>(&bytes).unwrap().ok);
    assert!(
        alias_disabled(&router, state.clone(), user, alias).await,
        "alias is disabled after revoke",
    );

    // enable → disabled = false again (the reverse; closes the one-way trap).
    let bytes = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.enable_account_alias",
        Bytes::from(
            encode_canonical(&EnableAccountAliasRequest {
                alias_id: alias_id_buf(alias),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("enable ok");
    assert!(decode::<EnableAccountAliasReply>(&bytes).unwrap().ok);
    assert!(
        !alias_disabled(&router, state.clone(), user, alias).await,
        "alias is live again after enable",
    );

    // enable is idempotent (`mail-aliases.md` § Disable).
    dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.enable_account_alias",
        Bytes::from(
            encode_canonical(&EnableAccountAliasRequest {
                alias_id: alias_id_buf(alias),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("enable is idempotent");
    assert!(!alias_disabled(&router, state.clone(), user, alias).await);
}

/// (b) `revoke` and `delete` reject the canonical `<handle>@<domain>`
/// alias with `canonical_alias_protected` — it's the primary mailbox +
/// AUTH identity, so it must stay alive (and the row is untouched).
#[tokio::test]
async fn revoke_and_delete_reject_canonical_alias() {
    let (router, state) = router_with_state().await;
    let user = [6u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    // The canonical address is `alice@example.com` (handle localpart on the
    // primary mail domain).
    let canonical = create_user_alias(&router, state.clone(), user, "example.com", "alice").await;

    let err = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.revoke_account_alias",
        Bytes::from(
            encode_canonical(&RevokeAccountAliasRequest {
                alias_id: alias_id_buf(canonical),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.canonical_alias_protected");

    let err = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.delete_account_alias",
        Bytes::from(
            encode_canonical(&DeleteAccountAliasRequest {
                alias_id: alias_id_buf(canonical),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.canonical_alias_protected");

    // The guard is non-destructive: the row is still present and enabled.
    assert!(
        alias_present(&router, state.clone(), user, canonical).await,
        "canonical alias survives the rejected delete",
    );
    assert!(
        !alias_disabled(&router, state.clone(), user, canonical).await,
        "canonical alias was never disabled",
    );
}

/// (c) `revoke` and `delete` on a *non-canonical* alias succeed — the
/// guard is scoped to the canonical row only, so ordinary aliases stay
/// fully managed.
#[tokio::test]
async fn revoke_and_delete_allow_non_canonical_alias() {
    let (router, state) = router_with_state().await;
    let user = [7u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    let revoked = create_user_alias(&router, state.clone(), user, "example.com", "sales").await;
    let deleted = create_user_alias(&router, state.clone(), user, "example.com", "promo").await;

    // revoke a non-canonical alias → ok + disabled.
    let bytes = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.revoke_account_alias",
        Bytes::from(
            encode_canonical(&RevokeAccountAliasRequest {
                alias_id: alias_id_buf(revoked),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("revoke non-canonical ok");
    assert!(decode::<RevokeAccountAliasReply>(&bytes).unwrap().ok);
    assert!(alias_disabled(&router, state.clone(), user, revoked).await);

    // delete a non-canonical alias → ok + gone.
    let bytes = dispatch(
        &router,
        state.clone(),
        user,
        "fauna.bridges.delete_account_alias",
        Bytes::from(
            encode_canonical(&DeleteAccountAliasRequest {
                alias_id: alias_id_buf(deleted),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("delete non-canonical ok");
    assert!(decode::<DeleteAccountAliasReply>(&bytes).unwrap().ok);
    assert!(
        !alias_present(&router, state.clone(), user, deleted).await,
        "deleted alias is gone from the list",
    );
}

/// (d) `list_account_aliases` marks the canonical `<handle>@<domain>` row
/// `is_canonical = true` and every other row `false`, so clients can render
/// it read-only (`mail-aliases.md` § Aliases UX — "marked as the primary
/// address"). The flag is computed from the runtime primary mail domain, not
/// stored — exactly the resolution the revoke/delete guard uses.
#[tokio::test]
async fn list_account_aliases_marks_canonical_row() {
    let (router, state) = router_with_state().await;
    let user = [8u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    // Canonical = the handle localpart "alice"; a second exact alias is not.
    let canonical = create_user_alias(&router, state.clone(), user, "example.com", "alice").await;
    let other = create_user_alias(&router, state.clone(), user, "example.com", "sales").await;

    assert!(
        alias_row(&router, state.clone(), user, canonical)
            .await
            .is_canonical,
        "the <handle>@<domain> row is flagged canonical",
    );
    assert!(
        !alias_row(&router, state.clone(), user, other)
            .await
            .is_canonical,
        "an ordinary alias is not flagged canonical",
    );
}

/// (e) `update_account_alias` rejects a **pattern change** on the canonical
/// alias with `canonical_alias_protected` — renaming the localpart would
/// strand AUTH login (`validate_recipient`), the same unrecoverable class as
/// disabling/deleting it. The row's pattern is left untouched.
#[tokio::test]
async fn update_rejects_canonical_pattern_change() {
    let (router, state) = router_with_state().await;
    let user = [9u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    let canonical = create_user_alias(&router, state.clone(), user, "example.com", "alice").await;

    let err = update_user_alias(&router, state.clone(), user, canonical, "alice2", "")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.canonical_alias_protected");

    // Non-destructive: the canonical address is unchanged.
    assert_eq!(
        alias_row(&router, state.clone(), user, canonical)
            .await
            .pattern,
        "alice",
        "the canonical pattern is untouched by the rejected update",
    );
}

/// (f) `update_account_alias` still **allows** editing the canonical row's
/// label/controls as long as the pattern is unchanged — the canonical is the
/// primary mailbox, not frozen; only its address (localpart) is protected.
#[tokio::test]
async fn update_allows_canonical_label_when_pattern_unchanged() {
    let (router, state) = router_with_state().await;
    let user = [10u8; 32];
    register_user(&state, user, "alice").await;
    add_primary_mail_domain(&state, "example.com").await;
    let canonical = create_user_alias(&router, state.clone(), user, "example.com", "alice").await;

    update_user_alias(&router, state.clone(), user, canonical, "alice", "Primary")
        .await
        .expect("label-only update of the canonical row is allowed");
    assert_eq!(
        alias_row(&router, state.clone(), user, canonical)
            .await
            .label,
        "Primary",
    );
}
