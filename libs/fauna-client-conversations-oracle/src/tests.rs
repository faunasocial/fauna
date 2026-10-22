//! tier_1: the custodian's authorize, the door's handshake end to end over a
//! real socket, the endpoint lock's hand-over, and the two classes performed
//! through a real manager (`conversations-oracle.md` § Done definition).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_conversations::address::{Rail, TypedAddress};
use fauna_conversations::backends::mock::MockRailBackend;
use fauna_core::identity_op::OPS_PER_WINDOW;
use fauna_ipc::oracle::{ConnectError, OracleClient, OracleEndpoint};

use super::*;

const ACTOR: [u8; 32] = [0x42; 32];
const PRINCIPAL: &[u8] = b"principal-1";

#[derive(Default)]
struct FakeAccount {
    holders: Mutex<HashMap<Vec<u8>, [u8; 32]>>,
    grants: Mutex<HashMap<[u8; 32], Vec<LiveTuple>>>,
    windows: Mutex<HashMap<Vec<u8>, RateWindow>>,
    records: Mutex<Vec<(Vec<u8>, RecordedPrincipalOp)>>,
    now: AtomicU64,
}

impl FakeAccount {
    fn with(holder: [u8; 32], classes: &[IdentityOpClass]) -> Self {
        let account = Self::default();
        account.now.store(1_000_000, Ordering::SeqCst);
        account
            .holders
            .lock()
            .unwrap()
            .insert(PRINCIPAL.to_vec(), holder);
        account.grant(holder, classes);
        account
    }

    fn grant(&self, holder: [u8; 32], classes: &[IdentityOpClass]) {
        self.grants.lock().unwrap().insert(
            holder,
            classes
                .iter()
                .map(|c| LiveTuple {
                    class: fauna_core::identity_op::CLASS.to_string(),
                    kind: Some(c.name().to_string()),
                })
                .collect(),
        );
    }

    fn records(&self) -> Vec<(String, String)> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .map(|(_, op)| (op.class.clone(), op.detail.clone()))
            .collect()
    }
}

impl CustodianAccount for FakeAccount {
    fn actor_id(&self) -> [u8; 32] {
        ACTOR
    }
    fn principal_holder<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, Option<[u8; 32]>> {
        let holder = self.holders.lock().unwrap().get(principal_id).copied();
        Box::pin(async move { holder })
    }
    fn live_tuples<'a>(
        &'a self,
        holder: &'a [u8; 32],
        _now: u64,
    ) -> DoorFuture<'a, Vec<LiveTuple>> {
        let tuples = self
            .grants
            .lock()
            .unwrap()
            .get(holder)
            .cloned()
            .unwrap_or_default();
        Box::pin(async move { tuples })
    }
    fn rate_window<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, RateWindow> {
        let window = self
            .windows
            .lock()
            .unwrap()
            .get(principal_id)
            .copied()
            .unwrap_or_default();
        Box::pin(async move { window })
    }
    fn store_rate_window<'a>(
        &'a self,
        principal_id: &'a [u8],
        window: RateWindow,
    ) -> DoorFuture<'a, ()> {
        self.windows
            .lock()
            .unwrap()
            .insert(principal_id.to_vec(), window);
        Box::pin(async {})
    }
    fn record<'a>(&'a self, principal_id: &'a [u8], op: RecordedPrincipalOp) -> DoorFuture<'a, ()> {
        self.records
            .lock()
            .unwrap()
            .push((principal_id.to_vec(), op));
        Box::pin(async {})
    }
    fn search_index<'a>(&'a self, _query: &'a str, _limit: u32) -> DoorFuture<'a, Vec<IndexHit>> {
        Box::pin(async { Vec::new() })
    }
    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

fn principal(holder: [u8; 32]) -> AdmittedPrincipal {
    AdmittedPrincipal {
        principal_id: PRINCIPAL.to_vec(),
        holder,
    }
}

fn custodian(
    account: FakeAccount,
) -> (
    ConversationsCustodian<FakeAccount>,
    Arc<ConversationsManager>,
) {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    (
        ConversationsCustodian::new(account, manager.clone()),
        manager,
    )
}

