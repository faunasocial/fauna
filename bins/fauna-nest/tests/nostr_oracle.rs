#![cfg(feature = "nostr")]
//! Tier_3: the oracle, Nostr class (TP11 — `docs/goal/architecture/
//! key-material-hierarchy.md` § Audience: deployment infrastructure → *The
//! oracle*; `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer →
//! *A principal as a bunker client*).
//!
//! A third-party principal holding a keyless `identity.op:nostr.sign_event`
//! grant binds a NIP-46 client key with `fauna.nostr.bunker.bind` on its
//! principal session, then drives the account's signer with **rust-nostr's
//! real `nostr-connect` client** over a bound relay — the
//! `nostr_relay_interop.rs` harness. Proven here, end to end:
//!
//!  * no live grant → the bind is refused (a principal cannot park a key);
//!  * under a live grant: `get_public_key` is the USER's key, a kind-1
//!    `sign_event` is signed and verifies, and the custodian records one
//!    operation row;
//!  * a kind on the oracle's deny set (NIP-41 key migration, 1776) is refused;
//!  * a method of a class the grant does not name (`nip44_encrypt`) is refused;
//!  * after the owner deletes the grant, the very next `sign_event` is refused
//!    — per-request re-resolution, no cached authority;
//!  * revoking the principal deletes its client row.
//!
//! The dep is a dev-dependency only, and its **source is never read** — the
//! project's dep-source rule. Only compiled under `--features nostr`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_bridge_nostr::signing::Keypair;
use fauna_core::identity_op::IdentityOpClass;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_nest::db::third_party_principals::{AttestedKeys, ExecutionForm, PrincipalAttestation};
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, db};
use fauna_nest::principal_handlers::{PrincipalBinding, dispatch_principal};
use fauna_nest::routes::AppState;
use fauna_protocol::nostr::{BindBunkerClientReply, BindBunkerClientRequest};

const ACCOUNT: [u8; 32] = [0xA7; 32];
const HOLDER: [u8; 32] = [0x77; 32];
const GRANT_ID: [u8; 16] = [0x0D; 16];
const CLIENT: &str = "https://oracle.example/client.json";
const SCOPE: &str = "fauna:identity:op:nostr.sign_event";

struct BoundRelay {
    state: Arc<AppState>,
    url: String,
}

/// A relay bound on an ephemeral port (`nostr_relay_interop.rs`'s harness).
async fn spawn_relay() -> BoundRelay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let base = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&base).await;
    let mut config = (*base.config).clone();
    config.nest.domain = Some(format!("{addr}"));
    let state = Arc::new(AppState {
        config: Arc::new(config),
        ..base
    });

    let router = nostr::routes().with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    BoundRelay {
        state,
        url: format!("ws://{addr}/nostr"),
    }
}

