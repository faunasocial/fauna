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
    KIND_FOLDERS_DEPOSITS_RETIRE, MAX_DEPOSIT_BYTES, MAX_DEPOSIT_CONTENT_TYPE_BYTES,
    MAX_DEPOSIT_PART_BYTES,
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
/// The depositor's writer key, attested at the consent and confined into
/// the owner's deposit grant.
const WRITER_SEED: [u8; 32] = [0x5D; 32];

fn writer_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&WRITER_SEED)
}

fn writer() -> [u8; 32] {
    writer_key().verifying_key().to_bytes()
}

/// The depositor's signature over the statement of `body` deposited into
/// `folder` as `name` with `content_type`.
fn sign(folder: i64, name: &str, content_type: &str, body: &[u8]) -> ByteBuf {
    let statement =
        fauna_protocol::folders::DepositStatement::for_body(folder, name, content_type, body);
    ByteBuf::from(statement.sign(&writer_key()).to_vec())
}

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
                writer_ed25519: Some(writer()),
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
    put_grant_held_by(state, grant_id, folder, expires, &HOLDER).await;
}

/// The owner's keyless deposit grant over `folder`, held by `holder`.
async fn put_grant_held_by(
    state: &AppState,
    grant_id: &[u8; 16],
    folder: i64,
    expires: i64,
    holder: &[u8; 32],
) {
    put_grant_to_writer(state, grant_id, folder, expires, holder, &writer()).await;
}

