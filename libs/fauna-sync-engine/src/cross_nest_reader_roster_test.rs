//! **The reader's cross-nest roster arm** — `mls-group-key-material.md` § M2 →
//! *Writer-signed change records*, ruling (3) as amended 2026-09-29: a
//! cross-nest member's engine reads the set's writer roster relayed to the
//! set's home nest (`fauna.folders.members.list_actors_remote`, `federation.md`
//! § Cross-nest…, *The cross-nest writer roster read*), under the same writer
//! filter as the same-nest read, with the same fail-closed posture.
//!
//! Pinned at tier_1 against a nest double on a mocked socket (the
//! `connected_arm_heal_test` / `build_engine_retired_custody_test` wiring): a
//! foreign-routed engine judges a non-owner writer's signed row HELD before any
//! successful read, VERIFIED after it, refuses a reader-access member's signed
//! row, and keeps the last roster when a later read fails. It never asks its
//! own nest's same-named set (`members.list_actors`) — that roster is another
//! set's.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::folders::{
    ActorMembersListRemoteRequest, ActorMembersListReply, FolderActorMember,
};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::ReaderBinding;
use fauna_protocol::sync_writer_sig::{ChangeSigner, SignedChange};
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::pull_remote_changes_test::test_engine_with_nest_client;

const FOLDER: &str = "holiday";
const HOME_NEST: &str = "https://home.example";
const SET_NONCE: [u8; 32] = [7; 32];

/// The nest double: the roster reply it serves (`None` ⇒ a genuine wire error,
/// the shape an old nest's `unknown_kind` or a relayed `peer_nest_outdated`
/// takes), plus a log of every kind and each relayed roster request.
#[derive(Default)]
struct NestDouble {
    kinds: Mutex<Vec<String>>,
    remote_requests: Mutex<Vec<ActorMembersListRemoteRequest>>,
    roster: Mutex<Option<ActorMembersListReply>>,
}

impl NestDouble {
    fn count(&self, kind: &str) -> usize {
        self.kinds
            .lock()
            .unwrap()
            .iter()
            .filter(|k| *k == kind)
            .count()
    }
}

pub(crate) fn value_of<T: serde::Serialize>(v: &T) -> fauna_protocol::Value {
    let bytes = fauna_core::encoding::canonical_encode(v).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

fn reply_for(
    nest: &NestDouble,
    kind: &str,
    payload: &fauna_protocol::Value,
) -> (bool, fauna_protocol::Value) {
    nest.kinds.lock().unwrap().push(kind.to_string());
    let error = |code: &str| (false, value_of(&RpcError::new(code, "error.test")));
    match kind {
        "fauna.folders.members.list_actors_remote" => {
            let bytes = fauna_core::encoding::canonical_encode(payload).unwrap();
            nest.remote_requests
                .lock()
                .unwrap()
                .push(fauna_protocol::decode_strict(&bytes).expect("a well-formed request"));
            match nest.roster.lock().unwrap().clone() {
                Some(reply) => (true, value_of(&reply)),
                None => error("fauna.folders.peer_nest_outdated"),
            }
        }
        _ => error("fauna.test.unexpected_kind"),
    }
}

/// A `NestClient` over an in-memory socket answering `nest` (the
/// `build_engine_retired_custody_test::stand_up_ws` wiring).
fn stand_up_ws(
    nest: Arc<NestDouble>,
) -> (
    Arc<NestClient>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    stand_up_ws_answering(
        ActorKeypair::from_secret([9u8; 32]),
        move |kind, payload| reply_for(&nest, kind, payload),
    )
}

/// A `NestClient` signed in as `identity` over an in-memory socket whose
/// every request `answer` replies to — `(ok, payload)` per `(kind, payload)`.
/// The wiring any tier_1 nest double in this crate stands on.
pub(crate) fn stand_up_ws_answering(
    identity: ActorKeypair,
    answer: impl Fn(&str, &fauna_protocol::Value) -> (bool, fauna_protocol::Value)
    + Send
    + Sync
    + 'static,
) -> (
    Arc<NestClient>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let auth = Arc::new(AuthClient::new("http://127.0.0.1:0".into(), identity));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, connection_state_tx) = client.supervisor_channels_for_test();
    let (adapter, mut server) = mpsc_pair();
    let server_task = tokio::spawn(async move {
        while let Some(bytes) = server.rx_from_client.recv().await {
            let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
                continue;
            };
            let (ok, payload) = answer(&req.kind, &req.payload);
            let reply = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id: req.correlation_id,
                payload,
                ok,
            });
            if server
                .tx_to_client
                .send(encode_frame(&reply).unwrap())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });
    (client, server_task, supervisor)
}

