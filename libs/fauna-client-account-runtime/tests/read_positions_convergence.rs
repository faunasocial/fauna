//! **Two devices, end to end in shared Rust** — the read-state proof
//! `docs/goal/behavior/conversation-read-state.md` § The proofs it ships with
//! names: manager A reads a native thread → the read-position seam → store A
//! → a walk → store B → the seam → manager B reports the thread read.
//!
//! Two real `AccountStoreRuntime`s of one account on two store dirs, joined by
//! the peer leg over the in-memory transport (no nest anywhere — the
//! `fauna-sync-engine` `peer_dial_convergence` shape), each with a real
//! `ConversationsManager` whose seam is this crate's production registration
//! (`read_positions::register`). Nothing in the test touches a plane, a
//! marker or a position by hand: A's read reaches B's badge through the
//! production writer task, the store's monotone door, B's own pump pass and
//! B's watcher.
//!
//! Latency-independent (convention 14): B's passes are driven explicitly
//! (`reconcile_now`), and the waits are deadline polls on state with one
//! named generous budget a green run pays a tick of.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_client_account_runtime::read_positions;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
use fauna_conversations::store::history::ChannelHistorySlice;
use fauna_conversations::thread::{ThreadFlavor, ThreadId};
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::merge_policy::{KIND_DEVICE_ENDPOINTS, home_scope_for_kind};
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE,
    CloudBackupExclusion, PeerLegBinding, PeerLegFactoryInputs, PeerTransportFactory,
    RuntimePrincipal, StoreRoot, resolve_writer_key_serialized,
};
use fauna_transport::testing::{Listeners, MemTransport, await_listening, listeners};

const CHANNEL: &str = "5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead5ead";

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([0x7E; 32])
}

// ── No nest: the peer leg is the only route between the two stores ──────────

#[derive(Debug)]
struct NoNest(&'static str);

impl std::fmt::Display for NoNest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no nest in this test: {}", self.0)
    }
}

impl RpcErrorClass for NoNest {
    fn is_rejection(&self) -> bool {
        false
    }
}

/// Answers `fauna.nest.info` (the peer leg's brake evidence) and fails every
/// leg that moves data.
#[derive(Clone)]
struct NestInfoOnly;

impl RpcRequester for NestInfoOnly {
    type Error = NoNest;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.nest.info" {
            let reply = NestInfoReply {
                capabilities: vec!["peer-sync".to_string()],
                ..Default::default()
            };
            return Ok(decode_strict(&encode_canonical(&reply).expect("encode"))
                .expect("node-info reply decodes"));
        }
        Err(NoNest(kind))
    }
}

impl fauna_protocol::KeyedRpcRequester for NestInfoOnly {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

/// The in-memory peer transport factory — the sibling of
/// `fauna-sync-engine`'s `tests/common::mem_factory`, which that crate's own
/// integration tests cannot share without a feature switch on every run.
fn mem_factory(listeners: &Listeners) -> PeerTransportFactory {
    let listeners = Listeners::clone(listeners);
    Arc::new(move |inputs: PeerLegFactoryInputs| {
        let listeners = Listeners::clone(&listeners);
        Box::pin(async move {
            Ok(PeerLegBinding {
                transport: Arc::new(MemTransport {
                    me: fauna_transport::EndpointKey::from_bytes(
                        inputs.writer_key.verifying_key().to_bytes(),
                    ),
                    listeners,
                }),
                bound_addrs: vec!["203.0.113.9:4711".parse().unwrap()],
            })
        })
    })
}

fn creds_for(base: &Path, device: &str) -> CredentialStore {
    CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(device).join("creds"))
}

fn store_root_for(base: &Path, device: &str) -> StoreRoot {
    StoreRoot::at(base.join(device).join("state"))
}

