//! The deposit door's gate and park, over the principal dispatch — the same
//! path both doors take.

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_mls::wrapped_blob::GrantWindow;
use fauna_protocol::folders::{
    DepositEnvelope, DepositPart, FolderDepositBeginReply, FolderDepositBeginRequest,
    FolderDepositCommitRequest, FolderDepositPartRequest, FolderDepositReply, FolderDepositRequest,
    FolderDepositsListReply, FolderDepositsListRequest, FolderDepositsPartReply,
    FolderDepositsPartRequest, FolderDepositsRetireReply, FolderDepositsRetireRequest,
    KIND_FOLDERS_DEPOSIT, KIND_FOLDERS_DEPOSIT_BEGIN, KIND_FOLDERS_DEPOSIT_COMMIT,
    KIND_FOLDERS_DEPOSIT_PART, KIND_FOLDERS_DEPOSITS_LIST, KIND_FOLDERS_DEPOSITS_PART,
    KIND_FOLDERS_DEPOSITS_RETIRE, MAX_DEPOSIT_BYTES, MAX_DEPOSIT_PART_BYTES,
};

use crate::db::CacheDb;
use crate::db::third_party_principals::{AttestedKeys, ExecutionForm, PrincipalAttestation};
use crate::principal_handlers::{PrincipalBinding, dispatch_principal};
use crate::routes::AppState;

const ACCOUNT: [u8; 32] = [0xA1; 32];
const OTHER: [u8; 32] = [0xB2; 32];
const CLIENT: &str = "https://app.example/client.json";
const HOLDER: [u8; 32] = [0x77; 32];
const NEVER: i64 = i64::MAX;
const MARKER: &[u8] = b"deposit-plaintext-marker";

struct Fixture {
    state: Arc<AppState>,
    binding: PrincipalBinding,
    folder: i64,
    msek: [u8; 32],
}

/// An account with a folder, a recipient key, and a principal consented
/// `fauna:folder:deposit:<that folder>` — with the owner's keyless grant over
/// it when `granted`.
async fn fixture(granted: bool) -> Fixture {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&ACCOUNT, "free", "test").await.unwrap();
    let folder = db.create_folder("drop-box", &ACCOUNT).await.unwrap();
    let scope = format!("fauna:folder:deposit:{folder}");
    db.record_atproto_oauth_grant(
        &ACCOUNT,
        b"family-1",
        CLIENT,
        Some("Example App"),
        &scope,
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
            execution_form: ExecutionForm::Remote,
            manifest: None,
        },
    )
    .await
    .unwrap();
    let principal_id = db.list_third_party_principals(&ACCOUNT).await.unwrap()[0]
        .principal_id
        .clone();
    let state = Arc::new(AppState::for_test(db));
    let msek = [9; 32];
    crate::test_support::seed_recipient_seal_key(&state.db, &ACCOUNT, &msek).await;
    if granted {
        put_grant(&state, &[1; 16], folder, NEVER).await;
    }
    Fixture {
        state,
        binding: PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: vec![scope],
            family_id: None,
        },
        folder,
        msek,
    }
}

async fn put_grant(state: &AppState, grant_id: &[u8; 16], folder: i64, expires: i64) {
    let now = crate::db::now_epoch_secs();
    let blob = fauna_client_capabilities::mint_folder_deposit_grant(
        &ACCOUNT,
        grant_id,
        &HOLDER,
        GrantWindow(
            u64::try_from(now - 60).unwrap(),
            u64::try_from(expires).unwrap(),
        ),
        &[folder],
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap();
    state
        .db
        .put_capability_grant(&ACCOUNT, grant_id, &HOLDER, expires, &blob)
        .await
        .unwrap();
}

fn request(folder: i64, name: &str, body: &[u8]) -> Bytes {
    fauna_protocol::encode_canonical(&FolderDepositRequest {
        folder_id: folder,
        name: name.into(),
        content_type: "text/plain".into(),
        body: ByteBuf::from(body.to_vec()),
        extra: Default::default(),
    })
    .unwrap()
}

async fn deposit(f: &Fixture, payload: Bytes) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch_principal(f.state.clone(), &f.binding, KIND_FOLDERS_DEPOSIT, payload).await
}

