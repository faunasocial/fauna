//! Tier_3: the oracle, posting class (TP11 — `docs/goal/architecture/
//! key-material-hierarchy.md` § Audience: deployment infrastructure → *The
//! oracle*, the `post.author` row; the key: `atproto-pds-full.md` § D10).
//!
//! A third-party principal holding a keyless `identity.op:post.author` grant
//! authors public posts as the account through `fauna.posts.author.create`
//! and retracts its own through `fauna.posts.author.delete`, on its
//! principal session. Proven here, in process, end to end through the
//! principal dispatch chokepoint:
//!
//!  * no live grant → refused; a grant but no delegation → refused;
//!  * under both, a create lands a post that the account's local feed serves
//!    and whose stored wire verifies as DELEGATED through `K`'s cert chain —
//!    the reader-side check every nest and app runs;
//!  * a create carrying an audience member (`gated`) is refused, and so is an
//!    interaction reference;
//!  * the principal deletes its own post; a post it did not author — another
//!    principal's, or an id it never made — is refused;
//!  * after the owner revokes the delegation (destroying `K`), the next create
//!    is refused;
//!  * the owner's roster reads serve this custodian's record, and revoking the
//!    principal deletes it.

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::data::{Capability, DeviceAuthorization, PostBody, Reference, Timestamp};
use fauna_core::encoding::{
    AuthoringOrigin, EmbedAsBytes, canonical_decode, canonical_encode, decode_signed_bytes,
    sign_envelope, verify_authoring_envelope,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::identity_op::IdentityOpClass;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use fauna_nest::principal_handlers::{PrincipalBinding, dispatch_principal};
use fauna_nest::routes::AppState;
use fauna_protocol::feed::{FeedLocalPostsReply, FeedLocalPostsRequest};
use fauna_protocol::post_author::{
    KIND_CREATE, KIND_DELETE, PostAuthorCreateReply, PostAuthorCreateRequest,
    PostAuthorDeleteReply, PostAuthorDeleteRequest,
};

const SCOPE: &str = "fauna:identity:op:post.author";
const FEED: &str = "fauna:feed:read";

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    Arc::new(state)
}

fn now_secs() -> u64 {
    Timestamp::now_secs() as u64
}

/// One consented principal: its roster row (attested key, the class and the
/// feed read in scope) and the binding its session dials with.
async fn consent(
    state: &AppState,
    account: [u8; 32],
    client: &str,
    holder: [u8; 32],
) -> PrincipalBinding {
    let scopes = format!("{SCOPE} {FEED}");
    state
        .db
        .record_atproto_oauth_grant(
            &account,
            client.as_bytes(),
            client,
            Some("Poster"),
            &scopes,
            &[],
            "jkt",
            i64::MAX,
            None,
            fauna_nest::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(holder),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Remote,
                manifest: Some(PrincipalManifest {
                    publisher_key: [0x9B; 32],
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    let principal_id = state
        .db
        .list_third_party_principals(&account)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.client_id == client)
        .expect("the principal row")
        .principal_id;
    PrincipalBinding {
        account,
        principal_id,
        token_scopes: vec![SCOPE.to_string(), FEED.to_string()],
        family_id: None,
    }
}

/// The owner's device mints the keyless `identity.op:post.author` grant.
async fn mint_grant(state: &AppState, account: [u8; 32], grant_id: [u8; 16], holder: [u8; 32]) {
    let now = now_secs();
    let blob = fauna_mls::wrapped_blob::build_grant_blob(
        &account,
        &grant_id,
        &holder,
        None,
        GrantWindow(now - 1, now + 3600),
        &[(
            fauna_client_capabilities::identity_op_scope(IdentityOpClass::PostAuthor),
            None,
        )],
    )
    .expect("a keyless grant builds")
    .to_canonical_bytes()
    .expect("encode");
    state
        .db
        .put_capability_grant(&account, &grant_id, &holder, (now + 3600) as i64, &blob)
        .await
        .unwrap();
}

/// The D10 ceremony the approving app runs: fetch (mint) `K_pub`, sign the
/// identity cert over it, provision it.
async fn provision_delegation(state: &AppState, identity: &ActorKeypair) {
    let account = identity.actor_id().0;
    let k_pub = fauna_nest::atproto_authoring_key::mint_or_fetch(&state.db, &account)
        .await
        .unwrap();
    let cert = DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: k_pub,
        capabilities: vec![Capability::Post],
        created_at: Timestamp::now(),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(identity, &cert).unwrap();
    let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
    fauna_nest::atproto_authoring_key::verify_delegation_cert(
        &wire,
        &account,
        &k_pub,
        Timestamp::now().0,
    )
    .expect("the cert the ceremony builds is one the provision door accepts");
    assert!(
        state
            .db
            .set_atproto_authoring_key_cert(&account, &wire)
            .await
            .unwrap()
    );
}

fn text(content: &str) -> PostAuthorCreateRequest {
    PostAuthorCreateRequest {
        body: PostBody::Text {
            content: content.into(),
            facets: vec![],
        },
        references: vec![],
        content_warning: None,
        extra: Default::default(),
    }
}

async fn create(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    req: &PostAuthorCreateRequest,
) -> Result<String, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(req).unwrap();
    let reply: Bytes = dispatch_principal(state.clone(), binding, KIND_CREATE, payload).await?;
    let reply: PostAuthorCreateReply = fauna_protocol::decode_strict(&reply).unwrap();
    Ok(reply.post_id)
}

async fn delete(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    post_id: &str,
) -> Result<PostAuthorDeleteReply, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(&PostAuthorDeleteRequest {
        post_id: post_id.to_string(),
        ..Default::default()
    })
    .unwrap();
    let reply: Bytes = dispatch_principal(state.clone(), binding, KIND_DELETE, payload).await?;
    Ok(fauna_protocol::decode_strict(&reply).unwrap())
}

