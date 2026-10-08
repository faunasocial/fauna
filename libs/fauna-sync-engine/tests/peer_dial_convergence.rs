//! Row 46 — the peer-leg DIAL pass, end to end through the pump with **no
//! nest anywhere** (`account-data-plane.md` § The peer leg: a nest outage
//! "is merely the situation in which the peer leg is the only reachable peer
//! set"): two real `AccountStoreRuntime`s on two store dirs, one account,
//! converge over dialed channels driven by their own passes — no
//! hand-assembled channels, no direct plane calls on the dialing side.
//!
//! The requester answers ONLY `fauna.nest.info` (the brake gate's evidence)
//! and fails every data leg as a transport fault — the "no nest" in the
//! module title is every leg that moves data.
//!
//! Latency-independent throughout (convention 14): passes are driven
//! explicitly (`reconcile_now` — the command round-trip is the causal
//! barrier), the one deadline poll is a named generous budget, and the
//! listener-registration barrier is `fauna_transport::testing`'s.

#![cfg(feature = "account-runtime")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::ModerationConfig;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_ENDPOINTS, KIND_GENERATION_MINT, KIND_MODERATION, home_scope_for_kind,
};
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE,
    RuntimePrincipal, StoreRoot, resolve_writer_key_serialized,
};
use fauna_sync_engine::generation_tip::GenerationTrust;
use fauna_transport::testing::{Listeners, await_listening, listeners};

mod common;
use common::{mem_factory, mem_factory_with};

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([0x5A; 32])
}

fn schedule() -> AccountStateKeySchedule {
    AccountStateKeySchedule::derive(&BackupKey::derive(root().secret_bytes()))
}

fn trust() -> GenerationTrust {
    GenerationTrust {
        root: root().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    }
}

/// A transport fault the pump absorbs — nothing reached a nest, because
/// there is none.
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

/// Answers `fauna.nest.info` (with the always-on `peer-sync` advertisement)
/// and fails everything else — the brake gate's evidence is metadata, and
/// every leg that moves data is dead.
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

fn creds_for(base: &Path, device: &str) -> CredentialStore {
    CredentialStore::with_file_backend(CRED_NAMESPACE, base.join(device).join("creds"))
}

fn store_root_for(base: &Path, device: &str) -> StoreRoot {
    StoreRoot::at(base.join(device).join("state"))
}

fn params(base: &Path, device: &str, listeners: &Listeners) -> AccountRuntimeParams<NestInfoOnly> {
    AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
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
        peer_transport: Some(mem_factory(listeners)),
        // The machine's named row — never read here (no nest answers enrollment).
        enrollment_target_device_id: "ab".repeat(32),
    }
}

/// The sibling's device-endpoints entry, staged the sanctioned door-less way
/// (`fauna_peer_sync::discovery` module docs) — production replicas learned
/// these while connectivity lasted; this test's whole premise is that it is
/// gone now.
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

/// Pre-mint a device's writer key (the sanctioned pre-assembly resolver) and
/// stage `rows` into its store — before its runtime ever starts, so the
/// staging can never race an assembly.
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