/// Link a custodial account (deposited nsec) and return the user's keypair.
async fn link_custodial(state: &AppState, actor_hex: &str) -> Keypair {
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let kp = Keypair::generate();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        actor_hex,
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    kp
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// The owner's device mints the keyless `identity.op` grant to the
/// principal's attested key — the `fauna.capabilities.mint` shape, stored the
/// way that handler stores it.
async fn mint_grant(state: &AppState, class: IdentityOpClass) {
    mint_grant_to(state, &GRANT_ID, &HOLDER, class).await;
}

async fn mint_grant_to(
    state: &AppState,
    grant_id: &[u8; 16],
    holder: &[u8; 32],
    class: IdentityOpClass,
) {
    let now = now_secs();
    let blob = fauna_mls::wrapped_blob::build_grant_blob(
        &ACCOUNT,
        grant_id,
        holder,
        None,
        GrantWindow(now - 1, now + 3600),
        &[(fauna_client_capabilities::identity_op_scope(class), None)],
    )
    .expect("a keyless grant builds")
    .to_canonical_bytes()
    .expect("encode");
    state
        .db
        .put_capability_grant(&ACCOUNT, grant_id, holder, (now + 3600) as i64, &blob)
        .await
        .unwrap();
}

async fn bind(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    client_pubkey: &str,
) -> Result<BindBunkerClientReply, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(&BindBunkerClientRequest {
        client_pubkey: client_pubkey.to_string(),
        extra: Default::default(),
    })
    .unwrap();
    let reply: Bytes =
        dispatch_principal(state.clone(), binding, "fauna.nostr.bunker.bind", payload).await?;
    Ok(fauna_protocol::decode_strict(&reply).unwrap())
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_principal_signs_through_the_oracle_under_its_live_grant_only() {
    use nostr_connect::prelude::*;

    let relay = spawn_relay().await;
    let state = relay.state.clone();
    let actor_hex = hex::encode(ACCOUNT);
    state
        .db
        .create_user(&ACCOUNT, "free", "test")
        .await
        .unwrap();
    let user_kp = link_custodial(&state, &actor_hex).await;

    // The consent ceremony mints the principal with its attested key.
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
                execution_form: ExecutionForm::Device,
                manifest: None,
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
    };

    let app_keys = Keys::generate();
    let client_pubkey = app_keys.public_key().to_hex();

    // No grant yet: the principal cannot park a client key.
    let err = bind(&state, &binding, &client_pubkey).await.unwrap_err();
    assert_eq!(err.code, "fauna.nostr.permission_denied", "{err:?}");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM nostr_oracle_clients").await,
        0
    );

    // The owner grants `nostr.sign_event`; the bind now answers the
    // secret-less connect string for the account's signer.
    mint_grant(&state, IdentityOpClass::NostrSignEvent).await;
    let reply = bind(&state, &binding, &client_pubkey).await.expect("bind");
    assert!(
        reply
            .connect_string
            .starts_with(&format!("bunker://{}?relay=", reply.signer_pubkey)),
        "{}",
        reply.connect_string
    );
    assert!(!reply.connect_string.contains("secret="));
    assert_ne!(reply.signer_pubkey, user_kp.public_key_hex());

    // The real client, at the bound relay (the reply's relay is the box's
    // public `wss://` face, which this loopback harness does not serve).
    let uri = NostrConnectUri::parse(format!(
        "bunker://{}?relay={}",
        reply.signer_pubkey, relay.url
    ))
    .expect("parse bunker URI");
    let signer =
        NostrConnect::new(uri, app_keys, Duration::from_secs(10), None).expect("nip46 client");

    let user_pk = tokio::time::timeout(Duration::from_secs(15), signer.get_public_key_async())
        .await
        .expect("get_public_key within 15s")
        .expect("get_public_key over the wire");
    assert_eq!(user_pk.to_hex(), user_kp.public_key_hex());

    let sign = |kind: u16, content: &str| {
        let unsigned = EventBuilder::new(Kind::Custom(kind), content).finalize_unsigned(user_pk);
        let signer = &signer;
        async move {
            tokio::time::timeout(Duration::from_secs(15), signer.sign_event_async(unsigned))
                .await
                .expect("an answer within 15s")
        }
    };

    let signed = sign(1, "signed through the oracle")
        .await
        .expect("kind 1 is signed under the grant");
    assert_eq!(signed.pubkey, user_pk);
    signed.verify().expect("the signature verifies client-side");
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM nostr_oracle_ops WHERE class = 'nostr.sign_event' AND detail = '1'"
        )
        .await,
        1,
        "the custodian records the operation"
    );

    // The deny set: NIP-41 key migration is succession, never delegable.
    assert!(sign(1776, "migrate").await.is_err(), "kind 1776 is refused");

    // A class the grant does not name.
    let peer = Keys::generate().public_key();
    let encrypted = tokio::time::timeout(
        Duration::from_secs(15),
        signer.nip44_encrypt_async(&peer, "not granted"),
    )
    .await
    .expect("an answer within 15s");
    assert!(
        encrypted.is_err(),
        "nip44 is refused under a sign_event-only grant"
    );

    // The owner revokes the grant: the very next request is refused.
    assert!(
        state
            .db
            .delete_capability_grant(&ACCOUNT, &GRANT_ID)
            .await
            .unwrap()
    );
    assert!(
        sign(1, "after revoke").await.is_err(),
        "a revoked grant severs at the next call"
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM nostr_oracle_ops").await,
        1,
        "refusals record no operation"
    );

    // Revoking the principal takes its client row with it.
    state
        .db
        .revoke_third_party_principal(&ACCOUNT, &principal_id)
        .await
        .unwrap()
        .expect("the principal existed");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM nostr_oracle_clients").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM nostr_oracle_ops").await,
        0
    );
}

/// One client key, one principal: a second principal of the same account
/// cannot bind a key the first already bound, so a request signed with that
/// key never resolves the second principal's grants.
#[tokio::test]
async fn a_bound_client_key_belongs_to_one_principal() {
    let relay = spawn_relay().await;
    let state = relay.state.clone();
    state
        .db
        .create_user(&ACCOUNT, "free", "test")
        .await
        .unwrap();
    link_custodial(&state, &hex::encode(ACCOUNT)).await;
    // Two principals, each with its own attested key (the consent refuses a
    // key another principal holds) and its own live grant.
    const HOLDER_TWO: [u8; 32] = [0x78; 32];
    for (family, client, holder) in [
        (&b"family-1"[..], "https://one.example/client.json", HOLDER),
        (
            &b"family-2"[..],
            "https://two.example/client.json",
            HOLDER_TWO,
        ),
    ] {
        state
            .db
            .record_atproto_oauth_grant(
                &ACCOUNT,
                family,
                client,
                None,
                SCOPE,
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
                    execution_form: ExecutionForm::Device,
                    manifest: None,
                },
            )
            .await
            .unwrap();
    }
    mint_grant(&state, IdentityOpClass::NostrSignEvent).await;
    mint_grant_to(
        &state,
        &[0x0E; 16],
        &HOLDER_TWO,
        IdentityOpClass::NostrSignEvent,
    )
    .await;
    let principals = state
        .db
        .list_third_party_principals(&ACCOUNT)
        .await
        .unwrap();
    assert_eq!(principals.len(), 2);
    let principal_holding = |holder: [u8; 32]| PrincipalBinding {
        account: ACCOUNT,
        principal_id: principals
            .iter()
            .find(|p| p.holder_x25519.as_deref() == Some(&holder[..]))
            .expect("a principal per holder")
            .principal_id
            .clone(),
        token_scopes: vec![SCOPE.to_string()],
    };
    let (one, two) = (principal_holding(HOLDER), principal_holding(HOLDER_TWO));
    let key = Keypair::generate().public_key_hex();
    bind(&state, &one, &key).await.expect("first bind");
    bind(&state, &one, &key)
        .await
        .expect("a re-bind of the same principal is idempotent");
    let err = bind(&state, &two, &key).await.unwrap_err();
    assert_eq!(err.code, "fauna.nostr.permission_denied", "{err:?}");
    bind(&state, &two, &Keypair::generate().public_key_hex())
        .await
        .expect("a key of its own binds");

    // A malformed key is refused before any row is touched.
    let err = bind(&state, &two, "NOT-HEX").await.unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed", "{err:?}");
}
