//! Third-party deposit adoption (`docs/goal/behavior/file-sync.md`
//! § Third-party deposit ingress): a parked item, sealed to the owner's
//! recipient key, lands in the sync root through the write door, is recorded
//! as an ordinary change and retired once durable — idempotently on the
//! deposit id.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use fauna_core::crypto::BackupKey;
use fauna_core::data::{ContentHash, FoldersConfig};
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretArray32;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::folders::{DepositEnvelope, DepositPart, DepositStatement, ParkedDeposit};
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::SyncDb;
use crate::engine::{DepositAdoption, SyncEngine};
use crate::ignore::IgnoreMatcher;
use crate::nest_api::FakeSyncControl;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

const FOLDER: i64 = 7;
const MSEK: [u8; 32] = [0x5E; 32];
const BODY: &[u8] = b"a third party's deposited bytes";

fn test_sync_engine(server_uri: &str, watch_dir: std::path::PathBuf) -> SyncEngine {
    let kp = ActorKeypair::generate();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        reqwest::Client::new(),
    ));
    let device_id = [0u8; 32];
    SyncEngine::new(
        watch_dir,
        SyncDb::open_in_memory().unwrap(),
        SyncClient::new(auth, &device_id),
        Some("drop-box".to_string()),
        device_id,
        None,
        None,
        Some(BackupKey::from_bytes([0x61u8; 32]).into()),
        None,
        None,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate()),
        crate::config::SyncMode::Sync,
    )
}

/// The writers the owner's grant log authorizes for [`FOLDER`], shared with
/// the test so it can land a `Mint` between passes.
type Writers = Arc<Mutex<BTreeSet<[u8; 32]>>>;

/// The account's mail custody and grant log, as the account store answers
/// them.
struct MailCustody {
    mseks: Vec<[u8; 32]>,
    writers: Writers,
}

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for MailCustody {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        Ok(FoldersConfig::default())
    }
    async fn recipient_mseks(&self) -> anyhow::Result<Vec<SecretArray32>> {
        Ok(self.mseks.iter().map(|m| (*m).into()).collect())
    }
    async fn deposit_writers(&self, folder_id: i64) -> anyhow::Result<BTreeSet<[u8; 32]>> {
        if folder_id != FOLDER {
            return Ok(BTreeSet::new());
        }
        Ok(self.writers.lock().unwrap().clone())
    }
}

/// The depositor the owner's log authorizes — the app the consent named.
fn depositor() -> SigningKey {
    SigningKey::from_bytes(&[0xD1; 32])
}

/// Another principal's writer key, which the log never named for [`FOLDER`].
fn stranger() -> SigningKey {
    SigningKey::from_bytes(&[0xD2; 32])
}

