//! Third-party deposit adoption (`docs/goal/behavior/file-sync.md`
//! § Third-party deposit ingress): a parked item, sealed to the owner's
//! recipient key, lands in the sync root through the write door, is recorded
//! as an ordinary change and retired once durable — idempotently on the
//! deposit id.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::{ContentHash, FoldersConfig};
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretArray32;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::folders::{DepositEnvelope, ParkedDeposit};
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

/// The account's mail custody, as the account store answers it.
struct MailCustody(Vec<[u8; 32]>);

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for MailCustody {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        Ok(FoldersConfig::default())
    }
    async fn recipient_mseks(&self) -> anyhow::Result<Vec<SecretArray32>> {
        Ok(self.0.iter().map(|m| (*m).into()).collect())
    }
}

fn envelope(name: &str, body: &[u8]) -> Vec<u8> {
    fauna_protocol::encode_canonical(&DepositEnvelope {
        name: name.into(),
        content_type: "text/plain".into(),
        body: body.to_vec().into(),
        extra: Default::default(),
    })
    .unwrap()
    .to_vec()
}

/// Sealed as the nest's door seals it: X-Wing to the owner's standing key.
fn sealed_hybrid(msek: &[u8; 32], name: &str, body: &[u8]) -> Vec<u8> {
    let kp = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(msek);
    fauna_mls::wrapped_blob::seal_to_recipient_xwing(&envelope(name, body), &kp.public)
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
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
    _server: MockServer,
}

async fn seat(mseks: Vec<[u8; 32]>) -> Seat {
    let server = MockServer::start().await;
    MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf());
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    engine.set_deposit_inbox(FOLDER, Arc::new(MailCustody(mseks)));
    Seat {
        engine,
        watch,
        control,
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

    // The record landed (as the watcher's own upload, or a later pass's).
    s.engine
        .mark_hydrated("note.txt", ContentHash::of_raw(BODY))
        .unwrap();
    let second = s.engine.adopt_deposits().await.unwrap();
    assert_eq!(
        second,
        DepositAdoption {
            already_landed: 1,
            ..Default::default()
        }
    );
    assert!(s.control.parked_ids(FOLDER).is_empty());
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
