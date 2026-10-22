//! Tier_1: the oracle, profile class (TP11 — `docs/goal/architecture/
//! key-material-hierarchy.md` § Audience: deployment infrastructure → *The
//! oracle*, the `profile.author` row; the key: `atproto-pds-full.md` § D10).
//!
//! A third-party principal holding a keyless `identity.op:profile.author`
//! grant edits the display fields of the account's profile through
//! `fauna.profile.author.set`, on its principal session. Proven here, in
//! process, end to end through the principal dispatch chokepoint:
//!
//!  * no live grant → refused; a grant but no delegation → refused; a
//!    delegation that carries `Post` but not `UpdateProfile` → refused;
//!  * a `post.author` session never reaches the door at all — the reach is
//!    per class, not per custodian;
//!  * under both, a set lands a profile the read serves verbatim, which
//!    `decode_profile` reports as DELEGATED — the reader-side check every nest
//!    and app runs — with the named fields changed, the unnamed ones kept and
//!    every non-display field carried from the owner's own profile;
//!  * an avatar (for now) and a member this build does not name are refused;
//!  * after the owner revokes the delegation (destroying `K`), the next set
//!    is refused;
//!  * the owner's roster reads serve this custodian's record, and revoking the
//!    principal deletes it.

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::data::{
    Capability, DeviceAuthorization, InboxMode, NestEntry, NestRole, Profile, ProfileLink,
    Timestamp,
};
use fauna_core::encoding::{
    AuthoringOrigin, EmbedAsBytes, canonical_encode, decode_profile, sign_and_pack, sign_envelope,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::identity_op::IdentityOpClass;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use fauna_nest::principal_handlers::{PrincipalBinding, dispatch_principal};
use fauna_nest::routes::AppState;
use fauna_protocol::profile::{ProfileGetReply, ProfileGetRequest, ProfileSetRequest};
use fauna_protocol::profile_author::{KIND_SET, ProfileAuthorSetReply, ProfileAuthorSetRequest};

const SCOPE: &str = "fauna:identity:op:profile.author";
const READ: &str = "fauna:profile:read";
const POST_SCOPE: &str = "fauna:identity:op:post.author";
const REFUSED: &str = "fauna.profile.permission_denied";

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    Arc::new(state)
}

fn now_secs() -> u64 {
    Timestamp::now_secs() as u64
}

/// One consented principal: its roster row (attested key, `scopes`) and the
/// binding its session dials with.
async fn consent(
    state: &AppState,
    account: [u8; 32],
    client: &str,
    holder: [u8; 32],
    scopes: &[&str],
) -> PrincipalBinding {
    state
        .db
        .record_atproto_oauth_grant(
            &account,
            client.as_bytes(),
            client,
            Some("Editor"),
            &scopes.join(" "),
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
        token_scopes: scopes.iter().map(ToString::to_string).collect(),
        family_id: None,
    }
}

/// The owner's device mints the keyless `identity.op:<class>` grant.
async fn mint_grant(
    state: &AppState,
    account: [u8; 32],
    grant_id: [u8; 16],
    holder: [u8; 32],
    class: IdentityOpClass,
) {
    let now = now_secs();
    let blob = fauna_mls::wrapped_blob::build_grant_blob(
        &account,
        &grant_id,
        &holder,
        None,
        GrantWindow(now - 1, now + 3600),
        &[(fauna_client_capabilities::identity_op_scope(class), None)],
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
/// identity cert over it carrying `capabilities`, provision it.
async fn provision_delegation(
    state: &AppState,
    identity: &ActorKeypair,
    capabilities: Vec<Capability>,
) {
    let account = identity.actor_id().0;
    let k_pub = fauna_nest::atproto_authoring_key::mint_or_fetch(&state.db, &account)
        .await
        .unwrap();
    let cert = DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: k_pub,
        capabilities,
        created_at: Timestamp::now(),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(identity, &cert).unwrap();
    let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
    assert!(
        state
            .db
            .set_atproto_authoring_key_cert(&account, &wire)
            .await
            .unwrap()
    );
}

/// The owner publishes their own profile from their app — the base every
/// non-display field must survive the principal's edit from.
async fn publish_own_profile(state: &Arc<AppState>, identity: &ActorKeypair) -> Profile {
    let profile = Profile {
        actor_id: identity.actor_id(),
        display_name: Some("Alice".into()),
        bio: Some("the owner's bio".into()),
        avatar: None,
        banner: None,
        links: vec![],
        nests: vec![NestEntry {
            nest_id: vec![0x42; 32],
            url: "https://nest.example".into(),
            roles: vec![NestRole::Social],
        }],
        admin_nests: vec![],
        load_hint: None,
        inbox_mode: InboxMode::ContactsOnly,
        recovery_head: None,
        audience: fauna_core::data::ProfileAudience::ThisNest,
        updated_at: Timestamp::now(),
    };
    let body = sign_and_pack(identity, &profile).unwrap();
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::profile_handlers::register_profile_handlers(&mut b);
    let router = b.build();
    let payload = fauna_protocol::encode_canonical(&ProfileSetRequest {
        body: fauna_protocol::ByteBuf::from(body),
        extra: Default::default(),
    })
    .unwrap();
    let meta = router.kind_meta("fauna.profile.set").expect("registered");
    (meta.handler)(
        state.clone(),
        identity.actor_id().0,
        Bytes::from(payload.to_vec()),
    )
    .await
    .expect("the owner publishes their own profile");
    profile
}

async fn set(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    req: &ProfileAuthorSetRequest,
) -> Result<ProfileAuthorSetReply, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(req).unwrap();
    let reply: Bytes = dispatch_principal(state.clone(), binding, KIND_SET, payload).await?;
    Ok(fauna_protocol::decode_strict(&reply).unwrap())
}

/// The profile the read serves for the account, on the principal session.
async fn read(state: &Arc<AppState>, binding: &PrincipalBinding) -> Vec<u8> {
    let payload = fauna_protocol::encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(binding.account),
        extra: Default::default(),
    })
    .unwrap();
    let reply: Bytes = dispatch_principal(state.clone(), binding, "fauna.profile.get", payload)
        .await
        .expect("the principal's profile read");
    let reply: ProfileGetReply = fauna_protocol::decode_strict(&reply).unwrap();
    reply.body.into_vec()
}