fn writer_of(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

/// How a test envelope is signed.
enum Signing<'a> {
    /// Over its own statement, by this key — what an honest door parks.
    By(&'a SigningKey),
    /// Names no writer and carries no signature.
    Unsigned,
    /// Names `writer` but is signed over another body.
    OverOtherBytes(&'a SigningKey),
}

/// A single-message envelope, signed as `signing` says.
fn envelope_signed(name: &str, body: &[u8], signing: &Signing<'_>) -> DepositEnvelope {
    let mut env = DepositEnvelope {
        name: name.into(),
        content_type: "text/plain".into(),
        body: body.to_vec().into(),
        parts: 0,
        ..Default::default()
    };
    sign(&mut env, body, signing);
    env
}

/// Stamp `env` with the writer and the signature over `body` as `signing`
/// says — the statement the nest's door verified before it parked the item.
fn sign(env: &mut DepositEnvelope, body: &[u8], signing: &Signing<'_>) {
    let (key, signed_body): (&SigningKey, &[u8]) = match signing {
        Signing::Unsigned => return,
        Signing::By(key) => (key, body),
        Signing::OverOtherBytes(key) => (key, b"bytes the depositor never sent"),
    };
    let statement = DepositStatement::for_body(FOLDER, &env.name, &env.content_type, signed_body);
    env.writer = Some(writer_of(key));
    env.sig = statement.sign(key).to_vec().into();
}

fn envelope(name: &str, body: &[u8]) -> Vec<u8> {
    encode(&envelope_signed(name, body, &Signing::By(&depositor())))
}

fn encode(env: &DepositEnvelope) -> Vec<u8> {
    fauna_protocol::encode_canonical(env).unwrap().to_vec()
}

/// Seal any plaintext as the nest's door seals it: X-Wing to the owner's
/// standing key.
fn seal_xwing(msek: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let kp = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(msek);
    fauna_mls::wrapped_blob::seal_to_recipient_xwing(plaintext, &kp.public)
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
}

fn sealed_hybrid(msek: &[u8; 32], name: &str, body: &[u8]) -> Vec<u8> {
    seal_xwing(msek, &envelope(name, body))
}

/// A larger item as the staged upload parks it: a header naming the part
/// count, and each part sealed on its own with its index inside the seal.
fn park_parts(control: &FakeSyncControl, id: i64, name: &str, parts: &[&[u8]]) {
    let mut header = DepositEnvelope {
        name: name.into(),
        content_type: "application/octet-stream".into(),
        body: Vec::new().into(),
        parts: u32::try_from(parts.len()).unwrap(),
        ..Default::default()
    };
    sign(&mut header, &parts.concat(), &Signing::By(&depositor()));
    let header = encode(&header);
    control.park_deposit(FOLDER, parked(id, seal_xwing(&MSEK, &header)));
    for (i, body) in parts.iter().enumerate() {
        park_part(
            control,
            id,
            u32::try_from(i).unwrap(),
            u32::try_from(i).unwrap(),
            body,
        );
    }
}

/// Park part `slot` of item `id`, sealed as part `sealed_index`.
fn park_part(control: &FakeSyncControl, id: i64, slot: u32, sealed_index: u32, body: &[u8]) {
    let part = fauna_protocol::encode_canonical(&DepositPart {
        index: sealed_index,
        body: body.to_vec().into(),
        extra: Default::default(),
    })
    .unwrap();
    control.park_deposit_part(id, slot, seal_xwing(&MSEK, &part));
}

/// The classical degrade the door falls back to when the X-Wing seal fails.
fn sealed_classical(msek: &[u8; 32], name: &str, body: &[u8]) -> Vec<u8> {
    let (_, public) = fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(msek);
    fauna_mls::wrapped_blob::seal_to_recipient(&envelope(name, body), &public)
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
}

fn parked(id: i64, sealed: Vec<u8>) -> ParkedDeposit {
    ParkedDeposit {
        id,
        sealed: sealed.into(),
        received_at: 1,
        extra: Default::default(),
    }
}

struct Seat {
    engine: SyncEngine,
    watch: tempfile::TempDir,
    control: FakeSyncControl,
    /// The writers the owner's log authorizes for [`FOLDER`] — a test lands a
    /// `Mint` by inserting one.
    writers: Writers,
    _server: MockServer,
}

/// A seat whose log authorizes [`depositor`] for [`FOLDER`].
async fn seat(mseks: Vec<[u8; 32]>) -> Seat {
    seat_authorizing(mseks, [writer_of(&depositor())].into()).await
}

async fn seat_authorizing(mseks: Vec<[u8; 32]>, writers: BTreeSet<[u8; 32]>) -> Seat {
    let server = MockServer::start().await;
    MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf());
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    let writers: Writers = Arc::new(Mutex::new(writers));
    engine.set_deposit_inbox(
        FOLDER,
        Arc::new(MailCustody {
            mseks,
            writers: Arc::clone(&writers),
        }),
    );
    Seat {
        engine,
        watch,
        control,
        writers,
        _server: server,
    }
}

/// The user's files in the seat's root — the engine's own dot-dirs aside.
fn user_files(s: &Seat) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(s.watch.path())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// The journey, in two passes over a harness whose change record never lands
/// (the WS-RPC record has no mock): the first pass lands the item on disk
/// through the write door and keeps it parked, because nothing is retired that
/// is not durable; once the row is durable the next pass finds it landed and
/// retires it.
#[tokio::test]
async fn a_parked_deposit_lands_and_retires_once_its_row_is_durable() {
    let s = seat(vec![MSEK]).await;
    s.control
        .park_deposit(FOLDER, parked(1, sealed_hybrid(&MSEK, "note.txt", BODY)));

    let first = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        first,
        DepositAdoption {
            deferred: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        std::fs::read(s.watch.path().join("note.txt")).unwrap(),
        BODY
    );
    assert_eq!(
        s.control.parked_ids(FOLDER),
        vec![1],
        "not durable, not retired"
    );
    // Ruling 3 (`folder-deposit.md`): whichever upload records exactly these
    // bytes at this path names the admitted writer as the depositor; other
    // bytes there are the owner's own edit.
    let hash = ContentHash::of_raw(BODY);
    assert_eq!(
        s.engine.deposit_mark("note.txt", &hash),
        Some(writer_of(&depositor())),
        "the landed bytes carry their depositor into the record"
    );
    assert_eq!(
        s.engine
            .deposit_mark("note.txt", &ContentHash::of_raw(b"an edit")),
        None,
        "an edit after the landing is not the deposit"
    );

    // The record landed (as the watcher's own upload, or a later pass's).
    s.engine
        .mark_hydrated("note.txt", ContentHash::of_raw(BODY))
        .unwrap();
    let second = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        second,
        DepositAdoption {
            already_landed: 1,
            landed: vec!["note.txt".to_string()],
            ..Default::default()
        }
    );
    assert!(s.control.parked_ids(FOLDER).is_empty());
    assert_eq!(
        s.engine.deposit_mark("note.txt", &hash),
        None,
        "a retired item's mark is forgotten"
    );
    assert_eq!(
        user_files(&s),
        vec!["note.txt".to_string()],
        "one file, never a duplicate"
    );
}