fn params(base: &Path, device: &str, net: &Listeners) -> AccountRuntimeParams<NestInfoOnly> {
    AccountRuntimeParams {
        store_backup_exclusion: CloudBackupExclusion::NotApplicable {
            platform: "test".into(),
        },
        store_root: store_root_for(base, device),
        actor_id_hex: root().actor_id_hex(),
        rpc: NestInfoOnly,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(root().into()),
        credentials: creds_for(base, device),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: Some(mem_factory(net)),
        // The machine's named row — never read here (no nest answers enrollment).
        enrollment_target_device_id: "ab".repeat(32),
    }
}

/// Pre-mint `device`'s writer key and stage `rows` into its store before its
/// runtime starts — how B learns A's NodeId (a device-endpoints entry a
/// production replica learned while a nest was reachable).
async fn premint_and_stage(base: &Path, device: &str, rows: &[StateEntry]) -> [u8; 32] {
    let store_root = store_root_for(base, device);
    let creds = creds_for(base, device);
    let writer_key = resolve_writer_key_serialized(&store_root, &root().actor_id_hex(), &creds)
        .expect("pre-mint writer key");
    let writer_pub = writer_key.verifying_key().to_bytes();
    if !rows.is_empty() {
        let dir = store_root
            .store_dir(&root().actor_id_hex())
            .expect("store dir");
        let store = AccountStore::open(
            SqliteBackend::open(&dir).unwrap(),
            &root().actor_id_hex(),
            WriterId(writer_pub),
        )
        .await
        .unwrap();
        for row in rows {
            store.put_state(row.clone()).await.unwrap();
        }
    }
    writer_pub
}

fn endpoints_row(node_id: [u8; 32]) -> StateEntry {
    let value = DeviceEndpoints {
        node_id,
        lan_addrs: Vec::new(),
        public_addrs: Vec::new(),
        relay_url: None,
    };
    StateEntry {
        kind: KIND_DEVICE_ENDPOINTS.into(),
        key: fauna_core::hex32::encode(&node_id),
        scope: home_scope_for_kind(KIND_DEVICE_ENDPOINTS).unwrap().into(),
        value: canonical_encode(&value).unwrap(),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    }
}

/// `device`'s rows as a sibling holds them: its verified enrollment beside
/// its device-endpoints entry — an entry is a dial candidate only while its
/// device is a verified fleet member (`account-sync-plane.md` § The peer leg
/// → *Discovery*). `device`'s writer key must already be pre-minted.
fn member_rows(base: &Path, device: &str) -> Vec<StateEntry> {
    let key = resolve_writer_key_serialized(
        &store_root_for(base, device),
        &root().actor_id_hex(),
        &creds_for(base, device),
    )
    .expect("the pre-minted writer key");
    let id = key.verifying_key().to_bytes();
    let cert = fauna_core::data::DeviceAuthorization {
        actor_id: root().actor_id(),
        device_key: id,
        capabilities: vec![fauna_core::data::Capability::RenewBearer],
        created_at: fauna_core::data::Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(&root(), &cert).unwrap();
    let authorization =
        canonical_encode(&fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env)).unwrap();
    let kind = fauna_protocol::merge_policy::KIND_DEVICE_SET;
    let enrollment = StateEntry {
        kind: kind.into(),
        key: fauna_core::hex32::encode(&id),
        scope: home_scope_for_kind(kind).unwrap().into(),
        value: canonical_encode(&fauna_core::generation::sign_device_enrollment(
            &key,
            authorization,
            5_000,
        ))
        .unwrap(),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    };
    vec![enrollment, endpoints_row(id)]
}

// ── The conversations side ───────────────────────────────────────────────────

/// A peer's message at channel `seq`, stamped long before either manager's
/// launch floor — so only a channel position can ever make it unread.
fn peer_message(seq: u64) -> MessageSnapshot {
    MessageSnapshot {
        message_id: MessageId(format!("conv:{CHANNEL}:{seq}")),
        sender: TypedAddress::Email {
            email_address: "peer@host.test".into(),
        },
        sender_display: String::new(),
        body: format!("message {seq}"),
        document: fauna_core::render::RenderDocument::default(),
        timestamp_ms: seq as i64,
        subject_line: None,
        badges: MessageBadges::default(),
        reply_to: None,
        reactions: vec![],
        deleted: false,
        is_own: false,
        legal_takedown_ref: None,
        labels: vec![],
        plane_ref: None,
        can_delete: false,
    }
}