fn bio(text: &str) -> ProfileAuthorSetRequest {
    ProfileAuthorSetRequest {
        bio: Some(text.into()),
        ..Default::default()
    }
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_principal_edits_the_display_fields_through_the_authoring_key() {
    let state = state().await;
    let identity = ActorKeypair::generate();
    let account = identity.actor_id().0;
    state
        .db
        .create_user(&account, "free", "test")
        .await
        .unwrap();
    let owned = publish_own_profile(&state, &identity).await;
    let editor = consent(
        &state,
        account,
        "https://editor.example/client.json",
        [0x77; 32],
        &[SCOPE, READ],
    )
    .await;
    let poster = consent(
        &state,
        account,
        "https://poster.example/client.json",
        [0x78; 32],
        &[POST_SCOPE, READ],
    )
    .await;

    // No grant yet: refused.
    let err = set(&state, &editor, &bio("too early")).await.unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");

    // A grant, but the account has no delegation: refused.
    mint_grant(
        &state,
        account,
        [0x0D; 16],
        [0x77; 32],
        IdentityOpClass::ProfileAuthor,
    )
    .await;
    let err = set(&state, &editor, &bio("no key yet")).await.unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");

    // A delegation that carries `Post` alone: refused.
    provision_delegation(&state, &identity, vec![Capability::Post]).await;
    let err = set(&state, &editor, &bio("post only")).await.unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");

    // A posting principal — grant and delegation both live for ITS class —
    // never reaches the profile door: the reach reads the class.
    mint_grant(
        &state,
        account,
        [0x0E; 16],
        [0x78; 32],
        IdentityOpClass::PostAuthor,
    )
    .await;
    let err = set(&state, &poster, &bio("as the poster"))
        .await
        .unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");
    assert_eq!(
        err.details.as_deref(),
        Some(&fauna_protocol::Value::String(format!(
            "{KIND_SET}: not covered by the principal's scopes"
        ))),
        "refused at the gate, never reaching the custodian: {err:?}"
    );

    provision_delegation(
        &state,
        &identity,
        vec![Capability::Post, Capability::UpdateProfile],
    )
    .await;
    let links = vec![ProfileLink {
        label: "site".into(),
        uri: "https://alice.example".into(),
    }];
    let reply = set(
        &state,
        &editor,
        &ProfileAuthorSetRequest {
            bio: Some("edited through the oracle".into()),
            links: Some(links.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("the display fields are edited under the grant and the delegation");

    // The read serves exactly the bytes the reply named, and they verify as
    // DELEGATED, authored by the account.
    let served = read(&state, &editor).await;
    assert_eq!(served, reply.body.into_vec());
    let (profile, origin) = decode_profile(&served).expect("the delegated chain verifies");
    assert!(
        matches!(origin, AuthoringOrigin::Delegated { .. }),
        "{origin:?}"
    );
    assert_eq!(profile.actor_id, ActorId(account));
    assert_eq!(profile.bio.as_deref(), Some("edited through the oracle"));
    assert_eq!(profile.links, links);
    assert_eq!(profile.display_name, owned.display_name, "unnamed: kept");
    assert_eq!(profile.nests, owned.nests, "never the principal's to set");
    assert_eq!(profile.inbox_mode, owned.inbox_mode);

    // The empty string clears.
    set(
        &state,
        &editor,
        &ProfileAuthorSetRequest {
            display_name: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .expect("a clear");
    let (profile, _) = decode_profile(&read(&state, &editor).await).unwrap();
    assert_eq!(profile.display_name, None);
    assert_eq!(profile.bio.as_deref(), Some("edited through the oracle"));

    // A picture, for now, and a member this build does not name: refused.
    let err = set(
        &state,
        &editor,
        &ProfileAuthorSetRequest {
            avatar: Some("ab".repeat(32)),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code, "fauna.profile.author.media_unavailable",
        "{err:?}"
    );
    let mut newer = bio("with a newer field");
    newer
        .extra
        .insert("pronouns".into(), fauna_protocol::Value::String("x".into()));
    let err = set(&state, &editor, &newer).await.unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");

    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM profile_author_ops").await,
        2,
        "one record row per performed operation; refusals record none"
    );
    let ops = state
        .db
        .list_principal_oracle_ops(&account, &editor.principal_id, 10)
        .await
        .unwrap();
    let details: Vec<(&str, &str)> = ops
        .iter()
        .map(|o| (o.class.as_str(), o.detail.as_str()))
        .collect();
    assert_eq!(
        details,
        [
            ("profile.author", "set display_name"),
            ("profile.author", "set bio links"),
        ]
    );

    // The owner revokes the delegation — K is destroyed — and the next set is
    // refused.
    assert!(
        state
            .db
            .delete_atproto_authoring_key(&account)
            .await
            .unwrap()
    );
    let err = set(&state, &editor, &bio("after the revoke"))
        .await
        .unwrap_err();
    assert_eq!(err.code, REFUSED, "{err:?}");

    // Revoking the principal takes its record with it.
    state
        .db
        .revoke_third_party_principal(&account, &editor.principal_id)
        .await
        .unwrap()
        .expect("the principal existed");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM profile_author_ops").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM profile_author_principals").await,
        0
    );
}
