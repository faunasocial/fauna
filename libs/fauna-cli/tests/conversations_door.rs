//! tier_1: the conversations tools' half of the oracle's door
//! (`conversations-oracle.md` § The custodian and the door) against a test
//! door on a socket of its own — the possession proof opened under the
//! consent's holder key, each catalog operation crossing as its door's own,
//! every refusal relayed verbatim, and the one sentence for a door nothing
//! serves.
#![cfg(all(feature = "device", unix))]

use std::sync::{Arc, Mutex};

use fauna_cli::device::HolderKey;
use fauna_cli::door::{DoorIdentity, perform_at};
use fauna_cli::tools::ConversationOp;
use fauna_ipc::oracle::{
    Challenge, ChallengeOutcome, DoorFuture, Hello, OracleDoor, OracleEndpoint, OracleMessage,
    OracleMessagePage, OracleOp, OracleRefusal, OracleReply, OracleRoom, OracleSent, PendingProof,
};
use serde_json::json;

const ACTOR: [u8; 32] = [0xA1; 32];
const PRINCIPAL: [u8; 16] = [0x5E; 16];

/// A door that admits one principal — the holder key it seals to — and
/// answers each operation with a fixed projection, keeping what it was asked.
struct TestDoor {
    holder: [u8; 32],
    asked: Mutex<Vec<OracleOp>>,
}

impl OracleDoor for TestDoor {
    type Principal = ();

    fn challenge<'a>(&'a self, hello: &'a Hello) -> DoorFuture<'a, ChallengeOutcome<()>> {
        Box::pin(async move {
            if hello.actor_id != ACTOR {
                return Err(OracleRefusal::WrongAccount);
            }
            if hello.principal_id != PRINCIPAL {
                return Err(OracleRefusal::UnknownPrincipal);
            }
            let mut pending = PendingProof {
                nonce: Default::default(),
                principal: (),
            };
            pending.nonce.copy_from_slice(&[7u8; 32]);
            let binding = fauna_client_conversations_oracle::proof_binding(&ACTOR, &PRINCIPAL);
            let (enc, ciphertext) = fauna_mls::wrapped_blob::envelope::hpke_seal(
                &self.holder,
                &binding,
                &binding,
                &pending.nonce[..],
            )
            .expect("seal");
            Ok((Challenge::Sealed { enc, ciphertext }, pending))
        })
    }

    fn perform<'a>(
        &'a self,
        _principal: &'a (),
        op: OracleOp,
    ) -> DoorFuture<'a, Result<OracleReply, OracleRefusal>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(op.clone());
            match op {
                OracleOp::ListRooms => Ok(OracleReply::Rooms(vec![OracleRoom {
                    room_id: "room-1".into(),
                    title: "Family".into(),
                    class: "end-to-end".into(),
                    members: vec!["Alice".into()],
                    last_activity_ms: 5,
                    unread: 1,
                    can_send: true,
                    can_search_room: false,
                }])),
                OracleOp::ReadRoom { room_id, .. } => Ok(OracleReply::Page(OracleMessagePage {
                    room_id,
                    messages: vec![OracleMessage {
                        position: 0,
                        message_id: "m0".into(),
                        sender: "alice".into(),
                        sender_display: "Alice".into(),
                        body: "hello".into(),
                        timestamp_ms: 4,
                        is_own: false,
                    }],
                    earlier: None,
                })),
                OracleOp::Search { .. } => Err(OracleRefusal::NotGranted {
                    class: "conversation.read".into(),
                }),
                OracleOp::Send { room_id, .. } => Ok(OracleReply::Sent(OracleSent {
                    room_id,
                    message_id: "m1".into(),
                    position: 1,
                })),
            }
        })
    }
}

fn socket_path(name: &str) -> std::path::PathBuf {
    // Short and fixed: a unix socket path has a hard length budget.
    std::path::PathBuf::from(format!(
        "/tmp/fauna-cli-door-{}-{name}/oracle.sock",
        std::process::id()
    ))
}