/// The journey: accepted, parked in the inbox sealed to the owner's
/// recipient key — the owner opens the name, type and bytes; nothing of them
/// rests in the clear.
#[tokio::test]
async fn a_deposit_is_accepted_and_parked_sealed_to_the_owner() {
    let f = fixture(true).await;
    let reply = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .expect("accepted");
    let reply: FolderDepositReply = fauna_protocol::decode_strict(&reply).unwrap();
    assert!(reply.accepted);

    let rows = f.state.db.list_folder_deposits(f.folder).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].principal_id, f.binding.principal_id);
    let sealed = &rows[0].sealed;
    assert!(
        !sealed.windows(MARKER.len()).any(|w| w == MARKER),
        "the body must not rest in the clear"
    );
    assert!(
        !sealed.windows(b"note.txt".len()).any(|w| w == b"note.txt"),
        "the name is content and rests sealed"
    );
    let opened: DepositEnvelope =
        fauna_protocol::decode_strict(&crate::test_support::open_recipient_record(sealed, &f.msek))
            .unwrap();
    assert_eq!(opened.name, "note.txt");
    assert_eq!(opened.content_type, "text/plain");
    assert_eq!(opened.body.as_ref(), MARKER);
}

/// The deposit is not a folder entry: the change feed `fauna.sync.files`
/// lists from holds nothing until a seat adopts it.
#[tokio::test]
async fn a_parked_deposit_is_not_a_folder_entry() {
    let f = fixture(true).await;
    deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .expect("accepted");
    assert!(
        f.state
            .db
            .get_files_for_folder(f.folder)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The scope is per folder: another of the account's folders is refused, with
/// the same answer as a folder that does not exist.
#[tokio::test]
async fn a_scope_naming_another_folder_is_refused() {
    let f = fixture(true).await;
    let other = f.state.db.create_folder("private", &ACCOUNT).await.unwrap();
    put_grant(&f.state, &[2; 16], other, NEVER).await;
    for folder in [other, other + 1000] {
        let err = deposit(&f, request(folder, "note.txt", MARKER))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.folders.permission_denied", "{folder}");
    }
    assert!(
        f.state
            .db
            .list_folder_deposits(other)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The scope alone admits nothing: the owner's keyless grant is the
/// revocation record the door re-resolves — none, or a lapsed one, refuses.
#[tokio::test]
async fn without_a_live_deposit_grant_the_scope_alone_is_refused() {
    let f = fixture(false).await;
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.permission_denied");

    let lapsed = crate::db::now_epoch_secs() - 1;
    put_grant(&f.state, &[3; 16], f.folder, lapsed).await;
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.permission_denied");
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A folder the scope names but another account owns is refused — the
/// grant is the owner's, and only the owner's own folders take deposits.
#[tokio::test]
async fn another_accounts_folder_is_refused() {
    let mut f = fixture(true).await;
    f.state
        .db
        .create_user(&OTHER, "free", "test")
        .await
        .unwrap();
    let theirs = f.state.db.create_folder("theirs", &OTHER).await.unwrap();
    f.binding.token_scopes = vec![format!("fauna:folder:deposit:{theirs}")];
    // The row's own scopes are what the session intersects with: re-consent
    // the principal for the other folder, and grant it, so only ownership
    // stands between it and the deposit.
    f.state
        .db
        .record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-2",
            CLIENT,
            Some("Example App"),
            &format!("fauna:folder:deposit:{theirs}"),
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
                execution_form: ExecutionForm::Remote,
                manifest: None,
            },
        )
        .await
        .unwrap();
    put_grant(&f.state, &[4; 16], theirs, NEVER).await;
    let err = deposit(&f, request(theirs, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.permission_denied");
    assert!(
        f.state
            .db
            .list_folder_deposits(theirs)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The ruled refusal pair: a metadata-only folder keeps no content here, so
/// there is nowhere to park a deposit.
#[tokio::test]
async fn a_metadata_only_folder_is_refused() {
    let f = fixture(true).await;
    f.state
        .db
        .conn()
        .await
        .execute(
            "UPDATE folders SET nest_content_residency = 'metadata_only' WHERE id = ?1",
            [f.folder],
        )
        .unwrap();
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.permission_denied");
    let detail = format!("{:?}", err.details);
    assert!(detail.contains("metadata-only"), "{detail}");
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The request's shape: one plain file name, a bounded body.
#[tokio::test]
async fn a_malformed_deposit_is_refused() {
    let f = fixture(true).await;
    for name in ["", "..", "a/b", "a\\b"] {
        let err = deposit(&f, request(f.folder, name, MARKER))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.folders.invalid_params", "{name:?}");
    }
    let err = deposit(
        &f,
        request(f.folder, "big.bin", &vec![0; MAX_DEPOSIT_BYTES + 1]),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_params");
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty()
    );
}

/// No actor class holds the kind: the account's own session is refused at
/// the central gate.
#[test]
fn no_actor_class_holds_the_deposit_kind() {
    use crate::bridge_method_allowlist::{CallerClass, is_permitted};
    for class in [
        CallerClass::User,
        CallerClass::Admin,
        CallerClass::BridgeMda,
    ] {
        assert!(!is_permitted(class, KIND_FOLDERS_DEPOSIT), "{class:?}");
    }
    assert!(is_permitted(CallerClass::ThirdParty, KIND_FOLDERS_DEPOSIT));
}

fn list_request(folder: i64, after: i64) -> Bytes {
    fauna_protocol::encode_canonical(&FolderDepositsListRequest {
        folder_id: folder,
        after,
        extra: Default::default(),
    })
    .unwrap()
}

fn retire_request(folder: i64, deposit_id: i64) -> Bytes {
    fauna_protocol::encode_canonical(&FolderDepositsRetireRequest {
        folder_id: folder,
        deposit_id,
        extra: Default::default(),
    })
    .unwrap()
}

/// The owner's list hands back the parked item sealed as it rests, and the
/// owner's retire drains it — once; a second retire answers `false`.
#[tokio::test]
async fn the_owner_lists_then_retires_a_parked_deposit() {
    let f = fixture(true).await;
    deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .expect("accepted");
    let reply: FolderDepositsListReply = fauna_protocol::decode_strict(
        &super::deposits_list_handler()(f.state.clone(), ACCOUNT, list_request(f.folder, 0))
            .await
            .expect("listed"),
    )
    .unwrap();
    assert_eq!(reply.items.len(), 1);
    assert!(!reply.more);
    let item = &reply.items[0];
    let opened: DepositEnvelope = fauna_protocol::decode_strict(
        &crate::test_support::open_recipient_record(&item.sealed, &f.msek),
    )
    .unwrap();
    assert_eq!(opened.name, "note.txt");

    for expected in [true, false] {
        let reply: FolderDepositsRetireReply = fauna_protocol::decode_strict(
            &super::deposits_retire_handler()(
                f.state.clone(),
                ACCOUNT,
                retire_request(f.folder, item.id),
            )
            .await
            .expect("retire answered"),
        )
        .unwrap();
        assert_eq!(reply.retired, expected);
    }
    let reply: FolderDepositsListReply = fauna_protocol::decode_strict(
        &super::deposits_list_handler()(f.state.clone(), ACCOUNT, list_request(f.folder, 0))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(reply.items.is_empty());
}

/// Another account reaches neither half of the owner's inbox: the folder
/// answers as one that does not exist.
#[tokio::test]
async fn another_account_cannot_list_or_retire_the_owners_inbox() {
    let f = fixture(true).await;
    f.state
        .db
        .create_user(&OTHER, "free", "test")
        .await
        .unwrap();
    deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .expect("accepted");
    let err = super::deposits_list_handler()(f.state.clone(), OTHER, list_request(f.folder, 0))
        .await
        .expect_err("not the owner");
    assert!(err.code.ends_with("not_found"), "{}", err.code);
    let err = super::deposits_retire_handler()(f.state.clone(), OTHER, retire_request(f.folder, 1))
        .await
        .expect_err("not the owner");
    assert!(err.code.ends_with("not_found"), "{}", err.code);
    assert_eq!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// The owner's half is the account's own apps' — never a third party's.
#[test]
fn only_the_account_holds_the_inbox_kinds() {
    use crate::bridge_method_allowlist::{CallerClass, is_permitted};
    for kind in [KIND_FOLDERS_DEPOSITS_LIST, KIND_FOLDERS_DEPOSITS_RETIRE] {
        assert!(is_permitted(CallerClass::User, kind), "{kind}");
        assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
        assert!(!is_permitted(CallerClass::ThirdParty, kind), "{kind}");
        assert!(!is_permitted(CallerClass::BridgeMda, kind), "{kind}");
    }
}

// ── deposits larger than one message ────────────────────────────────────────

fn encode<T: serde::Serialize>(v: &T) -> Bytes {
    fauna_protocol::encode_canonical(v).unwrap()
}

async fn begin(f: &Fixture) -> Result<ByteBuf, fauna_protocol::RpcError> {
    let reply = dispatch_principal(
        f.state.clone(),
        &f.binding,
        KIND_FOLDERS_DEPOSIT_BEGIN,
        encode(&FolderDepositBeginRequest {
            folder_id: f.folder,
            extra: Default::default(),
        }),
    )
    .await?;
    let reply: FolderDepositBeginReply = fauna_protocol::decode_strict(&reply).unwrap();
    Ok(reply.upload)
}

async fn part(
    f: &Fixture,
    upload: &ByteBuf,
    index: u32,
    body: &[u8],
) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch_principal(
        f.state.clone(),
        &f.binding,
        KIND_FOLDERS_DEPOSIT_PART,
        encode(&FolderDepositPartRequest {
            folder_id: f.folder,
            upload: upload.clone(),
            index,
            body: ByteBuf::from(body.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
}

async fn commit(
    f: &Fixture,
    upload: &ByteBuf,
    name: &str,
    parts: u32,
) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch_principal(
        f.state.clone(),
        &f.binding,
        KIND_FOLDERS_DEPOSIT_COMMIT,
        encode(&FolderDepositCommitRequest {
            folder_id: f.folder,
            upload: upload.clone(),
            name: name.into(),
            content_type: "application/octet-stream".into(),
            parts,
            extra: Default::default(),
        }),
    )
    .await
}

/// The journey: three parts, each sealed on arrival, adoptable only once the
/// commit lands — then the owner opens the header and every part, in order,
/// with the recipient key, and none of it rests in the clear.
#[tokio::test]
async fn a_staged_deposit_parks_sealed_parts_and_lists_only_once_committed() {
    let f = fixture(true).await;
    let upload = begin(&f).await.expect("begun");
    let bodies: [Vec<u8>; 3] = [
        [MARKER, b"-0"].concat(),
        [MARKER, b"-1"].concat(),
        [MARKER, b"-2"].concat(),
    ];
    // Out of order, and a resent index replaces itself.
    for i in [2u32, 0, 1, 1] {
        part(&f, &upload, i, &bodies[i as usize])
            .await
            .expect("part");
    }
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty(),
        "a staged upload is not adoptable"
    );
    let err = commit(&f, &upload, "scan.bin", 4).await.unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    commit(&f, &upload, "scan.bin", 3).await.expect("committed");
    // A retried commit answers alike and parks nothing more.
    commit(&f, &upload, "scan.bin", 3)
        .await
        .expect("committed again");

    let rows = f.state.db.list_folder_deposits(f.folder).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].sealed.windows(8).any(|w| w == b"scan.bin"));
    let header: DepositEnvelope = fauna_protocol::decode_strict(
        &crate::test_support::open_recipient_record(&rows[0].sealed, &f.msek),
    )
    .unwrap();
    assert_eq!(header.name, "scan.bin");
    assert_eq!(header.parts, 3);
    assert!(header.body.is_empty());
    for (i, body) in bodies.iter().enumerate() {
        let reply: FolderDepositsPartReply = fauna_protocol::decode_strict(
            &super::deposits_part_handler()(
                f.state.clone(),
                ACCOUNT,
                encode(&FolderDepositsPartRequest {
                    folder_id: f.folder,
                    deposit_id: rows[0].id,
                    index: u32::try_from(i).unwrap(),
                    extra: Default::default(),
                }),
            )
            .await
            .expect("part served"),
        )
        .unwrap();
        assert!(!reply.sealed.windows(MARKER.len()).any(|w| w == MARKER));
        let opened: DepositPart = fauna_protocol::decode_strict(
            &crate::test_support::open_recipient_record(&reply.sealed, &f.msek),
        )
        .unwrap();
        assert_eq!(opened.index as usize, i);
        assert_eq!(opened.body.as_ref(), body.as_slice());
    }
    // A committed upload takes no more parts.
    let err = part(&f, &upload, 3, b"late").await.unwrap_err();
    assert!(err.code.ends_with("invalid_request"), "{}", err.code);
}

/// Every call re-runs the gate: a grant revoked mid-upload ends it.
#[tokio::test]
async fn revoking_the_grant_mid_upload_refuses_the_rest() {
    let f = fixture(true).await;
    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, MARKER).await.expect("part");
    assert!(
        f.state
            .db
            .delete_capability_grant(&ACCOUNT, &[1; 16])
            .await
            .unwrap()
    );
    for err in [
        part(&f, &upload, 1, MARKER).await.unwrap_err(),
        commit(&f, &upload, "scan.bin", 1).await.unwrap_err(),
    ] {
        assert_eq!(err.code, "fauna.folders.permission_denied");
    }
}

/// An upload token names one principal's upload in one folder; a bad name
/// at commit drops the staged parts at once.
#[tokio::test]
async fn an_unknown_token_is_not_found_and_a_bad_name_discards_the_upload() {
    let f = fixture(true).await;
    let err = part(&f, &ByteBuf::from(vec![0; 16]), 0, MARKER)
        .await
        .unwrap_err();
    assert!(err.code.ends_with("not_found"), "{}", err.code);
    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, MARKER).await.expect("part");
    let err = commit(&f, &upload, "a/b", 1).await.unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    let err = commit(&f, &upload, "fine.bin", 1).await.unwrap_err();
    assert!(err.code.ends_with("not_found"), "discarded: {}", err.code);
}

/// The quota: where tier quotas are enforced, the tier's `max_inbox_bytes`
/// bounds the sealed bytes parked across the account — a part past it and a
/// single-message deposit past it are refused alike, and an adopted item's
/// retire gives the room back.
#[tokio::test]
async fn the_tier_inbox_quota_refuses_past_it_and_a_retire_gives_room_back() {
    let f = fixture(true).await;
    *f.state.enforce_tier_quotas.write().await = true;
    let mut tier = f.state.db.get_tier("free").await.unwrap().unwrap();
    // Room for one sealed full part and change, not two.
    tier.max_inbox_bytes =
        i64::try_from(MAX_DEPOSIT_PART_BYTES + MAX_DEPOSIT_PART_BYTES / 2).unwrap();
    f.state.db.update_tier(&tier).await.unwrap();

    let full = vec![7u8; MAX_DEPOSIT_PART_BYTES];
    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, &full).await.expect("first part fits");
    let err = part(&f, &upload, 1, &full).await.unwrap_err();
    assert_eq!(err.code, "fauna.folders.inbox_full");
    commit(&f, &upload, "big.bin", 1).await.expect("committed");
    let half = &full[..MAX_DEPOSIT_BYTES / 2];
    let err = deposit(&f, request(f.folder, "more.bin", half))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.inbox_full");

    let id = f.state.db.list_folder_deposits(f.folder).await.unwrap()[0].id;
    assert!(
        f.state
            .db
            .retire_folder_deposit(f.folder, id)
            .await
            .unwrap()
    );
    deposit(&f, request(f.folder, "more.bin", half))
        .await
        .expect("room again");
}

/// A storage-only tier (`max_inbox_bytes = 0`) parks nothing.
#[tokio::test]
async fn a_zero_inbox_tier_admits_no_deposit() {
    let f = fixture(true).await;
    *f.state.enforce_tier_quotas.write().await = true;
    let mut tier = f.state.db.get_tier("free").await.unwrap().unwrap();
    tier.max_inbox_bytes = 0;
    f.state.db.update_tier(&tier).await.unwrap();
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.inbox_full");
}

/// The larger-deposit kinds are the principal's, the part read the owner's.
#[test]
fn the_staged_kinds_are_third_party_only_and_the_part_read_the_owners() {
    use crate::bridge_method_allowlist::{CallerClass, is_permitted};
    for kind in [
        KIND_FOLDERS_DEPOSIT_BEGIN,
        KIND_FOLDERS_DEPOSIT_PART,
        KIND_FOLDERS_DEPOSIT_COMMIT,
    ] {
        assert!(is_permitted(CallerClass::ThirdParty, kind), "{kind}");
        for class in [
            CallerClass::User,
            CallerClass::Admin,
            CallerClass::BridgeMda,
        ] {
            assert!(!is_permitted(class, kind), "{kind} {class:?}");
        }
    }
    assert!(is_permitted(CallerClass::User, KIND_FOLDERS_DEPOSITS_PART));
    assert!(!is_permitted(
        CallerClass::ThirdParty,
        KIND_FOLDERS_DEPOSITS_PART
    ));
}