/// A second seat that finds the item already landed — another seat adopted
/// it and its record reached this one — writes nothing and retires it.
#[tokio::test]
async fn a_seat_that_finds_the_item_landed_only_retires_it() {
    let s = seat(vec![MSEK]).await;
    // The other seat's adopted file, as this seat holds it once its record
    // arrived: on disk, its row's head these bytes.
    std::fs::write(s.watch.path().join("note.txt"), BODY).unwrap();
    let _ = s.engine.upload_file("note.txt").await;
    s.engine
        .mark_hydrated("note.txt", ContentHash::of_raw(BODY))
        .unwrap();
    s.control
        .park_deposit(FOLDER, parked(4, sealed_hybrid(&MSEK, "note.txt", BODY)));
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(done.already_landed, 1);
    assert!(s.control.parked_ids(FOLDER).is_empty());
    assert_eq!(user_files(&s), vec!["note.txt".to_string()]);
}

/// Another file already holds the item's name: the item lands beside it,
/// under the name every seat derives from the deposit id, and the user's file
/// is untouched.
#[tokio::test]
async fn a_taken_name_lands_the_item_under_its_deposit_variant() {
    let s = seat(vec![MSEK]).await;
    std::fs::write(s.watch.path().join("report.pdf"), b"the user's own report").unwrap();
    s.control
        .park_deposit(FOLDER, parked(17, sealed_hybrid(&MSEK, "report.pdf", BODY)));
    s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        std::fs::read(s.watch.path().join("report.pdf")).unwrap(),
        b"the user's own report"
    );
    assert_eq!(
        std::fs::read(s.watch.path().join("report (deposit 17).pdf")).unwrap(),
        BODY
    );
}

