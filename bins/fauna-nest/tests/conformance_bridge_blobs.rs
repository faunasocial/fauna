//! End-to-end round-trip: provision → fetch → revoke through the
//! live RpcRouter using canonical-CBOR-encoded payloads. Uses the
//! committed wrapped-blob test vectors as opaque bytes for the
//! provision side (we treat them as random ciphertext since nest
//! never decodes; the conformance is the *envelope* round-trip).

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_mls::wrapped_blob::{
    ServedSetKeys, WebdavKeysBlob, WebdavKeysPlaintext, seal_webdav_keys_blob,
    unseal_webdav_keys_blob,
};
use fauna_nest::{bridge_blob_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    wrapped_blob::{
        BulkByteAccess, BulkByteMintPurpose, FetchWebdavKeysBlobReply, FetchWebdavKeysBlobRequest,
        FetchWrappedMlsBlobReply, FetchWrappedMlsBlobRequest, ListDkimSelectorsReply,
        ListDkimSelectorsRequest, MintBulkByteTokenReply, MintBulkByteTokenRequest,
        ProvisionWebdavKeysBlobRequest, ProvisionWrappedMlsBlobRequest, RevokeDkimBlobRequest,
        RevokeWrappedMlsBlobRequest, WebdavListFilesReply, WebdavListFilesRequest,
        WebdavListFoldersReply, WebdavListFoldersRequest, WebdavQuotaReply, WebdavQuotaRequest,
        WebdavRecordChangeReply, WebdavRecordChangeRequest,
    },
};
use serde_bytes::ByteBuf;

async fn router_with_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