#[tokio::test]
async fn authorize_refuses_an_unknown_principal_a_wrong_class_and_a_revoked_grant() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (c, _) = custodian(FakeAccount::with(
        holder,
        &[IdentityOpClass::ConversationRead],
    ));

    // Unknown: another principal id, or the row's key moved under it.
    let stranger = AdmittedPrincipal {
        principal_id: b"someone-else".to_vec(),
        holder,
    };
    assert_eq!(
        c.authorize(&stranger, IdentityOpClass::ConversationRead)
            .await,
        Err(OracleRefusal::UnknownPrincipal)
    );
    assert_eq!(
        c.authorize(&principal([9; 32]), IdentityOpClass::ConversationRead)
            .await,
        Err(OracleRefusal::UnknownPrincipal)
    );

    // Wrong class: read granted, send asked.
    assert_eq!(
        c.authorize(&principal(holder), IdentityOpClass::ConversationRead)
            .await,
        Ok(())
    );
    assert_eq!(
        c.authorize(&principal(holder), IdentityOpClass::ConversationSend)
            .await,
        Err(OracleRefusal::NotGranted {
            class: "conversation.send".into()
        })
    );
    // A nest custodian's class is never this door's, granted or not.
    c.account().grant(holder, &[IdentityOpClass::PostAuthor]);
    assert!(matches!(
        c.authorize(&principal(holder), IdentityOpClass::ConversationRead)
            .await,
        Err(OracleRefusal::NotGranted { .. })
    ));

    // Revoked: the log holds nothing live for the key — the very next
    // operation is refused, nothing cached.
    c.account().grant(holder, &[]);
    assert!(matches!(
        c.authorize(&principal(holder), IdentityOpClass::ConversationRead)
            .await,
        Err(OracleRefusal::NotGranted { .. })
    ));
}