/// The classical degrade opens too, and so does an item sealed before a
/// mail-key rotation, under a retained prior MSEK.
#[tokio::test]
async fn the_classical_degrade_and_a_prior_msek_both_open() {
    let prior = [0x11; 32];
    let s = seat(vec![MSEK, prior]).await;
    s.control
        .park_deposit(FOLDER, parked(1, sealed_classical(&MSEK, "a.txt", b"a")));
    s.control
        .park_deposit(FOLDER, parked(2, sealed_hybrid(&prior, "b.txt", b"b")));
    s.engine.adopt_deposits().await.unwrap();
    assert_eq!(std::fs::read(s.watch.path().join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(s.watch.path().join("b.txt")).unwrap(), b"b");
}

/// An item this seat cannot open — sealed to a key it never derived — stays
/// parked and lands nothing; a seat with no MSEK at all adopts nothing.
#[tokio::test]
async fn an_unopenable_item_stays_parked() {
    let s = seat(vec![MSEK]).await;
    s.control
        .park_deposit(FOLDER, parked(1, sealed_hybrid(&[0x22; 32], "x.txt", BODY)));
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(done.deferred, 1);
    assert!(!s.watch.path().join("x.txt").exists());
    assert_eq!(s.control.parked_ids(FOLDER), vec![1]);

    let keyless = seat(Vec::new()).await;
    keyless
        .control
        .park_deposit(FOLDER, parked(1, sealed_hybrid(&MSEK, "x.txt", BODY)));
    assert_eq!(
        keyless.engine.adopt_deposits().await.unwrap(),
        DepositAdoption::default()
    );
    assert!(!keyless.watch.path().join("x.txt").exists());
}

/// An item larger than one message lands as its parts concatenated in index
/// order, and its retire takes the parts with it.
#[tokio::test]
async fn a_multi_part_item_lands_its_parts_in_order() {
    let s = seat(vec![MSEK]).await;
    park_parts(
        &s.control,
        3,
        "scan.bin",
        &[b"first-", b"second-", b"third"],
    );
    s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        std::fs::read(s.watch.path().join("scan.bin")).unwrap(),
        b"first-second-third"
    );
    s.engine
        .mark_hydrated("scan.bin", ContentHash::of_raw(b"first-second-third"))
        .unwrap();
    assert_eq!(s.engine.adopt_deposits().await.unwrap().already_landed, 1);
    assert!(s.control.parked_ids(FOLDER).is_empty());
}

/// A part whose sealed index is not its place — swapped at rest — refuses
/// the item: nothing lands, it stays parked.
#[tokio::test]
async fn a_part_out_of_its_place_leaves_the_item_parked() {
    let s = seat(vec![MSEK]).await;
    let mut header = DepositEnvelope {
        name: "scan.bin".into(),
        parts: 2,
        ..Default::default()
    };
    sign(&mut header, b"ab", &Signing::By(&depositor()));
    let header = encode(&header);
    s.control
        .park_deposit(FOLDER, parked(5, seal_xwing(&MSEK, &header)));
    park_part(&s.control, 5, 0, 1, b"b");
    park_part(&s.control, 5, 1, 0, b"a");
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(done.deferred, 1);
    assert!(!s.watch.path().join("scan.bin").exists());
    assert_eq!(s.control.parked_ids(FOLDER), vec![5]);
    let stages: Vec<_> = std::fs::read_dir(s.watch.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().starts_with(".fauna-deposit-"))
        .collect();
    assert!(
        stages.is_empty(),
        "the refused item's staged parts leave no temp file behind: {stages:?}"
    );
}

/// An engine no inbox armed — a member's, a cross-nest set's — never even
/// lists.
#[tokio::test]
async fn an_unarmed_engine_never_lists() {
    let server = MockServer::start().await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf());
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    assert_eq!(
        engine.adopt_deposits().await.unwrap(),
        DepositAdoption::default()
    );
    assert!(control.calls().is_empty());
}