async fn local_post_ids(state: &Arc<AppState>, binding: &PrincipalBinding) -> Vec<String> {
    let payload = fauna_protocol::encode_canonical(&FeedLocalPostsRequest {
        cursor: None,
        limit: Some(50),
        search: None,
        extra: Default::default(),
    })
    .unwrap();
    let reply: Bytes =
        dispatch_principal(state.clone(), binding, "fauna.feed.local.posts", payload)
            .await
            .expect("the principal's feed read");
    let reply: FeedLocalPostsReply = fauna_protocol::decode_strict(&reply).unwrap();
    reply.posts.into_iter().map(|p| p.post_id).collect()
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_principal_authors_and_retracts_public_posts_through_the_authoring_key() {
    let state = state().await;
    let identity = ActorKeypair::generate();
    let account = identity.actor_id().0;
    state
        .db
        .create_user(&account, "free", "test")
        .await
        .unwrap();
    let poster = consent(
        &state,
        account,
        "https://poster.example/client.json",
        [0x77; 32],
    )
    .await;
    let other = consent(
        &state,
        account,
        "https://other.example/client.json",
        [0x78; 32],
    )
    .await;

    // No grant yet: refused.
    let err = create(&state, &poster, &text("too early"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");

    // A grant, but the account has no delegation: refused.
    mint_grant(&state, account, [0x0D; 16], [0x77; 32]).await;
    mint_grant(&state, account, [0x0E; 16], [0x78; 32]).await;
    let err = create(&state, &poster, &text("no key yet"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");

    provision_delegation(&state, &identity).await;
    let post_id = create(&state, &poster, &text("posted through the oracle"))
        .await
        .expect("a public post is authored under the grant and the delegation");
    assert!(
        local_post_ids(&state, &poster).await.contains(&post_id),
        "the account's local feed serves the authored post"
    );

    // The stored wire verifies as DELEGATED through K's cert chain, authored
    // by the account.
    let digest: [u8; 32] = hex::decode(&post_id).unwrap().try_into().unwrap();
    let stored =
        fauna_nest::segments::post::load_post_body(&state.post_segments, &state.db, &digest)
            .await
            .unwrap()
            .expect("the post's stored wire");
    let wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
    let signer_auth = wire.signer_auth.clone();
    let (bytes, env) = wire.into_signed().unwrap();
    let post: fauna_core::data::Post = decode_signed_bytes(&bytes).unwrap();
    assert_eq!(post.author.0, account);
    assert!(post.gated.is_none());
    let origin = verify_authoring_envelope(
        &post,
        &bytes,
        &env,
        signer_auth.as_deref(),
        &Capability::Post,
        post.created_at,
    )
    .expect("the delegated chain verifies");
    assert!(
        matches!(origin, AuthoringOrigin::Delegated { .. }),
        "{origin:?}"
    );

    // An audience member, and an interaction reference: refused.
    let mut gated = text("for subscribers");
    gated
        .extra
        .insert("gated".into(), fauna_protocol::Value::Bool(true));
    let err = create(&state, &poster, &gated).await.unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");
    let mut like = text("");
    like.references = vec![Reference::Upvote {
        post_id: fauna_cbor::Cid::from_digest_dag_cbor(digest),
    }];
    let err = create(&state, &poster, &like).await.unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");

    // Another principal cannot retract this one's post, nor any post it did
    // not author.
    let err = delete(&state, &other, &post_id).await.unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");
    let err = delete(&state, &poster, &"ab".repeat(32)).await.unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");

    // Its own: retracted, and gone from the feed.
    let reply = delete(&state, &poster, &post_id)
        .await
        .expect("the principal retracts its own post");
    assert!(reply.deleted);
    assert!(!local_post_ids(&state, &poster).await.contains(&post_id));
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM post_author_ops").await,
        2,
        "one record row per performed operation; refusals record none"
    );
    let ops = state
        .db
        .list_principal_oracle_ops(&account, &poster.principal_id, 10)
        .await
        .unwrap();
    let details: Vec<(&str, String)> = ops
        .iter()
        .map(|o| (o.class.as_str(), o.detail.clone()))
        .collect();
    assert_eq!(
        details,
        [
            ("post.author", format!("delete {post_id}")),
            ("post.author", format!("create {post_id}")),
        ]
    );

    // The owner revokes the delegation — K is destroyed — and the next
    // create is refused.
    assert!(
        state
            .db
            .delete_atproto_authoring_key(&account)
            .await
            .unwrap()
    );
    let err = create(&state, &poster, &text("after the revoke"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.posts.permission_denied", "{err:?}");

    // Revoking the principal takes its record with it.
    state
        .db
        .revoke_third_party_principal(&account, &poster.principal_id)
        .await
        .unwrap()
        .expect("the principal existed");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM post_author_ops").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM post_author_principals").await,
        0
    );
}
