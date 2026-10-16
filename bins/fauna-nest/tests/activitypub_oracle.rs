#![cfg(feature = "activitypub")]
//! Tier_3: the oracle, ActivityPub class (TP11 — `docs/goal/architecture/
//! key-material-hierarchy.md` § Audience: deployment infrastructure → *The
//! oracle*, the `activitypub.http_sig` row).
//!
//! A third-party principal holding a keyless
//! `identity.op:activitypub.http_sig` grant asks the account's actor-key
//! custodian for an HTTP Signature over an outbound ActivityPub request with
//! `fauna.activitypub.oracle.http_sig` on its principal session. Proven here,
//! in process, end to end through the principal dispatch chokepoint:
//!
//!  * no live grant → refused;
//!  * under a live grant, a `POST` of a `Create{Note}` to a host the
//!    principal's manifest declared is signed: the reply's `Signature` value
//!    verifies against the account's PUBLISHED actor key with the very
//!    verifier the inbox runs, over exactly the `host`, `date` and `digest`
//!    the reply hands back — and the custodian records one operation row;
//!  * a host outside the declared peer set is refused;
//!  * the sovereign activity shapes — `Move`, `Delete` of the actor,
//!    `Update` of the actor — are refused (the custodian's half of the deny
//!    list);
//!  * after the owner deletes the grant, the very next call is refused —
//!    per-request re-resolution, no cached authority;
//!  * revoking the principal deletes its record.

use std::sync::Arc;

use bytes::Bytes;
use fauna_bridge_activitypub::http_signatures::{
    compute_digest, parse_signature_header, verify_signature,
};
use fauna_core::identity_op::IdentityOpClass;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use fauna_nest::principal_handlers::{PrincipalBinding, dispatch_principal};
use fauna_nest::routes::AppState;
use fauna_protocol::activitypub_oracle::{HttpSigReply, HttpSigRequest, KIND_HTTP_SIG};

const ACCOUNT: [u8; 32] = [0xA7; 32];
const HOLDER: [u8; 32] = [0x77; 32];
const GRANT_ID: [u8; 16] = [0x0D; 16];
const CLIENT: &str = "https://oracle.example/client.json";
const SCOPE: &str = "fauna:identity:op:activitypub.http_sig";
const PEER: &str = "mastodon.example";
const ACTOR_URL: &str = "https://nest.example/ap/users/alice";

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    fauna_nest::activitypub::init_db(&db)
        .await
        .expect("init ap tables");
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    Arc::new(state)
}