/// Serve `door` at a socket of this test's own; the endpoint and the
/// shutdown switch.
async fn serve(
    name: &str,
    door: Arc<TestDoor>,
) -> (OracleEndpoint, tokio::sync::watch::Sender<bool>) {
    let path = socket_path(name);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let at = path.clone();
    tokio::spawn(async move { fauna_ipc::oracle::serve_unix(&at, door, stopped).await });
    for _ in 0..200 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (OracleEndpoint::Unix(path), stop)
}

async fn run(
    endpoint: &OracleEndpoint,
    holder: Arc<HolderKey>,
    op: ConversationOp,
) -> Result<serde_json::Value, String> {
    let endpoint = endpoint.clone();
    tokio::task::spawn_blocking(move || {
        let identity = DoorIdentity {
            actor_id: ACTOR,
            principal_id: PRINCIPAL.to_vec(),
            holder,
        };
        perform_at(&endpoint, &identity, op).map_err(|e| e.text())
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tool_crosses_as_its_doors_operation_and_a_refusal_is_relayed_verbatim() {
    let holder = Arc::new(HolderKey::generate());
    let door = Arc::new(TestDoor {
        holder: holder.public(),
        asked: Mutex::default(),
    });
    let (endpoint, stop) = serve("ops", door.clone()).await;

    let rooms = run(&endpoint, holder.clone(), ConversationOp::ListRooms)
        .await
        .unwrap();
    assert_eq!(rooms["rooms"][0]["room_id"], "room-1");
    assert_eq!(rooms["rooms"][0]["class"], "end-to-end");

    let page = run(
        &endpoint,
        holder.clone(),
        ConversationOp::ReadRoom {
            room_id: "room-1".into(),
            before: Some(9),
        },
    )
    .await
    .unwrap();
    assert_eq!(page["messages"][0]["body"], "hello");
    assert_eq!(page["earlier"], json!(null));

    let sent = run(
        &endpoint,
        holder.clone(),
        ConversationOp::Send {
            room_id: "room-1".into(),
            body: "on my way".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(sent["position"], 1);

    let refused = run(
        &endpoint,
        holder.clone(),
        ConversationOp::Search {
            query: "trip".into(),
            room_id: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused,
        OracleRefusal::NotGranted {
            class: "conversation.read".into()
        }
        .to_string()
    );

    assert_eq!(
        *door.asked.lock().unwrap(),
        [
            OracleOp::ListRooms,
            OracleOp::ReadRoom {
                room_id: "room-1".into(),
                before: Some(9)
            },
            OracleOp::Send {
                room_id: "room-1".into(),
                body: "on my way".into()
            },
            OracleOp::Search {
                query: "trip".into(),
                room_id: None
            },
        ]
    );
    stop.send(true).unwrap();
}

/// A key the consent did not attest opens nothing: the door performs nothing.
#[tokio::test(flavor = "multi_thread")]
async fn another_holder_key_cannot_enter() {
    let door = Arc::new(TestDoor {
        holder: HolderKey::generate().public(),
        asked: Mutex::default(),
    });
    let (endpoint, stop) = serve("key", door.clone()).await;
    let refused = run(
        &endpoint,
        Arc::new(HolderKey::generate()),
        ConversationOp::ListRooms,
    )
    .await
    .unwrap_err();
    assert_eq!(refused, "this app could not open the Fauna app's challenge");
    assert!(door.asked.lock().unwrap().is_empty());
    stop.send(true).unwrap();
}

/// Nothing serving the door is the one sentence the model relays.
#[tokio::test(flavor = "multi_thread")]
async fn no_custodian_is_one_sentence_the_model_can_relay() {
    let endpoint = OracleEndpoint::Unix(socket_path("none"));
    let refused = run(
        &endpoint,
        Arc::new(HolderKey::generate()),
        ConversationOp::ListRooms,
    )
    .await
    .unwrap_err();
    assert_eq!(
        refused,
        "open Fauna on this device to let this app reach your conversations"
    );
}