/// A row signed directly by `writer` under the set's nonce, as a nest serves it.
fn signed_row(writer: &ActorKeypair, seq: i64, path: &str) -> SyncChange {
    let mut row = SyncChange {
        seq,
        path_hash: fauna_core::hex32::encode(&fauna_core::sync::path_hash(path)),
        manifest_hash: Some(fauna_core::hex32::encode(&[3; 32])),
        size_bytes: 10,
        change_type: "create".into(),
        created_at: 1_000,
        device_id: Some(fauna_core::hex32::encode(&[4; 32])),
        author_actor_id: Some(writer.actor_id().to_hex()),
        path_sealed: Some(fauna_protocol::ByteBuf::from(vec![1, 2, 3])),
        derived_through: Some(seq - 1),
        ..Default::default()
    };
    let signer = ChangeSigner::direct(writer);
    let statement = SignedChange::for_row(&row, SET_NONCE).unwrap();
    row.signature = Some(fauna_protocol::ByteBuf::from(
        signer.sign_statement(&statement).to_vec(),
    ));
    row.signer_key = Some(fauna_protocol::ByteBuf::from(signer.signer_key().to_vec()));
    row
}

fn member(actor: &ActorKeypair, role: &str, access: Option<&str>) -> FolderActorMember {
    FolderActorMember {
        actor_id: actor.actor_id().to_hex(),
        role: role.into(),
        access: access.map(str::to_string),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_cross_nest_reader_holds_a_writers_row_until_the_relayed_roster_read_then_judges_it() {
    let (owner, writer, reader) = (
        ActorKeypair::from_secret([1; 32]),
        ActorKeypair::from_secret([2; 32]),
        ActorKeypair::from_secret([3; 32]),
    );
    let channel_hex = "cd".repeat(32);
    let nest = Arc::new(NestDouble::default());
    let (client, _server, _supervisor) = stand_up_ws(Arc::clone(&nest));
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine_with_nest_client(dir.path().to_path_buf(), FOLDER, client);
    engine.set_foreign_routing(HOME_NEST.to_string(), channel_hex.clone());
    // The binding install `engine_lifecycle` does for a foreign set: the
    // MLS-recorded owner is the pre-read seed.
    engine.set_reader_binding(ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner.actor_id().0),
        ..Default::default()
    });
    let writers_row = signed_row(&writer, 5, "a.txt");
    let readers_row = signed_row(&reader, 6, "b.txt");
    let owners_row = signed_row(&owner, 4, "c.txt");

    // 1. The relayed read fails (an old home nest): never read ⇒ the writer's
    //    row is HELD — fail closed, the row not lost — while the owner's own
    //    row BELOW it verifies off the seed.
    let (admitted, held_from) = engine
        .verify_served_rows(vec![writers_row.clone(), owners_row.clone()], &[])
        .await;
    assert_eq!(
        held_from,
        Some(5),
        "the pull stops below the unjudgeable row"
    );
    assert_eq!(
        admitted.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![4],
        "the owner's row verifies off the MLS-recorded seed"
    );
    assert_eq!(
        nest.count("fauna.folders.members.list_actors_remote"),
        1,
        "a held row triggered one relayed read"
    );

    // 2. The home nest answers: the writer's row verifies, the reader-access
    //    member's is refused (a signed row by a non-writer is not a record).
    *nest.roster.lock().unwrap() = Some(ActorMembersListReply {
        members: vec![
            member(&owner, "owner", None),
            member(&writer, "member", Some("writer")),
            member(&reader, "member", Some("reader")),
        ],
        caller_access: Some("reader".into()),
        ..Default::default()
    });
    let (admitted, held_from) = engine
        .verify_served_rows(vec![writers_row.clone(), readers_row.clone()], &[])
        .await;
    assert_eq!(held_from, None);
    assert_eq!(
        admitted.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![5],
        "the writer's row is admitted, the reader-access member's refused"
    );

    // 3. A later read fails: the last roster stands.
    *nest.roster.lock().unwrap() = None;
    engine.refresh_reader_roster().await;
    let (admitted, held_from) = engine
        .verify_served_rows(vec![writers_row, readers_row], &[])
        .await;
    assert_eq!(held_from, None);
    assert_eq!(admitted.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![5]);

    // Every read was the relay, addressed by channel to the home nest — never
    // the own nest's name-addressed `members.list_actors` (another set's roster).
    let requests = nest.remote_requests.lock().unwrap().clone();
    assert!(requests.len() >= 3, "got {requests:?}");
    assert!(
        requests
            .iter()
            .all(|r| r.channel_id == channel_hex && r.nest_url == HOME_NEST)
    );
    assert_eq!(nest.count("fauna.folders.members.list_actors"), 0);
}