#[tokio::test]
async fn the_ceiling_binds_per_principal_and_a_refused_operation_is_not_recorded() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (c, _) = custodian(FakeAccount::with(
        holder,
        &[IdentityOpClass::ConversationRead],
    ));
    for _ in 0..OPS_PER_WINDOW {
        c.perform_op(&principal(holder), OracleOp::ListRooms)
            .await
            .expect("inside the ceiling");
    }
    let refused = c
        .perform_op(&principal(holder), OracleOp::ListRooms)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, OracleRefusal::CeilingReached { retry_after_secs } if retry_after_secs > 0),
        "{refused:?}"
    );
    assert_eq!(c.account().records().len(), OPS_PER_WINDOW as usize);
    // The window rolls.
    c.account().now.fetch_add(WINDOW_SECS, Ordering::SeqCst);
    assert!(
        c.perform_op(&principal(holder), OracleOp::ListRooms)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn the_two_classes_read_and_send_through_the_page_without_touching_the_draft() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (c, manager) = custodian(FakeAccount::with(
        holder,
        &[
            IdentityOpClass::ConversationRead,
            IdentityOpClass::ConversationSend,
        ],
    ));
    let room = manager.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    manager.set_compose_body(room.clone(), "the user's own half sentence".into());

    let OracleReply::Rooms(rooms) = c
        .perform_op(&principal(holder), OracleOp::ListRooms)
        .await
        .unwrap()
    else {
        panic!("rooms")
    };
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].room_id, room.0);
    assert_eq!(rooms[0].class, "end-to-end");

    let OracleReply::Sent(sent) = c
        .perform_op(
            &principal(holder),
            OracleOp::Send {
                room_id: room.0.clone(),
                body: "on my way".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("sent")
    };
    let OracleReply::Page(page) = c
        .perform_op(
            &principal(holder),
            OracleOp::ReadRoom {
                room_id: room.0.clone(),
                before: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("page")
    };
    let mine = page
        .messages
        .iter()
        .find(|m| m.message_id == sent.message_id)
        .expect("the sent message reads back");
    assert_eq!(
        (mine.position, mine.body.as_str(), mine.is_own),
        (sent.position, "on my way", true)
    );
    assert_eq!(page.earlier, None);
    assert_eq!(
        manager
            .thread_detail(room.clone())
            .unwrap()
            .compose
            .body_draft,
        "the user's own half sentence"
    );

    // An unknown room is the typed refusal; a blank body is refused.
    assert_eq!(
        c.perform_op(
            &principal(holder),
            OracleOp::ReadRoom {
                room_id: "nope".into(),
                before: None
            }
        )
        .await,
        Err(OracleRefusal::NoSuchRoom)
    );
    assert!(matches!(
        c.perform_op(
            &principal(holder),
            OracleOp::Send {
                room_id: room.0.clone(),
                body: " ".into()
            }
        )
        .await,
        Err(OracleRefusal::Refused { .. })
    ));

    // Every admitted operation is recorded with its class and room.
    let records = c.account().records();
    assert_eq!(
        records[0],
        ("conversation.read".into(), ALL_ROOMS_DETAIL.into())
    );
    assert_eq!(records[1], ("conversation.send".into(), room.0.clone()));
    assert_eq!(records[2], ("conversation.read".into(), room.0.clone()));
}

/// The door end to end: the principal proves its holder key through the real
/// HPKE seal; a wrong account and a principal that cannot open the challenge
/// are refused before any operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handshake_proves_the_holder_key_and_refuses_everything_else() {
    let (secret, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (wrong_secret, _) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (c, _) = custodian(FakeAccount::with(
        holder,
        &[IdentityOpClass::ConversationRead],
    ));
    let custodian = Arc::new(c);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oracle-test.sock");
    let mut keeper = serve::DoorKeeper::at(path.clone(), custodian.clone());
    assert!(keeper.tick(true).await);
    while !path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let endpoint = OracleEndpoint::Unix(path);

    let outcome = tokio::task::spawn_blocking(move || {
        let open = |secret: [u8; 32]| {
            move |enc: &[u8], ct: &[u8]| open_challenge(&secret, &ACTOR, PRINCIPAL, enc, ct)
        };
        let wrong_account =
            OracleClient::connect(&endpoint, PRINCIPAL, &[1; 32], open(secret)).err();
        let unknown = OracleClient::connect(&endpoint, b"nobody", &ACTOR, open(secret)).err();
        let cannot_open =
            OracleClient::connect(&endpoint, PRINCIPAL, &ACTOR, open(wrong_secret)).err();
        let forged = OracleClient::connect(&endpoint, PRINCIPAL, &ACTOR, |_, _| {
            Some(zeroize::Zeroizing::new(vec![0; ORACLE_NONCE_LEN]))
        })
        .err();
        let mut client = OracleClient::connect(&endpoint, PRINCIPAL, &ACTOR, open(secret)).unwrap();
        let listed = client.call(OracleOp::ListRooms).unwrap();
        let send = client
            .call(OracleOp::Send {
                room_id: "r".into(),
                body: "b".into(),
            })
            .unwrap();
        (wrong_account, unknown, cannot_open, forged, listed, send)
    })
    .await
    .unwrap();
    assert!(matches!(
        outcome.0,
        Some(ConnectError::Refused(OracleRefusal::WrongAccount))
    ));
    assert!(matches!(
        outcome.1,
        Some(ConnectError::Refused(OracleRefusal::UnknownPrincipal))
    ));
    assert!(matches!(outcome.2, Some(ConnectError::CannotOpen)));
    assert!(matches!(
        outcome.3,
        Some(ConnectError::Refused(OracleRefusal::ProofFailed))
    ));
    assert_eq!(outcome.4, Ok(OracleReply::Rooms(vec![])));
    assert_eq!(
        outcome.5,
        Err(OracleRefusal::NotGranted {
            class: "conversation.send".into()
        })
    );
    assert!(!keeper.tick(false).await);
}

/// One app serves an account's door at a time; the next takes it over when
/// the first stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_endpoint_lock_hands_the_door_over() {
    let (_, holder) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let (first, _) = custodian(FakeAccount::with(holder, &[]));
    let (second, _) = custodian(FakeAccount::with(holder, &[]));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oracle-handover.sock");
    let mut a = serve::DoorKeeper::at(path.clone(), Arc::new(first));
    let mut b = serve::DoorKeeper::at(path.clone(), Arc::new(second));

    assert!(a.tick(true).await, "first come serves");
    assert!(!b.tick(true).await, "the second waits");
    assert!(!a.tick(false).await, "signing out gives the door up");
    assert!(
        b.tick(true).await,
        "the second takes it over on its next tick"
    );
    assert!(!a.tick(true).await, "and the first now waits");
    assert!(!b.tick(false).await);
}