/// A device's manager holding the channel's first three messages — restored
/// the way a launch restores `history/<ch>`.
fn device_manager() -> (Arc<ConversationsManager>, ThreadId) {
    let m = ConversationsManager::new();
    let thread = m.restore_channel_slice(&ChannelHistorySlice {
        channel_id_hex: CHANNEL.to_string(),
        label: "peer".to_string(),
        flavor: ThreadFlavor::OneToOne,
        participants: vec![],
        messages: (1..=3).map(peer_message).collect(),
        ..Default::default()
    });
    (m, thread)
}

fn unread(m: &ConversationsManager, thread: &ThreadId) -> u32 {
    m.snapshot()
        .threads
        .iter()
        .find(|t| &t.thread_id == thread)
        .expect("the channel's thread")
        .unread_count
}

const BUDGET: Duration = Duration::from_secs(60);

/// Deadline-poll a probe — one generous budget, never a settle-sleep.
async fn eventually<F, Fut>(what: &str, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if tokio::time::timeout(BUDGET, async {
        while !probe().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        panic!("eventually({what}): not reached within budget");
    }
}

async fn markers(store: &AccountStoreHandle) -> Vec<(String, u64)> {
    store.read_markers().await.unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_read_on_one_device_is_read_on_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    let a_pub = premint_and_stage(&base, "a", &[]).await;
    premint_and_stage(&base, "b", &member_rows(&base, "a")).await;
    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("start A");
    await_listening(&net, &a_pub).await;
    let b = AccountStoreRuntime::start(params(&base, "b", &net))
        .await
        .expect("start B");

    let (manager_a, thread_a) = device_manager();
    let (manager_b, thread_b) = device_manager();
    assert_eq!(
        unread(&manager_b, &thread_b),
        0,
        "position unknown: the floor"
    );

    // The account-store-ready edge on both devices.
    let rt = tokio::runtime::Handle::current();
    read_positions::register(&manager_a, a.clone(), &rt);
    read_positions::register(&manager_b, b.clone(), &rt);
    // No marker anywhere yet: once each seam has delivered, the channel is
    // unread from its first message on both (the declared transition).
    eventually("both seams delivered their (empty) positions", || {
        let ready = unread(&manager_a, &thread_a) == 3 && unread(&manager_b, &thread_b) == 3;
        async move { ready }
    })
    .await;

    // A opens the thread: read in memory at once, raised through the seam.
    manager_a.select_thread(thread_a.clone());
    assert_eq!(unread(&manager_a, &thread_a), 0);
    eventually("A's store holds the marker", || {
        let a = a.clone();
        async move { markers(&a).await == vec![(CHANNEL.to_string(), 3)] }
    })
    .await;

    // B's own passes dial A and walk its rows; B's watcher re-reads after each.
    eventually("B's manager reports the thread read", || {
        let b = b.clone();
        let manager_b = Arc::clone(&manager_b);
        let thread_b = thread_b.clone();
        async move {
            let _ = b.reconcile_now().await;
            unread(&manager_b, &thread_b) == 0
        }
    })
    .await;
    assert_eq!(markers(&b).await, vec![(CHANNEL.to_string(), 3)]);

    // A later arrival above the position is news on B, whatever its stamp.
    manager_b.restore_channel_slice(&ChannelHistorySlice {
        channel_id_hex: CHANNEL.to_string(),
        messages: vec![peer_message(4)],
        ..Default::default()
    });
    assert_eq!(unread(&manager_b, &thread_b), 1);

    a.shutdown().await;
    b.shutdown().await;
}