/// **An on-demand root adopts too** — the linux FUSE root's off-disk posture,
/// where a placeholder is a row and never a file on the disk. A cloud-only file
/// holding the item's name is a taken name, never a free one: the item lands
/// beside it under its deposit variant, the placeholder row is left as it was,
/// and once the item's row is durable the pass retires it and names the path
/// the root flips in-sync.
#[tokio::test]
async fn an_on_demand_root_lands_the_item_beside_a_placeholder_holding_its_name() {
    let s = seat(vec![MSEK]).await;
    s.engine.set_placeholders_off_disk();
    // The fold's cloud-only row for the user's own report — no bytes here.
    let theirs = ContentHash::of_raw(b"the user's own report");
    s.engine
        .db()
        .upsert_entry(
            "report.pdf",
            None,
            None,
            Some(theirs),
            crate::db::SyncState::Placeholder,
            0,
            1_700_000_000,
            21,
            1,
            None,
        )
        .unwrap();
    s.control
        .park_deposit(FOLDER, parked(17, sealed_hybrid(&MSEK, "report.pdf", BODY)));

    let first = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(first.deferred, 1, "not durable yet, not retired");
    assert!(
        !s.watch.path().join("report.pdf").exists(),
        "the placeholder stays a row: nothing is written over the user's file"
    );
    assert_eq!(
        s.engine
            .db()
            .get_entry("report.pdf")
            .unwrap()
            .unwrap()
            .state,
        crate::db::SyncState::Placeholder
    );
    let variant = "report (deposit 17).pdf";
    assert_eq!(std::fs::read(s.watch.path().join(variant)).unwrap(), BODY);

    s.engine
        .mark_hydrated(variant, ContentHash::of_raw(BODY))
        .unwrap();
    let second = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        second,
        DepositAdoption {
            already_landed: 1,
            landed: vec![variant.to_string()],
            ..Default::default()
        }
    );
    assert!(s.control.parked_ids(FOLDER).is_empty());
}

/// Park `env` as item `id`, sealed to the owner's recipient key — what a nest
/// can always do, the key being public.
fn park(s: &Seat, id: i64, env: &DepositEnvelope) {
    s.control
        .park_deposit(FOLDER, parked(id, seal_xwing(&MSEK, &encode(env))));
}

/// Nothing of the item landed, and it is still parked.
fn assert_left_parked(s: &Seat, id: i64, name: &str) {
    assert!(
        !s.watch.path().join(name).exists(),
        "a refused item lands nothing"
    );
    assert!(
        s.control.parked_ids(FOLDER).contains(&id),
        "a refused item is never retired"
    );
}

/// **The adopter's check** (`folder-deposit.md` § The depositor signs, the
/// owner's device verifies): an item sealed to the owner's public recipient
/// key but naming no writer and carrying no signature — what a nest could
/// park of its own making — is refused and left parked.
#[tokio::test]
async fn an_unsigned_item_is_refused_and_left_parked() {
    let s = seat(vec![MSEK]).await;
    park(
        &s,
        1,
        &envelope_signed("forged.txt", BODY, &Signing::Unsigned),
    );
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        done,
        DepositAdoption {
            refused: 1,
            ..Default::default()
        }
    );
    assert_left_parked(&s, 1, "forged.txt");
}

/// A signature that does not verify over the statement the seat rebuilds —
/// signed over other bytes, or with the writer swapped — is refused.
#[tokio::test]
async fn a_signature_over_other_bytes_is_refused() {
    let s = seat(vec![MSEK]).await;
    park(
        &s,
        1,
        &envelope_signed("swapped.txt", BODY, &Signing::OverOtherBytes(&depositor())),
    );
    let mut renamed = envelope_signed("orig.txt", BODY, &Signing::By(&depositor()));
    renamed.name = "renamed.txt".into();
    park(&s, 2, &renamed);
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(done.refused, 2);
    assert_left_parked(&s, 1, "swapped.txt");
    assert_left_parked(&s, 2, "renamed.txt");
}