const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// Deadline-poll a probe — a generous budget a green run pays one tick of
/// (convention 14), never a settle-sleep.
async fn eventually<F, Fut>(what: &str, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if tokio::time::timeout(CONVERGENCE_BUDGET, async {
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

async fn moderation_of(handle: &AccountStoreHandle) -> Option<Vec<String>> {
    handle
        .get_preference(KIND_MODERATION)
        .await
        .ok()
        .flatten()
        .and_then(|row| canonical_decode::<ModerationConfig>(&row.value).ok())
        .map(|m| {
            m.muted_keywords
                .into_iter()
                .map(|k| k.keyword)
                .collect::<Vec<_>>()
        })
}

/// **The slice's own success sentence**: two runtimes, two store dirs, one
/// account, no reachable nest — a preference written on A becomes readable
/// on B over the DIALED leg, driven entirely through the pump. B's only
/// route to the value is dial → admit → pull-only walk: its nest legs all
/// fail, and nothing in the test touches a plane or channel by hand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preference_write_converges_over_the_dialed_leg_with_no_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    // A stages nothing; B learns A's NodeId the way production replicas do —
    // from a device-endpoints entry already in its store.
    let a_pub = premint_and_stage(&base, "a", &[]).await;
    let _b_pub = premint_and_stage(&base, "b", &member_rows(&base, "a")).await;

    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("start A");
    // The blob leg fails (no nest) — the plane row is durable first, which is
    // exactly the offline-mutation posture; the error is the nest leg's.
    let value = canonical_encode(&ModerationConfig {
        muted_keywords: vec!["over-the-dialed-leg".into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    let _ = a.put_preference(KIND_MODERATION, value).await;
    assert_eq!(
        moderation_of(&a).await.as_deref(),
        Some(&["over-the-dialed-leg".to_string()][..]),
        "A's row is durable locally despite the dead nest"
    );
    // A's prologue (holder from birth) binds; barrier on the registration.
    await_listening(&net, &a_pub).await;

    let b = AccountStoreRuntime::start(params(&base, "b", &net))
        .await
        .expect("start B");
    eventually("B converges A's preference over the dialed leg", || {
        let b = b.clone();
        async move {
            let report = b.reconcile_now().await.expect("B pass");
            let dialed = report
                .peer_dial
                .map(|d| d.targets >= 1 && d.admitted >= 1)
                .unwrap_or(false);
            dialed
                && moderation_of(&b).await.as_deref()
                    == Some(&["over-the-dialed-leg".to_string()][..])
        }
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

/// **The custody attach, red-verifiable** (the charter's :3700 sequencing —
/// "the peer leg's custody attaches when its pull plane first runs in
/// production"; the security review's watch): a `Shredded` mint
/// merged over the DIALED leg drops the retained generation key from the
/// dialing device's T10 slot, exactly as one merged over the nest leg does.
/// Removing `with_generation_custody` from the dial plane construction reds
/// this test and nothing else — the mutation run at the landing.
///
/// **Only an authored shred drops** (`account-data-taxonomy.md`
/// § *Fleet-scope reclamation* → *the authored shred*): G1's
/// shred is sig-less — what any `BackupKey` holder can write — and staged
/// first, G2's is signed by A, a verified member, and staged after it, so
/// the walk merges G1's before G2's. B ends holding exactly G1's key: an
/// unauthored drop would leave none, a detached custody both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shredded_mint_over_the_dialed_leg_drops_the_retained_key() {
    use fauna_core::generation::{
        EscrowTargetRecord, GenerationMintRecord, derive_escrow_xwing_keypair,
    };
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;

    let tmp = tempfile::tempdir().unwrap();
    let base: PathBuf = tmp.path().to_path_buf();
    let net = listeners();

    // Build a real mint (over A alone), then its SHREDDED phase — the
    // absorbing state whose observation must drop the key everywhere.
    let a_root = store_root_for(&base, "a");
    let a_creds = creds_for(&base, "a");
    let a_key = resolve_writer_key_serialized(&a_root, &root().actor_id_hex(), &a_creds)
        .expect("A writer key");
    let a_pub = a_key.verifying_key().to_bytes();
    let escrow = EscrowTargetRecord {
        xwing_escrow_pubkey: derive_escrow_xwing_keypair(&[0x66; 32])
            .public
            .to_bytes()
            .to_vec(),
    };
    let member = fauna_core::generation::FleetMember {
        device_id: a_pub,
        xwing_pubkey: fauna_core::generation::derive_device_xwing_keypair(&a_key.to_bytes())
            .public
            .to_bytes()
            .to_vec(),
        enrolled_at_ms: 5_000,
    };
    let core_of = |record: &GenerationMintRecord| match record {
        GenerationMintRecord::Minted { core, .. } => core.clone(),
        GenerationMintRecord::Shredded { core, .. } => core.clone(),
    };
    let unauthored = build_mint(
        std::slice::from_ref(&member),
        &escrow,
        "identity",
        Vec::new(),
        &a_key,
        7_000,
    )
    .expect("mint G1");
    let authored =
        build_mint(&[member], &escrow, "identity", Vec::new(), &a_key, 7_500).expect("mint G2");
    let shreds = [
        (
            unauthored.generation_id,
            GenerationMintRecord::Shredded {
                core: core_of(&unauthored.record),
                shredded_at_ms: 9_000,
                shredded_by: a_pub,
                shredder_sig: vec![],
            },
        ),
        (
            authored.generation_id,
            fauna_core::generation::sign_shred(&a_key, core_of(&authored.record), 9_000)
                .expect("sign"),
        ),
    ];

    // Stage the shredded row on A through a pull-only plane put — journal +
    // relay plane both fed, so A can SERVE it (door-less put_state feeds
    // neither; the sanctioned peer_sync_over_quic staging shape).
    {
        let dir = a_root.store_dir(&root().actor_id_hex()).expect("store dir");
        let store = AccountStore::open(
            SqliteBackend::open(&dir).unwrap(),
            &root().actor_id_hex(),
            WriterId(a_pub),
        )
        .await
        .unwrap();
        let sched = schedule();
        let tr = trust();
        let plane = fauna_sync_engine::account_state_plane::AccountStatePlane::new_pull_only(
            &store,
            &NestInfoOnly,
            &sched,
            &a_key,
            &tr,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        // In order: G1's unauthored shred journals before G2's authored one.
        for (generation_id, shredded) in &shreds {
            plane
                .put(
                    &fauna_sync_engine::account_state_plane::ItemId {
                        kind: KIND_GENERATION_MINT.into(),
                        key: fauna_core::hex32::encode(generation_id),
                    },
                    canonical_encode(shredded).unwrap().to_vec(),
                    None,
                )
                .await
                .expect("stage the shredded mint on A");
        }
    }

    // B holds G's key in its retained bundle (the W5 (account-data-plane.md § Workstreams).4a carriage) and knows
    // A's NodeId — the state of a device that legitimately opened G before
    // the shred happened elsewhere.
    let b_root = store_root_for(&base, "b");
    let b_creds = creds_for(&base, "b");
    let b_key = resolve_writer_key_serialized(&b_root, &root().actor_id_hex(), &b_creds)
        .expect("B writer key");
    let b_pub = b_key.verifying_key().to_bytes();
    {
        let dir = b_root.store_dir(&root().actor_id_hex()).expect("store dir");
        let slot = fauna_sync_engine::principal_bundle::PrincipalSlot::resolve(
            std::sync::Arc::new(creds_for(&base, "b")),
            root().actor_id_hex(),
            dir.clone(),
            &b_pub,
            None,
        );
        slot.record_generation_key(&unauthored.generation_id, &unauthored.gen_key);
        slot.record_generation_key(&authored.generation_id, &authored.gen_key);
        assert_eq!(
            slot.status().retained_generations,
            2,
            "B retains G1 and G2 before the dial"
        );
        let store = AccountStore::open(
            SqliteBackend::open(&dir).unwrap(),
            &root().actor_id_hex(),
            WriterId(b_pub),
        )
        .await
        .unwrap();
        for row in member_rows(&base, "a") {
            store.put_state(row).await.unwrap();
        }
    }

    let a = AccountStoreRuntime::start(params(&base, "a", &net))
        .await
        .expect("start A");
    await_listening(&net, &a_pub).await;
    let b = AccountStoreRuntime::start(params(&base, "b", &net))
        .await
        .expect("start B");

    eventually(
        "the authored shred drops B's retained G2 key and the unauthored one keeps G1's",
        || {
            let b = b.clone();
            async move {
                let _ = b.reconcile_now().await.expect("B pass");
                b.principal_bundle_status()
                    .await
                    .expect("status")
                    .retained_generations
                    == 1
            }
        },
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

// ── The same-account peer data plane: file bodies over the dialed leg ───────

/// A host's file-sync hooks for the leg: `serves` is the one body this host's
/// "engines" hold (folder `"7"`), and the registry is what its dial pass
/// fills.
fn file_sync_serving(
    serves: Option<(fauna_core::data::ContentHash, Vec<u8>)>,
) -> (
    fauna_sync_engine::account_runtime::PeerFileSync,
    std::sync::Arc<fauna_sync_engine::sibling_chunks::SiblingChannels>,
) {
    let siblings = fauna_sync_engine::sibling_chunks::SiblingChannels::new();
    let file_chunks: fauna_peer_sync::FileChunkFn =
        std::sync::Arc::new(move |folder: String, key: [u8; 32]| {
            let hit = serves
                .as_ref()
                .filter(|(k, _)| folder == "7" && k.digest() == key)
                .map(|(_, body)| body.clone());
            Box::pin(async move { hit })
        });
    (
        fauna_sync_engine::account_runtime::PeerFileSync {
            file_chunks,
            siblings: std::sync::Arc::clone(&siblings),
        },
        siblings,
    )
}

fn params_with_files(
    base: &Path,
    device: &str,
    listeners: &Listeners,
    file_sync: fauna_sync_engine::account_runtime::PeerFileSync,
) -> AccountRuntimeParams<NestInfoOnly> {
    AccountRuntimeParams {
        peer_transport: Some(mem_factory_with(listeners, Some(file_sync))),
        ..params(base, device, listeners)
    }
}

/// Start A serving `body` (under `key`) and B dialing it; return once B's
/// dial pass has admitted A into B's sibling registry.
async fn two_seats_one_body(
    base: &Path,
    net: &Listeners,
    key: fauna_core::data::ContentHash,
    body: Vec<u8>,
) -> (
    AccountStoreHandle,
    AccountStoreHandle,
    std::sync::Arc<fauna_sync_engine::sibling_chunks::SiblingChannels>,
) {
    let a_pub = premint_and_stage(base, "a", &[]).await;
    let _b_pub = premint_and_stage(base, "b", &member_rows(base, "a")).await;
    let (a_files, _a_siblings) = file_sync_serving(Some((key, body)));
    let (b_files, b_siblings) = file_sync_serving(None);
    let a = AccountStoreRuntime::start(params_with_files(base, "a", net, a_files))
        .await
        .expect("start A");
    await_listening(net, &a_pub).await;
    let b = AccountStoreRuntime::start(params_with_files(base, "b", net, b_files))
        .await
        .expect("start B");
    eventually("B's dial pass admits A into its sibling registry", || {
        let b = b.clone();
        let reg = std::sync::Arc::clone(&b_siblings);
        async move {
            let _ = b.reconcile_now().await.expect("B pass");
            reg.len() == 1
        }
    })
    .await;
    (a, b, b_siblings)
}

/// **Slice 1's success sentence, one tier down** (`p2p.md` § Goal, promises 1
/// and 3): with no nest anywhere, a file body one device holds reaches the
/// other device over the dialed, mutually admitted leg — through the
/// production bind (A's serve door) and the production dial pass (B's
/// registry) — and a chunk the sibling does not hold is handed back for the
/// nest, never failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_body_crosses_the_dialed_leg_and_the_rest_is_left_to_the_nest() {
    use fauna_sync_engine::sibling_source::SiblingChunkSource;

    let tmp = tempfile::tempdir().unwrap();
    let net = listeners();
    // Bigger than one reply, so the ranged pull runs more than one round.
    let body: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
    let key = fauna_core::data::ContentHash::of_raw(&body);
    let gone = fauna_core::data::ContentHash::of_raw(b"a chunk only the nest holds");
    let (a, b, siblings) = two_seats_one_body(tmp.path(), &net, key, body.clone()).await;

    let got = siblings
        .fetch("7", &[key, gone, key], "holiday/video")
        .await;
    assert_eq!(got.len(), 1, "only the held chunk arrives");
    assert_eq!(got[&key], body);
    let tally = siblings.tally();
    assert_eq!((tally.chunks, tally.to_nest), (1, 1), "{tally:?}");
    assert_eq!(tally.bytes, body.len() as u64);

    // Another folder's want is not served from this one, however the key reads.
    assert!(siblings.fetch("8", &[key], "elsewhere").await.is_empty());

    a.shutdown().await;
    b.shutdown().await;
}

/// Rule 4 on the file plane: a sibling serving bytes that do not hash to the
/// key it was asked for has nothing kept, and is dropped from the registry —
/// the nest serves the chunk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forged_body_is_refused_and_its_sibling_dropped() {
    use fauna_sync_engine::sibling_source::SiblingChunkSource;

    let tmp = tempfile::tempdir().unwrap();
    let net = listeners();
    let key = fauna_core::data::ContentHash::of_raw(b"the real chunk");
    let (a, b, siblings) =
        two_seats_one_body(tmp.path(), &net, key, b"something else entirely".to_vec()).await;

    assert!(siblings.fetch("7", &[key], "f").await.is_empty());
    assert_eq!(
        siblings.len(),
        0,
        "a sibling that served a forgery is dropped"
    );
    assert_eq!(siblings.tally().to_nest, 1);

    a.shutdown().await;
    b.shutdown().await;
}
