//! **The engine stamps its row on every per-set request it sends**
//! (`docs/goal/behavior/set-addressing.md` § The row form → *Whoever holds
//! the row sends it*, § Tests → *Engine unit*).
//!
//! A user who owns a set and holds a membership in another of the same name
//! has two rows one name cannot tell apart, and the nest's by-name search finds
//! the own set first. So an engine built `with_folder_ref(Local(id))` stamps
//! `folder_id: Some(id)` on what it sends, and the nest resolves that row
//! alone; an engine built without a ref sends the name forms alone; a
//! `Foreign` ref stamps nothing and keeps the channel form, which is a
//! cross-nest set's address (a foreign set has no row on this nest).
//!
//! The record and the list ride the control-plane `NestClient`, so they are
//! captured off a mocked socket (the shape of `offline_placeholder_delete_test`);
//! the conflict report rides the control API and is pinned beside its siblings
//! in `catchup_failed_report_test`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::folder_keys::FolderRef;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::sync::{
    SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply, SyncChangesListRequest,
};
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::causal::CausalStamp;
use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::test_engine_with_nest_client;

const FOLDER: &str = "photos";
const CHANNEL: [u8; 32] = [0x5c; 32];

/// Every request frame the double saw, as `(kind, payload)`.
type Captured = Arc<Mutex<Vec<(String, fauna_protocol::Value)>>>;

fn value_of<T: serde::Serialize>(v: &T) -> fauna_protocol::Value {
    let bytes = fauna_core::encoding::canonical_encode(v).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

fn typed<T: serde::de::DeserializeOwned>(v: &fauna_protocol::Value) -> T {
    let bytes = fauna_core::encoding::canonical_encode(v).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

/// Captures every request, answers the record and the list genuinely and
/// refuses anything else (so an unexpected kind is legible, never a hang).
async fn serve(mut server: fauna_client::testing::ServerSide, captured: Captured) {
    while let Some(bytes) = server.rx_from_client.recv().await {
        let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
            continue;
        };
        captured
            .lock()
            .unwrap()
            .push((req.kind.clone(), req.payload.clone()));
        let (ok, payload) = match req.kind.as_str() {
            "fauna.sync.changes.record" => (
                true,
                value_of(&SyncChangeRecordReply {
                    seq: 1,
                    extra: Default::default(),
                }),
            ),
            "fauna.sync.changes.list" => (
                true,
                value_of(&SyncChangesListReply {
                    changes: vec![],
                    ..Default::default()
                }),
            ),
            _ => (
                false,
                value_of(&RpcError::new(
                    "fauna.test.unexpected_kind",
                    "error.test.unexpected_kind",
                )),
            ),
        };
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
}

/// An engine on [`FOLDER`] over a real `NestClient` + supervisor whose socket
/// is the capturing double, bound to `row` the way the lifecycle binds it; a
/// `Foreign` row is a cross-nest set, so it is routed to its home nest too.
/// Records one change and lists once, then hands back what went on the wire.
async fn sent_by_an_engine_bound_to(
    row: Option<FolderRef>,
) -> Vec<(String, fauna_protocol::Value)> {
    let dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([9u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, state_tx) = client.supervisor_channels_for_test();
    let (adapter, server) = mpsc_pair();
    let captured: Captured = Arc::default();
    let server_task = tokio::spawn(serve(server, Arc::clone(&captured)));
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });
    let mut engine: SyncEngine =
        test_engine_with_nest_client(dir.path().to_path_buf(), FOLDER, client);
    if let Some(row) = row {
        if let FolderRef::Foreign(channel) = row {
            engine.set_foreign_routing("https://home.example".into(), hex::encode(channel));
        }
        engine = engine.with_folder_ref(row);
    }
    assert_eq!(engine.folder_row(), row, "the accessor reads the bound row");

    tokio::time::timeout(
        Duration::from_secs(10),
        engine.record_change(
            FOLDER,
            "a.jpg",
            None,
            0,
            "delete",
            None,
            None,
            CausalStamp::unknown(),
        ),
    )
    .await
    .expect("the record must not hang against a serving double")
    .expect("record_change");
    tokio::time::timeout(
        Duration::from_secs(10),
        engine.fetch_change_batch(FOLDER, 0, false),
    )
    .await
    .expect("the list must not hang against a serving double")
    .expect("fetch_change_batch");

    supervisor.abort();
    server_task.abort();
    captured.lock().unwrap().clone()
}

/// The record's and the list's `(folder_id, channel_id)`, off the wire.
fn addresses(sent: &[(String, fauna_protocol::Value)]) -> Vec<(&str, Option<i64>, Option<String>)> {
    sent.iter()
        .filter_map(|(kind, payload)| match kind.as_str() {
            "fauna.sync.changes.record" => {
                let r: SyncChangeRecordRequest = typed(payload);
                Some(("record", r.folder_id, r.channel_id))
            }
            "fauna.sync.changes.list" => {
                let r: SyncChangesListRequest = typed(payload);
                Some(("list", r.folder_id, r.channel_id))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_engine_bound_to_a_local_row_stamps_it_on_every_per_set_request() {
    let sent = sent_by_an_engine_bound_to(Some(FolderRef::Local(9))).await;
    assert_eq!(
        addresses(&sent),
        vec![("record", Some(9), None), ("list", Some(9), None)],
        "every per-set request carries the bound row; sent {sent:?}"
    );
}

#[tokio::test]
async fn an_engine_holding_no_row_sends_the_name_forms_alone() {
    let sent = sent_by_an_engine_bound_to(None).await;
    assert_eq!(
        addresses(&sent),
        vec![("record", None, None), ("list", None, None)],
        "sent {sent:?}"
    );
}

#[tokio::test]
async fn a_foreign_ref_stamps_no_row_and_keeps_the_channel_form() {
    let sent = sent_by_an_engine_bound_to(Some(FolderRef::Foreign(CHANNEL))).await;
    let channel = Some(hex::encode(CHANNEL));
    assert_eq!(
        addresses(&sent),
        vec![
            ("record", None, channel.clone()),
            ("list", None, channel.clone())
        ],
        "a cross-nest set's address is its channel; sent {sent:?}"
    );
}