/// The owner's keyless deposit grant over `folder`, held by `holder` and
/// confined to `writer`.
async fn put_grant_to_writer(
    state: &AppState,
    grant_id: &[u8; 16],
    folder: i64,
    expires: i64,
    holder: &[u8; 32],
    writer: &[u8; 32],
) {
    let now = crate::db::now_epoch_secs();
    let blob = fauna_client_capabilities::mint_folder_deposit_grant(
        &ACCOUNT,
        grant_id,
        holder,
        writer,
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
        .put_capability_grant(&ACCOUNT, grant_id, holder, expires, &blob)
        .await
        .unwrap();
}

/// A signed single-message deposit of `body` as `name`.
fn request(folder: i64, name: &str, body: &[u8]) -> Bytes {
    fauna_protocol::encode_canonical(&FolderDepositRequest {
        folder_id: folder,
        name: name.into(),
        content_type: "text/plain".into(),
        body: ByteBuf::from(body.to_vec()),
        sig: sign(folder, name, "text/plain", body),
        ..Default::default()
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

/// The grant is resolved for the key the principal attested: the owner's live
/// deposit grant over this very folder, held by another key, admits nothing —
/// on the single-message door and the staged one alike.
#[tokio::test]
async fn a_grant_held_by_another_key_admits_nothing() {
    let f = fixture(false).await;
    put_grant_held_by(&f.state, &[5; 16], f.folder, NEVER, &[0x88; 32]).await;
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.permission_denied");
    let err = begin(&f).await.unwrap_err();
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
                    writer_ed25519: Some(writer()),
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

/// Commit `upload`, signed over `body` — the whole body its parts carry.
async fn commit(
    f: &Fixture,
    upload: &ByteBuf,
    name: &str,
    parts: u32,
    body: &[u8],
) -> Result<Bytes, fauna_protocol::RpcError> {
    commit_typed(f, upload, name, "application/octet-stream", parts, body).await
}

async fn commit_typed(
    f: &Fixture,
    upload: &ByteBuf,
    name: &str,
    content_type: &str,
    parts: u32,
    body: &[u8],
) -> Result<Bytes, fauna_protocol::RpcError> {
    commit_signed(
        f,
        upload,
        name,
        content_type,
        parts,
        blake3::hash(body).as_bytes(),
        sign(f.folder, name, content_type, body),
    )
    .await
}

async fn commit_signed(
    f: &Fixture,
    upload: &ByteBuf,
    name: &str,
    content_type: &str,
    parts: u32,
    hash: &[u8; 32],
    sig: ByteBuf,
) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch_principal(
        f.state.clone(),
        &f.binding,
        KIND_FOLDERS_DEPOSIT_COMMIT,
        encode(&FolderDepositCommitRequest {
            folder_id: f.folder,
            upload: upload.clone(),
            name: name.into(),
            content_type: content_type.into(),
            parts,
            hash: ByteBuf::from(hash.to_vec()),
            sig,
            ..Default::default()
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
    let err = commit(&f, &upload, "scan.bin", 4, &bodies.concat())
        .await
        .unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    commit(&f, &upload, "scan.bin", 3, &bodies.concat())
        .await
        .expect("committed");
    // A retried commit answers alike and parks nothing more.
    commit(&f, &upload, "scan.bin", 3, &bodies.concat())
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
        commit(&f, &upload, "scan.bin", 1, MARKER)
            .await
            .unwrap_err(),
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
    let err = commit(&f, &upload, "a/b", 1, MARKER).await.unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    let err = commit(&f, &upload, "fine.bin", 1, MARKER)
        .await
        .unwrap_err();
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
    commit(&f, &upload, "big.bin", 1, &full)
        .await
        .expect("committed");
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

/// The commit's sealed header counts against the quota like a part: an inbox
/// already at its limit refuses the commit as full, the upload stays staged,
/// and once there is room the same upload commits.
#[tokio::test]
async fn a_commit_past_the_inbox_quota_is_refused_and_stays_staged() {
    let f = fixture(true).await;
    *f.state.enforce_tier_quotas.write().await = true;
    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, MARKER).await.expect("part");
    let mut tier = f.state.db.get_tier("free").await.unwrap().unwrap();
    let roomy = tier.max_inbox_bytes;
    tier.max_inbox_bytes = 1;
    f.state.db.update_tier(&tier).await.unwrap();
    let err = commit(&f, &upload, "big.bin", 1, MARKER).await.unwrap_err();
    assert_eq!(err.code, "fauna.folders.inbox_full");
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty()
    );
    tier.max_inbox_bytes = roomy;
    f.state.db.update_tier(&tier).await.unwrap();
    commit(&f, &upload, "big.bin", 1, MARKER)
        .await
        .expect("still staged");
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

/// A media type past [`MAX_DEPOSIT_CONTENT_TYPE_BYTES`] is refused on both
/// kinds, and the refused commit leaves the upload staged for a fixed retry.
#[tokio::test]
async fn an_over_long_content_type_is_refused_on_both_kinds() {
    let f = fixture(true).await;
    let long = "a".repeat(MAX_DEPOSIT_CONTENT_TYPE_BYTES + 1);
    let err = deposit(
        &f,
        encode(&FolderDepositRequest {
            folder_id: f.folder,
            name: "note.txt".into(),
            content_type: long.clone(),
            body: ByteBuf::from(MARKER.to_vec()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);

    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, MARKER).await.expect("part");
    let err = commit_typed(&f, &upload, "big.bin", &long, 1, MARKER)
        .await
        .unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    let at_bound = "a".repeat(MAX_DEPOSIT_CONTENT_TYPE_BYTES);
    commit_typed(&f, &upload, "big.bin", &at_bound, 1, MARKER)
        .await
        .expect("a type at the bound commits");
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .len()
            == 1
    );
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

/// A part read reaches the account's own folders only: another account asking
/// for a part of the owner's committed item answers not found, while the
/// owner's own read is served.
#[tokio::test]
async fn another_account_cannot_fetch_a_part_of_the_owners_item() {
    let f = fixture(true).await;
    f.state
        .db
        .create_user(&OTHER, "free", "test")
        .await
        .unwrap();
    let upload = begin(&f).await.expect("begun");
    part(&f, &upload, 0, MARKER).await.expect("part");
    commit(&f, &upload, "scan.bin", 1, MARKER)
        .await
        .expect("committed");
    let id = f.state.db.list_folder_deposits(f.folder).await.unwrap()[0].id;
    let ask = || {
        encode(&FolderDepositsPartRequest {
            folder_id: f.folder,
            deposit_id: id,
            index: 0,
            extra: Default::default(),
        })
    };
    let err = super::deposits_part_handler()(f.state.clone(), OTHER, ask())
        .await
        .expect_err("not the owner");
    assert!(err.code.ends_with("not_found"), "{}", err.code);
    super::deposits_part_handler()(f.state.clone(), ACCOUNT, ask())
        .await
        .expect("the owner's read is served");
}

/// A begun upload parks no byte the quota counts, so the staged uploads a
/// principal may hold open are counted: past the cap a begin is refused with a
/// code of its own (HTTP `429`), and committing one gives its place back.
#[tokio::test]
async fn staged_uploads_past_the_principal_cap_are_refused() {
    let f = fixture(true).await;
    let mut open = Vec::new();
    for _ in 0..super::MAX_STAGED_DEPOSIT_UPLOADS_PER_PRINCIPAL {
        open.push(begin(&f).await.expect("within the cap"));
    }
    let err = begin(&f).await.unwrap_err();
    assert_eq!(err.code, "fauna.folders.too_many_uploads");
    assert_eq!(
        crate::principal_session::rpc_error_response(&err).status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS
    );
    part(&f, &open[0], 0, MARKER).await.expect("part");
    commit(&f, &open[0], "scan.bin", 1, MARKER)
        .await
        .expect("committed");
    begin(&f).await.expect("a committed upload frees its place");
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

// ── The depositor's signature (`folder-deposit.md` § The depositor signs, the
// owner's device verifies) ──────────────────────────────────────────────────

fn signed_with(folder: i64, name: &str, body: &[u8], sig: ByteBuf) -> Bytes {
    encode(&FolderDepositRequest {
        folder_id: folder,
        name: name.into(),
        content_type: "text/plain".into(),
        body: ByteBuf::from(body.to_vec()),
        sig,
        ..Default::default()
    })
}

/// A deposit with no signature, one by another key, one over another body or
/// name, and one of the wrong length are each refused `invalid_params`, and
/// none is parked.
#[tokio::test]
async fn an_unsigned_or_mis_signed_deposit_is_refused() {
    let f = fixture(true).await;
    let other_key = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
    let by_other = ByteBuf::from(
        fauna_protocol::folders::DepositStatement::for_body(
            f.folder,
            "note.txt",
            "text/plain",
            MARKER,
        )
        .sign(&other_key)
        .to_vec(),
    );
    for sig in [
        ByteBuf::new(),
        by_other,
        sign(f.folder, "note.txt", "text/plain", b"another body"),
        sign(f.folder, "other.txt", "text/plain", MARKER),
        ByteBuf::from(vec![0; 63]),
    ] {
        let err = deposit(&f, signed_with(f.folder, "note.txt", MARKER, sig))
            .await
            .unwrap_err();
        assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    }
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty(),
        "a refused deposit parks nothing"
    );
}

/// The parked envelope carries the caller's attested writer key — never one
/// the request names — and the depositor's signature, which verifies over the
/// statement the owner's device rebuilds.
#[tokio::test]
async fn the_parked_envelope_carries_the_writer_and_the_signature() {
    let f = fixture(true).await;
    deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .expect("accepted");
    let rows = f.state.db.list_folder_deposits(f.folder).await.unwrap();
    let opened: DepositEnvelope = fauna_protocol::decode_strict(
        &crate::test_support::open_recipient_record(&rows[0].sealed, &f.msek),
    )
    .unwrap();
    assert_eq!(opened.writer, Some(writer()));
    fauna_protocol::folders::DepositStatement::for_body(
        f.folder,
        &opened.name,
        &opened.content_type,
        &opened.body,
    )
    .verify(&writer(), &opened.sig)
    .expect("the envelope's signature verifies under its writer");
}

/// A principal that attested no writer key cannot sign, so is refused at the
/// gate with the gate's one answer, even holding a grant and a signature.
#[tokio::test]
async fn a_writer_less_principal_is_not_depositable() {
    let f = fixture(true).await;
    let db = &f.state.db;
    db.create_user(&OTHER, "free", "test").await.unwrap();
    let folder = db.create_folder("drop-box", &OTHER).await.unwrap();
    let scope = format!("fauna:folder:deposit:{folder}");
    db.record_atproto_oauth_grant(
        &OTHER,
        b"family-w",
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
    let principal_id = db.list_third_party_principals(&OTHER).await.unwrap()[0]
        .principal_id
        .clone();
    let now = crate::db::now_epoch_secs();
    let blob = fauna_client_capabilities::mint_folder_deposit_grant(
        &OTHER,
        &[7; 16],
        &HOLDER,
        &writer(),
        GrantWindow(
            u64::try_from(now - 60).unwrap(),
            u64::try_from(NEVER).unwrap(),
        ),
        &[folder],
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap();
    db.put_capability_grant(&OTHER, &[7; 16], &HOLDER, NEVER, &blob)
        .await
        .unwrap();
    crate::test_support::seed_recipient_seal_key(db, &OTHER, &[3; 32]).await;
    let binding = PrincipalBinding {
        account: OTHER,
        principal_id,
        token_scopes: vec![scope],
        family_id: None,
    };
    let err = dispatch_principal(
        f.state.clone(),
        &binding,
        KIND_FOLDERS_DEPOSIT,
        request(folder, "note.txt", MARKER),
    )
    .await
    .unwrap_err();
    assert!(err.code.ends_with("permission_denied"), "{}", err.code);
}

/// A grant whose deposit tuple names another writer admits nothing: the door
/// admits a deposit only under a tuple confined to the caller's own writer.
#[tokio::test]
async fn a_grant_naming_another_writer_admits_nothing() {
    let f = fixture(false).await;
    put_grant_to_writer(&f.state, &[1; 16], f.folder, NEVER, &HOLDER, &[0x44; 32]).await;
    let err = deposit(&f, request(f.folder, "note.txt", MARKER))
        .await
        .unwrap_err();
    assert!(err.code.ends_with("permission_denied"), "{}", err.code);
}

/// The staged form verifies authorship over the request's own hash and the
/// staged parts' summed length: a commit by another key, over a length the
/// parts do not sum to, unsigned, or with a hash that is not 32 bytes is
/// refused and the upload stays staged; one the writer signed over the whole
/// body is accepted, and its header carries the writer and the signature.
#[tokio::test]
async fn the_staged_form_verifies_authorship_at_the_commit() {
    let f = fixture(true).await;
    let upload = begin(&f).await.expect("begun");
    let bodies = [b"first-".to_vec(), b"second".to_vec()];
    for (i, body) in bodies.iter().enumerate() {
        part(&f, &upload, u32::try_from(i).unwrap(), body)
            .await
            .expect("part");
    }
    let whole = bodies.concat();
    let hash = *blake3::hash(&whole).as_bytes();
    let other_key = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
    let statement = |len: u64| fauna_protocol::folders::DepositStatement {
        folder_id: f.folder,
        name: "scan.bin".into(),
        content_type: "application/octet-stream".into(),
        len,
        hash,
    };
    let len = whole.len() as u64;
    for sig in [
        ByteBuf::new(),
        ByteBuf::from(statement(len).sign(&other_key).to_vec()),
        ByteBuf::from(statement(len + 1).sign(&writer_key()).to_vec()),
    ] {
        let err = commit_signed(
            &f,
            &upload,
            "scan.bin",
            "application/octet-stream",
            2,
            &hash,
            sig,
        )
        .await
        .unwrap_err();
        assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    }
    let err = dispatch_principal(
        f.state.clone(),
        &f.binding,
        KIND_FOLDERS_DEPOSIT_COMMIT,
        encode(&FolderDepositCommitRequest {
            folder_id: f.folder,
            upload: upload.clone(),
            name: "scan.bin".into(),
            content_type: "application/octet-stream".into(),
            parts: 2,
            hash: ByteBuf::from(vec![0; 31]),
            sig: ByteBuf::from(statement(len).sign(&writer_key()).to_vec()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert!(err.code.ends_with("invalid_params"), "{}", err.code);
    assert!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .is_empty(),
        "a refused commit leaves the upload staged"
    );
    commit(&f, &upload, "scan.bin", 2, &whole)
        .await
        .expect("a commit the writer signed is accepted");
    let rows = f.state.db.list_folder_deposits(f.folder).await.unwrap();
    let header: DepositEnvelope = fauna_protocol::decode_strict(
        &crate::test_support::open_recipient_record(&rows[0].sealed, &f.msek),
    )
    .unwrap();
    assert_eq!(header.writer, Some(writer()));
    statement(len)
        .verify(&writer(), &header.sig)
        .expect("the header's signature verifies under its writer");
}

/// The HTTP door hashes the body as it streams and verifies the signature
/// before the item becomes adoptable — over a body that fits one message and
/// one cut into parts alike: a signature over another body is refused and
/// nothing is parked; the writer's signature over the body is accepted.
#[tokio::test]
async fn the_http_door_binds_the_signature_to_the_streamed_body() {
    let f = fixture(true).await;
    let door = super::HttpDoor {
        state: f.state.clone(),
        binding: f.binding.clone(),
        folder_id: f.folder,
    };
    let sig_over = |body: &[u8]| -> [u8; 64] {
        fauna_protocol::folders::DepositStatement::for_body(f.folder, "scan.bin", "image/png", body)
            .sign(&writer_key())
    };
    let small = MARKER.to_vec();
    let large: Vec<u8> = (0..MAX_DEPOSIT_PART_BYTES * 2 + 5)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    for body in [&small, &large] {
        let mut altered = body.clone();
        altered[0] ^= 1;
        let refused = door
            .deposit(
                "scan.bin".into(),
                "image/png".into(),
                sig_over(&altered),
                axum::body::Body::from(body.clone()),
            )
            .await;
        match refused {
            Err(super::DoorError::Rpc(err)) => {
                assert!(err.code.ends_with("invalid_params"), "{}", err.code);
            }
            _ => panic!("a signature over another body must be refused"),
        }
        assert!(
            f.state
                .db
                .list_folder_deposits(f.folder)
                .await
                .unwrap()
                .is_empty(),
            "nothing is adoptable after a refused signature"
        );
    }
    for body in [&small, &large] {
        assert!(
            door.deposit(
                "scan.bin".into(),
                "image/png".into(),
                sig_over(body),
                axum::body::Body::from(body.clone()),
            )
            .await
            .is_ok(),
            "the writer's signature over the body is accepted"
        );
    }
    assert_eq!(
        f.state
            .db
            .list_folder_deposits(f.folder)
            .await
            .unwrap()
            .len(),
        2
    );
}

/// The signature header is 64 bytes of unpadded base64url, or absent.
#[test]
fn the_signature_header_is_sixty_four_bytes_of_base64url() {
    use base64::Engine as _;
    let headers = |v: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert(super::DEPOSIT_SIGNATURE_HEADER, v.parse().unwrap());
        h
    };
    let b64 = |raw: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    assert_eq!(
        super::deposit_signature(&headers(&b64(&[7; 64]))),
        Some([7; 64])
    );
    assert_eq!(
        super::deposit_signature(&axum::http::HeaderMap::new()),
        None
    );
    for bad in [b64(&[7; 63]), b64(&[7; 65]), "not base64!".to_string()] {
        assert_eq!(super::deposit_signature(&headers(&bad)), None, "{bad}");
    }
}