/// A writer the log never authorized for this folder — a validly signed item
/// from another principal (or a key a nest minted for itself) — is refused and
/// left parked; once the `Mint` naming it reaches this replica, the first pass
/// after adopts it.
#[tokio::test]
async fn an_unauthorized_writer_waits_parked_until_its_mint_lands() {
    let s = seat(vec![MSEK]).await;
    park(
        &s,
        9,
        &envelope_signed("late.txt", BODY, &Signing::By(&stranger())),
    );
    let first = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(first.refused, 1);
    assert_left_parked(&s, 9, "late.txt");

    s.writers.lock().unwrap().insert(writer_of(&stranger()));
    let second = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(second.refused, 0);
    assert_eq!(
        std::fs::read(s.watch.path().join("late.txt")).unwrap(),
        BODY,
        "the first pass after the Mint lands it"
    );
}

/// A source holding no grant log answers no writer, which licenses nothing.
#[tokio::test]
async fn a_source_with_no_log_adopts_nothing() {
    let s = seat_authorizing(vec![MSEK], BTreeSet::new()).await;
    park(
        &s,
        1,
        &envelope_signed("a.txt", BODY, &Signing::By(&depositor())),
    );
    assert_eq!(s.engine.adopt_deposits().await.unwrap().refused, 1);
    assert_left_parked(&s, 1, "a.txt");
}

/// **Ever minted, revoked or not**: the writer set is every `Mint` or `Renew`
/// that ever named the writer for the folder, so an item a revoked principal
/// deposited before its grant ended still lands.
#[tokio::test]
async fn a_revoked_writers_item_still_lands() {
    use fauna_core::grant_event::{
        CLASS_DEPOSIT, GRANT_EVENT_SIGNATURE_LEN, GrantEvent, GrantEventKind, GrantEventScope,
        deposit_folder_token, writer_factor,
    };
    // The fold the reader answers from: a Mint naming the depositor for the
    // folder, then its Revoke.
    let writer = writer_of(&depositor());
    let event = |kind, at| GrantEvent {
        grant_id: vec![1; 16],
        holder: vec![2; 32],
        kind,
        scope: vec![
            GrantEventScope {
                class: CLASS_DEPOSIT.into(),
                kind: Some(deposit_folder_token(FOLDER)),
                tier: None,
            }
            .with_factor(&writer_factor(&writer)),
        ],
        window_start: 1_000,
        window_end: 2_000,
        at,
        sig: vec![0; GRANT_EVENT_SIGNATURE_LEN],
    };
    let authorized = fauna_core::grant_event::deposit_authorizations(&[
        event(GrantEventKind::Mint, 1_000),
        event(GrantEventKind::Revoke, 1_500),
    ])
    .remove(&FOLDER)
    .unwrap_or_default();
    assert!(authorized.contains(&writer), "the revoke drops nothing");

    let s = seat_authorizing(vec![MSEK], authorized).await;
    park(
        &s,
        1,
        &envelope_signed("kept.txt", BODY, &Signing::By(&depositor())),
    );
    let done = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(done.refused, 0);
    assert_eq!(
        std::fs::read(s.watch.path().join("kept.txt")).unwrap(),
        BODY
    );
}

/// A larger item signed over another body is refused once its parts are
/// staged and hashed, and its stage leaves nothing behind.
#[tokio::test]
async fn a_multi_part_item_signed_over_other_bytes_leaves_no_stage() {
    let s = seat(vec![MSEK]).await;
    let mut header = DepositEnvelope {
        name: "scan.bin".into(),
        parts: 2,
        ..Default::default()
    };
    sign(&mut header, b"not-these-bytes", &Signing::By(&depositor()));
    s.control
        .park_deposit(FOLDER, parked(6, seal_xwing(&MSEK, &encode(&header))));
    park_part(&s.control, 6, 0, 0, b"a");
    park_part(&s.control, 6, 1, 1, b"b");
    assert_eq!(s.engine.adopt_deposits().await.unwrap().refused, 1);
    assert_left_parked(&s, 6, "scan.bin");
    let leftovers: Vec<_> = std::fs::read_dir(s.watch.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().starts_with(".fauna-deposit-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