/// Enable federation for the account the way the Bridges page does: an RSA
/// actor key sealed under the deployment seed. Returns the PUBLISHED key.
async fn enable_federation(state: &AppState) -> String {
    let (privkey_der, public_pem) =
        fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
    let conn = state.db.conn().await;
    let seed = fauna_nest::nest_kek::require_deployment_seed(&conn).unwrap();
    let sealed =
        fauna_nest::activitypub::key_crypto::encrypt_rsa_privkey(&seed, &privkey_der).unwrap();
    fauna_nest::activitypub::db_helpers::create_account(
        &conn,
        &hex::encode(ACCOUNT),
        "alice",
        ACTOR_URL,
        &sealed,
        &public_pem,
    )
    .unwrap();
    public_pem
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// The owner's device mints the keyless `identity.op` grant to the
/// principal's attested key — the `fauna.capabilities.mint` shape.
async fn mint_grant(state: &AppState) {
    let now = now_secs();
    let blob = fauna_mls::wrapped_blob::build_grant_blob(
        &ACCOUNT,
        &GRANT_ID,
        &HOLDER,
        None,
        GrantWindow(now - 1, now + 3600),
        &[(
            fauna_client_capabilities::identity_op_scope(IdentityOpClass::ActivityPubHttpSig),
            None,
        )],
    )
    .expect("a keyless grant builds")
    .to_canonical_bytes()
    .expect("encode");
    state
        .db
        .put_capability_grant(&ACCOUNT, &GRANT_ID, &HOLDER, (now + 3600) as i64, &blob)
        .await
        .unwrap();
}

async fn sign(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    method: &str,
    target: &str,
    body: Option<&[u8]>,
) -> Result<HttpSigReply, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(&HttpSigRequest {
        method: method.to_string(),
        target: target.to_string(),
        body: body.map(|b| serde_bytes::ByteBuf::from(b.to_vec())),
        extra: Default::default(),
    })
    .unwrap();
    let reply: Bytes = dispatch_principal(state.clone(), binding, KIND_HTTP_SIG, payload).await?;
    Ok(fauna_protocol::decode_strict(&reply).unwrap())
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn activity(kind: &str, object: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{ACTOR_URL}/activities/1"),
        "type": kind,
        "actor": ACTOR_URL,
        "object": object,
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_principal_signs_for_a_declared_peer_under_its_live_grant_only() {
    let state = state().await;
    state
        .db
        .create_user(&ACCOUNT, "free", "test")
        .await
        .unwrap();
    let public_pem = enable_federation(&state).await;

    // The consent ceremony mints the principal with its attested key and the
    // manifest's declared peer set.
    state
        .db
        .record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            CLIENT,
            Some("Oracle App"),
            SCOPE,
            &[],
            "jkt",
            i64::MAX,
            None,
            fauna_nest::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(HOLDER),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Remote,
                manifest: Some(PrincipalManifest {
                    publisher_key: [0x9B; 32],
                    declared_activitypub_peers: vec![PEER.to_string()],
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    let principal_id = state
        .db
        .list_third_party_principals(&ACCOUNT)
        .await
        .unwrap()[0]
        .principal_id
        .clone();
    let binding = PrincipalBinding {
        account: ACCOUNT,
        principal_id: principal_id.clone(),
        token_scopes: vec![SCOPE.to_string()],
        family_id: None,
    };
    let inbox = format!("https://{PEER}/users/bob/inbox");
    let note = activity(
        "Create",
        serde_json::json!({
            "id": format!("{ACTOR_URL}/notes/1"),
            "type": "Note",
            "attributedTo": ACTOR_URL,
            "content": "signed through the oracle",
        }),
    );

    // No grant yet: refused.
    let err = sign(&state, &binding, "post", &inbox, Some(&note))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.activitypub.permission_denied", "{err:?}");

    mint_grant(&state).await;
    let reply = sign(&state, &binding, "post", &inbox, Some(&note))
        .await
        .expect("a declared peer is signed for under the grant");
    assert_eq!(reply.host, PEER);
    assert_eq!(reply.key_id, format!("{ACTOR_URL}#main-key"));
    assert_eq!(
        reply.digest.as_deref(),
        Some(compute_digest(&note).as_str())
    );

    // The inbox's own verifier, against the PUBLISHED actor key, over exactly
    // the headers the reply hands back.
    let parsed = parse_signature_header(&reply.signature).unwrap();
    assert_eq!(parsed.key_id, reply.key_id);
    for covered in ["(request-target)", "host", "date", "digest"] {
        assert!(
            parsed.headers.iter().any(|h| h == covered),
            "{covered} is covered"
        );
    }
    let headers = |name: &str| match name {
        "host" => Some(reply.host.clone()),
        "date" => Some(reply.date.clone()),
        "digest" => reply.digest.clone(),
        _ => None,
    };
    verify_signature(&parsed, &public_pem, "post", "/users/bob/inbox", headers)
        .expect("the signature verifies with the actor's published key");
    // Bound to the presented target: another path does not verify.
    assert!(verify_signature(&parsed, &public_pem, "post", "/users/eve/inbox", headers).is_err());
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM ap_oracle_ops WHERE class = 'activitypub.http_sig'"
        )
        .await,
        1,
        "the custodian records the operation"
    );

    // A signed GET (an authorized fetch) carries no body and no digest.
    let fetched = sign(
        &state,
        &binding,
        "get",
        &format!("https://{PEER}/users/bob"),
        None,
    )
    .await
    .expect("a GET to a declared peer");
    assert_eq!(fetched.digest, None);

    // A host outside the declared peer set.
    let err = sign(
        &state,
        &binding,
        "post",
        "https://elsewhere.example/inbox",
        Some(&note),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.activitypub.permission_denied", "{err:?}");

    // The sovereign shapes: who the user is is never delegable.
    for sovereign in [
        activity("Move", serde_json::json!(ACTOR_URL)),
        activity("Delete", serde_json::json!(ACTOR_URL)),
        activity(
            "Delete",
            serde_json::json!({ "id": ACTOR_URL, "type": "Tombstone" }),
        ),
        activity(
            "Update",
            serde_json::json!({ "id": ACTOR_URL, "type": "Person" }),
        ),
    ] {
        let err = sign(&state, &binding, "post", &inbox, Some(&sovereign))
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            "fauna.activitypub.permission_denied",
            "{}",
            String::from_utf8_lossy(&sovereign)
        );
    }
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ap_oracle_ops").await,
        2,
        "refusals record no operation"
    );

    // The owner's roster read serves this custodian's record beside the
    // Nostr one's: the summary and the newest-first operation list.
    let summaries = state
        .db
        .principal_oracle_summaries(&ACCOUNT, now_secs())
        .await
        .unwrap();
    let summary = &summaries[&principal_id];
    assert_eq!(summary.requests_served, 2);
    assert_eq!(summary.ops_in_window, 2);
    let ops = state
        .db
        .list_principal_oracle_ops(&ACCOUNT, &principal_id, 10)
        .await
        .unwrap();
    let details: Vec<(&str, &str)> = ops
        .iter()
        .map(|o| (o.class.as_str(), o.detail.as_str()))
        .collect();
    assert_eq!(
        details,
        [
            ("activitypub.http_sig", "get mastodon.example"),
            ("activitypub.http_sig", "post mastodon.example"),
        ]
    );

    // The owner revokes the grant: the very next request is refused.
    assert!(
        state
            .db
            .delete_capability_grant(&ACCOUNT, &GRANT_ID)
            .await
            .unwrap()
    );
    let err = sign(&state, &binding, "post", &inbox, Some(&note))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.activitypub.permission_denied", "{err:?}");

    // Revoking the principal takes its record with it.
    state
        .db
        .revoke_third_party_principal(&ACCOUNT, &principal_id)
        .await
        .unwrap()
        .expect("the principal existed");
    assert_eq!(count(&state, "SELECT COUNT(*) FROM ap_oracle_ops").await, 0);
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ap_oracle_principals").await,
        0
    );
}