async fn approve_mta(state: &Arc<AppState>, bridge_actor: [u8; 32], bridge_id: &str) {
    state
        .db
        .create_pending_bridge_service_user(
            &bridge_actor,
            fauna_nest::db::bridge_service_users::BridgeRole::Mta,
            bridge_id,
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

#[tokio::test]
async fn provision_then_fetch_wrapped_mls_blob_round_trip() {
    let (router, state) = router_with_state().await;

    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    // Provision: user uploads the wrapped_msek test vector as opaque ciphertext.
    // Any unknown actor falls back to CallerClass::User, which is permitted for
    // provision_wrapped_mls_blob.
    let blob_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../libs/fauna-protocol/schemas/test_vectors/wrapped_msek.bin"
    ))
    .expect("test vector present");
    let req = ProvisionWrappedMlsBlobRequest {
        extra: Default::default(),
        blob: ByteBuf::from(blob_bytes.clone()),
        credential_id: "default".to_string(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let _ = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.provision_wrapped_mls_blob",
        payload,
    )
    .await
    .expect("provision ok");

    // Fetch: bridge (MDA class) fetches by (user_actor, "default").
    let fetch_req = FetchWrappedMlsBlobRequest {
        extra: Default::default(),
        actor_id: user_actor.to_vec(),
        credential_id: "default".into(),
    };
    let fetch_payload = Bytes::from(encode_canonical(&fetch_req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.fetch_wrapped_mls_blob",
        fetch_payload,
    )
    .await
    .expect("fetch ok");

    let reply: FetchWrappedMlsBlobReply = decode(&reply_bytes).unwrap();
    assert_eq!(
        reply.blob.as_ref().map(|b| b.as_ref()),
        Some(&blob_bytes[..]),
        "fetched blob must equal provisioned bytes"
    );

    // Revoke: user revokes own blob.
    let revoke_req = RevokeWrappedMlsBlobRequest {
        extra: Default::default(),
        actor_id: user_actor.to_vec(),
        credential_id: "default".into(),
    };
    let revoke_payload = Bytes::from(encode_canonical(&revoke_req).unwrap().to_vec());
    let _ = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.revoke_wrapped_mls_blob",
        revoke_payload,
    )
    .await
    .expect("revoke ok");

    // Fetch again: should be None after revocation.
    let fetch_payload2 = Bytes::from(
        encode_canonical(&FetchWrappedMlsBlobRequest {
            extra: Default::default(),
            actor_id: user_actor.to_vec(),
            credential_id: "default".into(),
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes2 = dispatch(
        &router,
        state,
        bridge_actor,
        "fauna.bridges.fetch_wrapped_mls_blob",
        fetch_payload2,
    )
    .await
    .expect("fetch ok after revoke");
    let reply2: FetchWrappedMlsBlobReply = decode(&reply_bytes2).unwrap();
    assert!(reply2.blob.is_none(), "blob must be None after revocation");
}

/// WebDAV served-set key blob (slice 1b): the full provision → fetch → unseal
/// chain the MDA runs to obtain its per-set content keys (`webdav-server.md`
/// § Key model + § MDA↔nest WS-RPC contract). Unlike the opaque-bytes wrapped-mls
/// round-trip above, this seals a *real* `WebdavKeysPlaintext` and asserts the MDA
/// recovers the exact served-set content keys after unseal — the slice-1b success
/// proof. (The real-transport / real-Go-MDA end-to-end lands with slice 4.)
#[tokio::test]
async fn provision_then_fetch_and_unseal_webdav_keys_blob_round_trip() {
    let (router, state) = router_with_state().await;

    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    let msek = [0x5Au8; 32];
    common::approve_mda(&state, bridge_actor).await;

    // The client's served-set key material: two sets, one read-only, one with a
    // rotated generation history the MDA must be able to `key_for(version)`.
    let mut rotated = FolderContentKeys::genesis([0x11; 32], 1_700_000_000);
    rotated.rotate([0x22; 32], 1_700_000_100);
    let plaintext = WebdavKeysPlaintext::new(vec![
        ServedSetKeys {
            set_name: "docs".into(),
            read_only: false,
            keys: rotated,
        },
        ServedSetKeys {
            set_name: "photos".into(),
            read_only: true,
            keys: FolderContentKeys::genesis([0x33; 32], 1_700_000_200),
        },
    ]);
    let sealed =
        seal_webdav_keys_blob(&plaintext.to_canonical_bytes().unwrap(), &user_actor, &msek)
            .expect("seal");
    let blob_bytes = sealed.to_canonical_bytes().unwrap();

    // Provision (User class — an unknown actor falls back to User, permitted).
    let provision_payload = Bytes::from(
        encode_canonical(&ProvisionWebdavKeysBlobRequest {
            blob: ByteBuf::from(blob_bytes.clone()),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    );
    let _ = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.provision_webdav_keys_blob",
        provision_payload,
    )
    .await
    .expect("provision ok");

    // A non-MDA class (the user itself) is denied fetch by the allowlist gate.
    let deny = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.fetch_webdav_keys_blob",
        Bytes::from(
            encode_canonical(&FetchWebdavKeysBlobRequest {
                actor_id: user_actor.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;
    assert!(
        deny.is_err(),
        "fetch_webdav_keys_blob must deny non-MDA class"
    );

    // Fetch (BridgeMda class) — the MDA obtains the opaque blob for the actor.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.fetch_webdav_keys_blob",
        Bytes::from(
            encode_canonical(&FetchWebdavKeysBlobRequest {
                actor_id: user_actor.to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("fetch ok");
    let reply: FetchWebdavKeysBlobReply = decode(&reply_bytes).unwrap();
    let fetched = reply.blob.expect("blob present");
    assert_eq!(
        fetched.as_ref(),
        &blob_bytes[..],
        "fetched blob must equal provisioned bytes"
    );

    // Unseal (what the Go MDA does after MUA AUTH): recover the exact served-set
    // content keys under MSEK.
    let decoded_blob = WebdavKeysBlob::from_canonical_bytes(fetched.as_ref()).unwrap();
    let opened = unseal_webdav_keys_blob(&decoded_blob, &msek).expect("unseal");
    let recovered = WebdavKeysPlaintext::from_canonical_bytes(&opened).unwrap();
    assert_eq!(
        recovered, plaintext,
        "recovered plaintext must match sealed"
    );
    let docs = &recovered.served_sets[0];
    assert_eq!(docs.set_name, "docs");
    assert!(!docs.read_only);
    assert_eq!(docs.keys.key_for(1), Some(&[0x11; 32]), "pre-rotation key");
    assert_eq!(docs.keys.key_for(2), Some(&[0x22; 32]), "current key");
    assert!(recovered.served_sets[1].read_only, "photos is read-only");

    // A wrong MSEK cannot open the blob (the MDA session key is the only door).
    assert!(
        unseal_webdav_keys_blob(&decoded_blob, &[0u8; 32]).is_err(),
        "wrong MSEK must fail to unseal"
    );

    // An actor with no provisioned blob fetches None.
    let none_reply = dispatch(
        &router,
        state,
        bridge_actor,
        "fauna.bridges.fetch_webdav_keys_blob",
        Bytes::from(
            encode_canonical(&FetchWebdavKeysBlobRequest {
                actor_id: [0x99u8; 32].to_vec(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("fetch ok");
    assert!(
        decode::<FetchWebdavKeysBlobReply>(&none_reply)
            .unwrap()
            .blob
            .is_none(),
        "unprovisioned actor must fetch None"
    );
}

// Admin DNS surface: a selector the nest minted is read via
// `list_dkim_selectors` and retired via `revoke_dkim_blob`. The key never
// crosses the list reply — only the public record.
#[tokio::test]
async fn admin_list_revoke_dkim_round_trip() {
    let (router, state) = router_with_state().await;
    let admin_actor = [33u8; 32];
    state.db.add_admin_actor(&admin_actor).await.unwrap();

    fauna_nest::test_support::seat_deployment_seed(&state.db, &[0x5eu8; 32]).await;
    assert!(
        state
            .db
            .mint_mail_dkim_key("example.com", "s1")
            .await
            .expect("mint")
    );

    // List returns the public metadata, including the DNS TXT record the
    // admin must publish.
    let list_req = ListDkimSelectorsRequest {
        extra: Default::default(),
        domain: None,
    };
    let payload = Bytes::from(encode_canonical(&list_req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.list_dkim_selectors",
        payload,
    )
    .await
    .expect("admin list dkim ok");
    let listed: ListDkimSelectorsReply = decode(&reply_bytes).unwrap();
    assert_eq!(listed.selectors.len(), 1, "exactly one selector");
    let info = &listed.selectors[0];
    assert!(
        info.public_dns_value.starts_with("v=DKIM1; k=ed25519; p="),
        "{}",
        info.public_dns_value
    );
    assert!(info.created_at > 0, "created_at populated");

    // Retire it; the list then empties.
    let revoke_req = RevokeDkimBlobRequest {
        extra: Default::default(),
        domain: info.domain.clone(),
        selector: info.selector.clone(),
    };
    let payload = Bytes::from(encode_canonical(&revoke_req).unwrap().to_vec());
    let _ = dispatch(
        &router,
        state.clone(),
        admin_actor,
        "fauna.bridges.revoke_dkim_blob",
        payload,
    )
    .await
    .expect("admin revoke dkim ok");

    let list_req = ListDkimSelectorsRequest {
        extra: Default::default(),
        domain: None,
    };
    let payload = Bytes::from(encode_canonical(&list_req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        admin_actor,
        "fauna.bridges.list_dkim_selectors",
        payload,
    )
    .await
    .expect("admin list dkim ok");
    let listed: ListDkimSelectorsReply = decode(&reply_bytes).unwrap();
    assert!(listed.selectors.is_empty(), "selector retired");
}

/// Bulk-byte token (slice 3): the MDA mints a short-TTL scoped token for the
/// chunk/manifest byte routes (`webdav-server.md` § Bulk-byte plane). The mint
/// gates on the set being served to the actor (non-reserved + `webdav_enabled`) —
/// the enforceable "cross-set" boundary. A served set mints; an unserved /
/// wrong-mode / absent set is `set_not_served`.
#[tokio::test]
async fn mint_bulk_byte_token_gates_on_served_set() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    // A served folder.
    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // An unserved folder (webdav_enabled left false).
    state
        .db
        .create_folder_with_options(
            "private",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A second folder that is never served (`webdav_enabled` left false).
    state
        .db
        .create_folder_with_options(
            "archive",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mint_req = |set: &str, access: BulkByteAccess| {
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: user_actor.to_vec(),
            folder: set.to_string(),
            access,
            purpose: BulkByteMintPurpose::Folder,
            ..Default::default()
        })
        .unwrap()
    };

    // Served → mint succeeds; the token validates with the requested scope.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.mint_bulk_byte_token",
        mint_req("photos", BulkByteAccess::Write),
    )
    .await
    .expect("served set mints");
    let reply: MintBulkByteTokenReply = decode(&reply_bytes).unwrap();
    assert!(!reply.token.is_empty(), "a token is returned");
    let scope = state
        .auth
        .bulk_byte_tokens
        .validate(&reply.token)
        .await
        .expect("minted token validates");
    assert_eq!(scope.actor_id.0, user_actor);
    // Attributed to the set's row id, never its (sealed-at-rest) name.
    let photos_id = state
        .db
        .get_folder_for_actor("photos", &user_actor)
        .await
        .unwrap()
        .expect("the served set")
        .id;
    assert_eq!(scope.folder, format!("folder:{photos_id}"));
    assert_eq!(scope.access, BulkByteAccess::Write);
    assert_eq!(scope.expires_at, reply.expires_at);

    // Unserved / wrong-mode / absent → set_not_served.
    for (set, why) in [
        ("private", "unserved"),
        ("archive", "also unserved"),
        ("nonexistent", "absent"),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            bridge_actor,
            "fauna.bridges.mint_bulk_byte_token",
            mint_req(set, BulkByteAccess::Read),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.bridges.set_not_served",
            "{why} set must be denied a token"
        );
    }
}

/// A non-MDA caller (an unknown actor falls back to `CallerClass::User`) is
/// denied by the allowlist even for a genuinely served set.
#[tokio::test]
async fn mint_bulk_byte_token_denied_for_non_mda() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.mint_bulk_byte_token",
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            access: BulkByteAccess::Write,
            purpose: BulkByteMintPurpose::Folder,
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ── Mail-body bulk tokens (smtp-server.md § Message size limits) ──

/// The MTA may mint a token to stage a sealed body over the inline budget — gated
/// on the target actually being a mail recipient here, which is the only property
/// that means anything for mail (it has no folder).
#[tokio::test]
async fn an_mta_mints_a_mail_body_token_only_for_a_real_recipient() {
    let (router, state) = router_with_state().await;
    let mta_actor = [9u8; 32];
    approve_mta(&state, mta_actor, "mta-1").await;

    let recipient = [42u8; 32];
    let stranger = [43u8; 32];
    // A mail recipient is precisely an actor with a recipient seal key — the same
    // fact `ingest_inbound_mail` already fails closed on, so a token can never be
    // minted toward an actor whose mail could not be sealed anyway.
    common::seed_recipient_seal_key(&state.db, &recipient, &common::FIXTURE_MSEK).await;

    let mail_req = |actor: [u8; 32]| {
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: actor.to_vec(),
            folder: String::new(), // mail belongs to no set
            access: BulkByteAccess::Write,
            purpose: BulkByteMintPurpose::MailBody,
            ..Default::default()
        })
        .unwrap()
    };

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.mint_bulk_byte_token",
        mail_req(recipient),
    )
    .await
    .expect("the MTA mints a mail-body token for a real recipient");
    let reply: MintBulkByteTokenReply = decode(&reply_bytes).unwrap();
    let scope = state
        .auth
        .bulk_byte_tokens
        .validate(&reply.token)
        .await
        .expect("minted token validates");
    assert_eq!(scope.actor_id.0, recipient);
    assert_eq!(scope.purpose, BulkByteMintPurpose::MailBody);
    // No caller-supplied string rides into the scope as if it were an attribution key.
    assert_eq!(scope.folder, "");

    let err = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.mint_bulk_byte_token",
        mail_req(stranger),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code, "fauna.bridges.permission_denied",
        "an actor who is not a mail recipient here must not yield a mail-body token"
    );
}

/// **Admitting the MTA to the mint did NOT open the WebDAV byte path to it.**
///
/// This is the load-bearing half of the purpose split: the allowlist now lets the
/// MTA *call* `mint_bulk_byte_token` (it must, to stage a large sealed body), so the
/// only thing standing between an MTA and a folder token is the handler's
/// purpose×class gate. If that gate ever regresses, an MTA could mint byte-plane
/// authz against a user's served folder — which is why this is pinned separately
/// from the allowlist test.
#[tokio::test]
async fn an_mta_may_not_mint_a_folder_token() {
    let (router, state) = router_with_state().await;
    let mta_actor = [9u8; 32];
    approve_mta(&state, mta_actor, "mta-1").await;

    let user_actor = [42u8; 32];
    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // A genuinely served, sync-type, owned set — everything the MDA's gate asks for.
    // The MTA is still denied, because it is not the MDA.
    let err = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.mint_bulk_byte_token",
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            access: BulkByteAccess::Write,
            purpose: BulkByteMintPurpose::Folder,
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

/// Defense in depth: the cross-nest-writer token purpose is minted SOLELY by the
/// `fauna.federation.folder.write_token.mint` federation handler (after its
/// foreign-member + `access == 'writer'` gate) — a bridge is never a foreign-set
/// writer, so even a genuine MDA that hand-sets `purpose: ForeignFolderWrite`
/// on its request is refused. The byte routes are purpose-blind, so the mint
/// gate is the only place a caller-set purpose can be stopped from opening the
/// byte plane under the bridge's identity (`federation.md` § Cross-nest…,
/// contract point (ii)).
#[tokio::test]
async fn a_bridge_may_not_mint_a_foreign_folder_write_token() {
    let (router, state) = router_with_state().await;
    let mda_actor = [11u8; 32];
    common::approve_mda(&state, mda_actor).await;

    let target = [42u8; 32];
    let err = dispatch(
        &router,
        state.clone(),
        mda_actor,
        "fauna.bridges.mint_bulk_byte_token",
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: target.to_vec(),
            folder: "anything".into(),
            access: BulkByteAccess::Write,
            purpose: BulkByteMintPurpose::ForeignFolderWrite,
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code, "fauna.bridges.permission_denied",
        "a bridge is refused the foreign-folder-write purpose outright"
    );
}

/// Same defense, one rail over: the cross-nest **conversation** member's token
/// purpose is minted SOLELY by `fauna.federation.conversation.write_token.mint`
/// after H's structural foreign-member gate (`conversation-rooms.md` § The home
/// nest → *Attachment bytes*) — a bridge is never a room member, so a hand-set
/// `purpose: ForeignConversationWrite` is refused outright.
#[tokio::test]
async fn a_bridge_may_not_mint_a_foreign_conversation_write_token() {
    let (router, state) = router_with_state().await;
    let mda_actor = [11u8; 32];
    common::approve_mda(&state, mda_actor).await;

    let target = [42u8; 32];
    let err = dispatch(
        &router,
        state.clone(),
        mda_actor,
        "fauna.bridges.mint_bulk_byte_token",
        encode_canonical(&MintBulkByteTokenRequest {
            actor_id: target.to_vec(),
            folder: "anything".into(),
            access: BulkByteAccess::Write,
            purpose: BulkByteMintPurpose::ForeignConversationWrite,
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code, "fauna.bridges.permission_denied",
        "a bridge is refused the foreign-conversation-write purpose outright"
    );
}

// ── WebDAV data-plane kinds (webdav-server.md § MDA↔nest WS-RPC contract) ──

/// `webdav_list_folders` returns ONLY the actor's served folders
/// (non-reserved + per-set `webdav_enabled`), and is BridgeMda-only.
#[tokio::test]
async fn webdav_list_folders_returns_only_served_sync_sets() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    // Served sync set.
    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Unserved sync set (webdav_enabled left false) + a backup set (never servable).
    state
        .db
        .create_folder_with_options(
            "private",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .create_folder_with_options(
            "archive",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_list_folders",
        encode_canonical(&WebdavListFoldersRequest {
            actor_id: user_actor.to_vec(),
            extra: Default::default(),
        })
        .unwrap(),
    )
    .await
    .expect("list served sets");
    let reply: WebdavListFoldersReply = decode(&reply_bytes).unwrap();
    let served: Vec<(String, Option<Vec<u8>>)> = reply
        .folders
        .into_iter()
        .map(|s| (s.name, s.name_hash.map(|h| h.into_vec())))
        .collect();
    assert_eq!(
        served,
        vec![(
            "photos".to_string(),
            Some(fauna_core::path_crypto::set_name_hash("photos").to_vec())
        )],
        "only the served sync set, carrying the hash the MDA matches by"
    );

    // Non-MDA (unknown actor falls back to User) is denied by the allowlist.
    let err = dispatch(
        &router,
        state.clone(),
        user_actor,
        "fauna.bridges.webdav_list_folders",
        encode_canonical(&WebdavListFoldersRequest {
            actor_id: user_actor.to_vec(),
            extra: Default::default(),
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

/// A WebDAV write recorded via `webdav_record_change` (attributed to the
/// auto-registered pseudo-device) surfaces in `webdav_list_files` with its
/// content-key generation; both gate on the set being served, and both are
/// BridgeMda-only.
#[tokio::test]
async fn webdav_record_change_then_list_files_round_trips() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // An unserved sync set, to prove the gate.
    state
        .db
        .create_folder_with_options(
            "private",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let m1 = "ab".repeat(32); // hex of [0xAB; 32]
    let m2 = "cd".repeat(32);

    let record = |path: &str, manifest: &str, size: i64| WebdavRecordChangeRequest {
        actor_id: user_actor.to_vec(),
        folder: "photos".into(),
        path: path.into(),
        manifest_hash: Some(manifest.to_string()),
        size_bytes: size,
        change_type: "Created".into(),
        content_key_version: Some(7),
        if_match: None,
        if_none_match: None,
        path_sealed: Some(ByteBuf::from(mda_seal(path))),
        ..Default::default()
    };

    // Two writes (the second exercises the idempotent pseudo-device register).
    for req in [
        record("docs/a.txt", &m1, 123),
        record("docs/b.txt", &m2, 456),
    ] {
        let bytes = dispatch(
            &router,
            state.clone(),
            bridge_actor,
            "fauna.bridges.webdav_record_change",
            encode_canonical(&req).unwrap(),
        )
        .await
        .expect("record change");
        let reply: WebdavRecordChangeReply = decode(&bytes).unwrap();
        assert!(reply.seq >= 1, "a monotonic seq is assigned");
    }

    // The served set now lists both files, latest-per-path, with content_key_version.
    let bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_list_files",
        encode_canonical(&WebdavListFilesRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .expect("list served files");
    let reply: WebdavListFilesReply = decode(&bytes).unwrap();
    assert_eq!(reply.files.len(), 2);
    // Post-S9-flip, a sealed set rests no plaintext path — the nest forwards
    // the seal + salt verbatim and only the MDA (holding the content key) can
    // render it, mirroring `a_dav_write_seals_its_path_and_the_listing_renders_it_without_the_plaintext`.
    // The rows themselves are ORDER BY path_hash (a DB implementation detail,
    // not a promised lexicographic order — `get_files_for_folder`), so match
    // by rendered path rather than assuming an index.
    let render = |row: &fauna_protocol::wrapped_blob::WebdavFile| {
        fauna_core::label_custody::render_path(
            &mda_custody(),
            row.path_sealed.as_ref().map(|b| &b[..]),
            &row.path,
            row.path_hash.as_ref().map(|b| &b[..]),
            fauna_core::path_crypto::LabelField::SyncChangePath,
        )
    };
    let find = |path: &str| {
        reply
            .files
            .iter()
            .find(|f| {
                render(f) == fauna_core::path_crypto::SealedLabelRender::Sealed(path.to_string())
            })
            .unwrap_or_else(|| panic!("{path} not found in the listing"))
    };
    let a = find("docs/a.txt");
    assert_eq!(a.manifest_hash, m1);
    assert_eq!(a.size_bytes, 123);
    assert_eq!(a.content_key_version, Some(7));
    let b = find("docs/b.txt");
    assert_eq!(b.manifest_hash, m2);

    // Unserved set → set_not_served.
    let err = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_list_files",
        encode_canonical(&WebdavListFilesRequest {
            actor_id: user_actor.to_vec(),
            folder: "private".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.set_not_served");

    // Non-MDA caller denied on both kinds.
    for kind in [
        "fauna.bridges.webdav_list_files",
        "fauna.bridges.webdav_record_change",
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            user_actor,
            kind,
            encode_canonical(&WebdavListFilesRequest {
                actor_id: user_actor.to_vec(),
                folder: "photos".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.permission_denied", "{kind}");
    }
}

/// The three MDA-sent set-scoped kinds resolve the served set by its hash
/// address alone (S5b) — the address that survives the nest's plaintext name
/// blanking — with the served gate unchanged: a hash naming an unserved or
/// absent set is `set_not_served`, and a malformed hash is refused.
#[tokio::test]
async fn webdav_kinds_resolve_the_served_set_by_hash_alone() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;
    for name in ["photos", "private"] {
        state
            .db
            .create_folder_with_options(name, &user_actor, Default::default())
            .await
            .unwrap();
    }
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let hash = |name: &str| {
        Some(ByteBuf::from(
            fauna_core::path_crypto::set_name_hash(name).to_vec(),
        ))
    };
    let call = |kind: &'static str, payload: bytes::Bytes| {
        dispatch(&router, state.clone(), bridge_actor, kind, payload)
    };
    let record = |name_hash| WebdavRecordChangeRequest {
        actor_id: user_actor.to_vec(),
        path: "a.txt".into(),
        manifest_hash: Some("ab".repeat(32)),
        size_bytes: 3,
        change_type: "create".into(),
        content_key_version: Some(1),
        path_sealed: Some(ByteBuf::from(mda_seal("a.txt"))),
        name_hash,
        ..Default::default()
    };
    let list = |name_hash| WebdavListFilesRequest {
        actor_id: user_actor.to_vec(),
        name_hash,
        ..Default::default()
    };
    let mint = |name_hash| MintBulkByteTokenRequest {
        actor_id: user_actor.to_vec(),
        access: BulkByteAccess::Write,
        name_hash,
        ..Default::default()
    };

    call(
        "fauna.bridges.webdav_record_change",
        encode_canonical(&record(hash("photos"))).unwrap(),
    )
    .await
    .expect("a hash-addressed record resolves the served set");
    let listed: WebdavListFilesReply = decode(
        &call(
            "fauna.bridges.webdav_list_files",
            encode_canonical(&list(hash("photos"))).unwrap(),
        )
        .await
        .expect("a hash-addressed listing resolves the served set"),
    )
    .unwrap();
    assert_eq!(listed.files.len(), 1);
    call(
        "fauna.bridges.mint_bulk_byte_token",
        encode_canonical(&mint(hash("photos"))).unwrap(),
    )
    .await
    .expect("a hash-addressed mint resolves the served set");

    for name in ["private", "nonexistent"] {
        for (kind, payload) in [
            (
                "fauna.bridges.webdav_record_change",
                encode_canonical(&record(hash(name))).unwrap(),
            ),
            (
                "fauna.bridges.webdav_list_files",
                encode_canonical(&list(hash(name))).unwrap(),
            ),
            (
                "fauna.bridges.mint_bulk_byte_token",
                encode_canonical(&mint(hash(name))).unwrap(),
            ),
        ] {
            let err = call(kind, payload).await.unwrap_err();
            assert_eq!(err.code, "fauna.bridges.set_not_served", "{kind} {name}");
        }
    }

    let err = call(
        "fauna.bridges.webdav_list_files",
        encode_canonical(&list(Some(ByteBuf::from(vec![0u8; 5])))).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.invalid_request");
}

/// `webdav_quota` reports the very meter and tier ceiling `webdav_record_change`
/// enforces: a WebDAV write's bytes show up as used, the ceiling is the actor's
/// tier `max_storage_bytes`, a write past it is refused with the quota code (the
/// MDA's `507`), and the kind is BridgeMda-only.
#[tokio::test]
async fn webdav_quota_reports_the_meter_the_record_path_enforces() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    let mut tier = state
        .db
        .get_tier("free")
        .await
        .unwrap()
        .expect("free tier seeded");
    tier.max_storage_bytes = 1000;
    state.db.update_tier(&tier).await.unwrap();
    state
        .db
        .create_user(&user_actor, "free", "a-user")
        .await
        .unwrap();
    serve_a_set(&state, user_actor, "photos").await;

    let quota = |actor: [u8; 32]| {
        let router = &router;
        let state = state.clone();
        async move {
            dispatch(
                router,
                state,
                actor,
                "fauna.bridges.webdav_quota",
                encode_canonical(&WebdavQuotaRequest {
                    actor_id: user_actor.to_vec(),
                    extra: Default::default(),
                })
                .unwrap(),
            )
            .await
        }
    };
    let read = |bytes: Bytes| -> WebdavQuotaReply { decode(&bytes).unwrap() };

    let before = read(quota(bridge_actor).await.expect("quota"));
    assert_eq!(before.storage_bytes_used, 0);
    assert_eq!(before.storage_bytes_limit, Some(1000));

    let record = |path: &str, manifest: &str, size: i64| WebdavRecordChangeRequest {
        actor_id: user_actor.to_vec(),
        folder: "photos".into(),
        path: path.into(),
        manifest_hash: Some(manifest.to_string()),
        size_bytes: size,
        change_type: "create".into(),
        content_key_version: Some(1),
        if_match: None,
        if_none_match: None,
        path_sealed: Some(ByteBuf::from(mda_seal(path))),
        ..Default::default()
    };
    dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_record_change",
        encode_canonical(&record("a.bin", &"ab".repeat(32), 600)).unwrap(),
    )
    .await
    .expect("a write inside the allowance records");

    let after = read(quota(bridge_actor).await.expect("quota"));
    assert_eq!(after.storage_bytes_used, 600, "the write is charged");
    assert_eq!(after.storage_bytes_limit, Some(1000));

    // 600 + 500 > 1000: the record path refuses against the same pair.
    let err = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_record_change",
        encode_canonical(&record("b.bin", &"cd".repeat(32), 500)).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.sync.storage_quota_exceeded");
    let unchanged = read(quota(bridge_actor).await.expect("quota"));
    assert_eq!(
        unchanged.storage_bytes_used, 600,
        "a refused write charges nothing"
    );

    // Non-MDA caller denied.
    let err = quota(user_actor).await.unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

/// `webdav_record_change` enforces the WebDAV `If-Match` / `If-None-Match` ETag
/// conditional at the nest; a lost race is `fauna.bridges.conflict` (→ `412`).
#[tokio::test]
async fn webdav_record_change_enforces_if_match_conditional() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;

    state
        .db
        .create_folder_with_options(
            "photos",
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            "photos",
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let m1 = "ab".repeat(32);
    let m2 = "cd".repeat(32);
    let m3 = "ef".repeat(32);

    let req = |path: &str,
               manifest: &str,
               change: &str,
               if_match: Option<String>,
               if_none_match: Option<String>| {
        encode_canonical(&WebdavRecordChangeRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            path: path.into(),
            manifest_hash: Some(manifest.to_string()),
            size_bytes: 10,
            change_type: change.into(),
            content_key_version: Some(1),
            if_match,
            if_none_match,
            path_sealed: Some(ByteBuf::from(mda_seal(path))),
            ..Default::default()
        })
        .unwrap()
    };
    let go = |payload: Bytes| {
        dispatch(
            &router,
            state.clone(),
            bridge_actor,
            "fauna.bridges.webdav_record_change",
            payload,
        )
    };

    // 1. Unconditional create of "x" → ok; head("x") = m1.
    go(req("x", &m1, "Created", None, None))
        .await
        .expect("unconditional create");

    // 2. If-None-Match: * on an existing path → conflict.
    let err = go(req("x", &m1, "Created", None, Some("*".into())))
        .await
        .unwrap_err();
    assert_eq!(
        err.code, "fauna.bridges.conflict",
        "create-only over existing"
    );

    // 3. If-Match a WRONG etag → conflict.
    let err = go(req("x", &m2, "Modified", Some(m2.clone()), None))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.conflict", "stale If-Match");

    // 4. If-Match the CORRECT current etag → ok; head("x") = m2.
    go(req("x", &m2, "Modified", Some(m1.clone()), None))
        .await
        .expect("matching If-Match");

    // 5. If-Match the now-stale m1 → conflict (head moved to m2).
    let err = go(req("x", &m3, "Modified", Some(m1.clone()), None))
        .await
        .unwrap_err();
    assert_eq!(
        err.code, "fauna.bridges.conflict",
        "If-Match after head moved"
    );

    // 6. If-None-Match: * on a fresh path → ok (create).
    go(req("y", &m1, "Created", None, Some("*".into())))
        .await
        .expect("create-only on absent path");
}

// ── Sealed names & paths: the WebDAV / MDA leg (path-sealing S4) ─────────
//
// tier_3. Proof obligation (`docs/goal/behavior/webdav-server.md` § Key model,
// implementing `file-sync.md` § Sealed names & paths): the DAV leg must be an
// ordinary **keyed writer** — a PUT records a seal the nest cannot open — and an
// ordinary **sealed-first reader** — a PROPFIND carries the seal *and the salt
// it opens under* to the MDA, so the listing survives the plaintext scrub.
//
// Scope split, stated honestly: these tests drive the real
// `fauna.bridges.webdav_record_change` / `webdav_list_files` handlers and pin the
// WIRE. The Go-facing FFI wrappers themselves (`webdav_seal_path`,
// `webdav_render_paths`) are pinned by their own unit tests in
// `libs/fauna-ffi/src/mail.rs::webdav_keys_tests`, which cover the degrade
// matrix; the seal here is built with the identical derivation those wrap.

/// The served set's content key. The nest never holds this in any form — that is
/// the property under test, not a convenience.
const MDA_CONTENT_KEY: [u8; 32] = [0xc1; 32];
const MDA_GENERATION: u64 = 7;
const DAV_PATH: &str = "2026/eviction_notice.pdf";

fn mda_content_keys() -> FolderContentKeys {
    FolderContentKeys {
        current: fauna_core::folder_keys::ContentKeyGeneration {
            version: MDA_GENERATION,
            key: fauna_core::secret::SecretArray32::from(MDA_CONTENT_KEY),
            rotated_at: 1,
        },
        prior: vec![],
    }
}

/// Exactly what the bridge's `webdav_seal_path` FFI export computes.
fn mda_seal(path: &str) -> Vec<u8> {
    fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::content_key(MDA_CONTENT_KEY, MDA_GENERATION),
        &fauna_core::sync::path_hash(path),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// The MDA's session custody: content keys only, never a `BackupKey`
/// (`key-material-hierarchy.md` rule #7).
fn mda_custody() -> fauna_core::file_download::FileDownloadKeys {
    fauna_core::file_download::FileDownloadKeys {
        backup_key: None,
        mls_group_id: None,
        content_keys: Some(mda_content_keys()),
        ..Default::default()
    }
}

async fn serve_a_set(state: &Arc<AppState>, user_actor: [u8; 32], set: &str) {
    state
        .db
        .create_folder_with_options(
            set,
            &user_actor,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    state
        .db
        .update_folder_for_user(
            set,
            &user_actor,
            fauna_nest::db::FolderUpdate {
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// A DAV PUT records a seal the nest cannot open, and the PROPFIND listing
/// carries it back **with its salt** — then renders the true name even after the
/// plaintext column is perturbed to a decoy.
#[tokio::test]
async fn a_dav_write_seals_its_path_and_the_listing_renders_it_without_the_plaintext() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;
    serve_a_set(&state, user_actor, "photos").await;

    let manifest = "ab".repeat(32);
    let sealed = mda_seal(DAV_PATH);

    // ── PUT: the bridge seals bridge-side and records ───────────────────
    let bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_record_change",
        encode_canonical(&WebdavRecordChangeRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            path: DAV_PATH.into(),
            manifest_hash: Some(manifest.clone()),
            size_bytes: 321,
            change_type: "Created".into(),
            content_key_version: Some(MDA_GENERATION),
            if_match: None,
            if_none_match: None,
            path_sealed: Some(ByteBuf::from(sealed.clone())),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .expect("record a sealed DAV write");
    let reply: WebdavRecordChangeReply = decode(&bytes).unwrap();
    assert!(reply.seq >= 1);

    // ── The seal actually reached the row, not just the request ─────────
    {
        let conn = state.db.conn().await;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT path_sealed FROM sync_changes WHERE seq = ?1",
                rusqlite::params![reply.seq],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, sealed,
            "the DAV leg must persist the bridge's seal verbatim — a keyless \
             writer seam here is exactly what S4 exists to close"
        );
    }

    // ── Perturb the plaintext: the post-flip state, simulated ───────────
    // `path_hash` is untouched, which is the point — it is the convergent salt
    // the seal opens under, and it rides the wire beside the seal.
    {
        let conn = state.db.conn().await;
        let updated = conn
            .execute(
                "UPDATE sync_changes SET path = ?1 WHERE seq = ?2",
                rusqlite::params!["decoy/not-the-real-name.bin", reply.seq],
            )
            .unwrap();
        assert_eq!(updated, 1, "perturbed exactly the one row");
    }

    // ── PROPFIND ────────────────────────────────────────────────────────
    let bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_list_files",
        encode_canonical(&WebdavListFilesRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .expect("list served files");
    let listing: WebdavListFilesReply = decode(&bytes).unwrap();
    assert_eq!(listing.files.len(), 1);
    let row = &listing.files[0];

    assert_eq!(
        row.path_sealed.as_ref().map(|b| b.to_vec()),
        Some(sealed),
        "the listing must carry the seal — the nest forwards what it cannot read"
    );
    assert_eq!(
        row.path_hash.as_ref().map(|b| b.to_vec()),
        Some(fauna_core::sync::path_hash(DAV_PATH).to_vec()),
        "the listing must carry the SALT too; without it a scrubbed row is \
         unrenderable (the hole S2b hit on fauna.media.list)"
    );
    assert_eq!(row.path, "decoy/not-the-real-name.bin", "precondition");

    // ── The MDA renders the true name from the seal, not the decoy ──────
    let rendered = fauna_core::label_custody::render_path(
        &mda_custody(),
        row.path_sealed.as_ref().map(|b| &b[..]),
        &row.path,
        row.path_hash.as_ref().map(|b| &b[..]),
        fauna_core::path_crypto::LabelField::SyncChangePath,
    );
    assert_eq!(
        rendered,
        fauna_core::path_crypto::SealedLabelRender::Sealed(DAV_PATH.to_string()),
        "a plaintext-first renderer would have returned the decoy"
    );
}

/// The ratified degrade on the DAV leg: a session whose blob lacks the sealing
/// generation omits the entry rather than listing a blank or failing the
/// PROPFIND. Pinned here (not only in the FFI unit tests) because the wire is
/// what decides whether the MDA *can* degrade correctly — a listing that
/// dropped `path_hash` would force an omit even for a session holding the key.
#[tokio::test]
async fn a_dav_listing_row_the_session_cannot_open_omits_rather_than_leaking_or_failing() {
    let (router, state) = router_with_state().await;
    let user_actor = [42u8; 32];
    let bridge_actor = [55u8; 32];
    common::approve_mda(&state, bridge_actor).await;
    serve_a_set(&state, user_actor, "photos").await;

    let bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_record_change",
        encode_canonical(&WebdavRecordChangeRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            path: DAV_PATH.into(),
            manifest_hash: Some("cd".repeat(32)),
            size_bytes: 10,
            change_type: "Created".into(),
            content_key_version: Some(MDA_GENERATION),
            if_match: None,
            if_none_match: None,
            path_sealed: Some(ByteBuf::from(mda_seal(DAV_PATH))),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let reply: WebdavRecordChangeReply = decode(&bytes).unwrap();

    // Blank the plaintext outright — the post-flip shape (one row, so no PK
    // collision on the `(snapshot_id, path)` class of constraint).
    {
        let conn = state.db.conn().await;
        conn.execute(
            "UPDATE sync_changes SET path = '' WHERE seq = ?1",
            rusqlite::params![reply.seq],
        )
        .unwrap();
    }

    let bytes = dispatch(
        &router,
        state.clone(),
        bridge_actor,
        "fauna.bridges.webdav_list_files",
        encode_canonical(&WebdavListFilesRequest {
            actor_id: user_actor.to_vec(),
            folder: "photos".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let listing: WebdavListFilesReply = decode(&bytes).unwrap();
    assert_eq!(
        listing.files.len(),
        1,
        "the row is still listed on the wire"
    );
    let row = &listing.files[0];

    // A session holding a DIFFERENT generation's key — a removed member, or one
    // that has not synced the rotation.
    let wrong = fauna_core::file_download::FileDownloadKeys {
        content_keys: Some(FolderContentKeys {
            current: fauna_core::folder_keys::ContentKeyGeneration {
                version: MDA_GENERATION,
                key: fauna_core::secret::SecretArray32::from([0xff; 32]),
                rotated_at: 1,
            },
            prior: vec![],
        }),
        ..Default::default()
    };
    assert_eq!(
        fauna_core::label_custody::render_path(
            &wrong,
            row.path_sealed.as_ref().map(|b| &b[..]),
            &row.path,
            row.path_hash.as_ref().map(|b| &b[..]),
            fauna_core::path_crypto::LabelField::SyncChangePath,
        ),
        fauna_core::path_crypto::SealedLabelRender::Omit,
        "a wrong key must omit — never guess, never render the blank plaintext"
    );

    // ...and the session that DOES hold the key still renders it, proving the
    // omit above is about custody and not a missing salt on the wire.
    assert_eq!(
        fauna_core::label_custody::render_path(
            &mda_custody(),
            row.path_sealed.as_ref().map(|b| &b[..]),
            &row.path,
            row.path_hash.as_ref().map(|b| &b[..]),
            fauna_core::path_crypto::LabelField::SyncChangePath,
        ),
        fauna_core::path_crypto::SealedLabelRender::Sealed(DAV_PATH.to_string()),
    );
}
