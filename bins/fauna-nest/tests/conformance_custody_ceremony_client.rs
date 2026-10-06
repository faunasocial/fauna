//! **The custody ceremony, end to end over the real nest** (tier_3; W8.4 (account-data-plane.md § Workstreams) —
//! `docs/goal/architecture/account-data-plane.md` § Replica posture → *The
//! custody grant + ceremony*): two accounts on one box run
//! offer → accept → mint → deliver through the PRODUCTION stack, over a real
//! MLS conversation channel.
//!
//! The flow this file drives, every link production code:
//!
//!   Alice (owner) messages Bob (host) — the shipped same-nest DM bootstrap
//!   (`FaunaMlsBackend::send` over `NestConversationsRpc`: KP fetch, group
//!   create, Welcome deliver, channel send) → `begin_offer` records the
//!   ceremony in Alice's ceremony state (`fauna.state.custody-ceremony`) → `drive_ceremonies` posts
//!   the offer as a `ChannelMessageBody::Custody` application message → Bob's
//!   inbound poll hands it to the production `StoreCustodyCeremonySink`, which
//!   verifies + durably captures it → Bob consents (`build_accept`, binding
//!   his serving device key) → his drive posts the accept → Alice's poll
//!   captures it → her drive runs the interactive mint door (signed Mint
//!   event on her grant log, keyless blob released against the STORED log,
//!   deposited over the real `fauna.capabilities.mint`), signs the witness,
//!   posts the deliver, and writes her `fauna.state.custodian-endpoints` row
//!   through the REAL R14 (account-data-plane.md § The ratified decisions) writer door under a resolved generation → Bob's
//!   poll captures the deliver (the witness verifies against his accepted
//!   key) → his drive writes his `fauna.state.custodies-held` row through
//!   his own door.
//!
//! End state asserted: the nest holds the keyless capability row; Alice's
//! grant log holds the custody-shaped Mint event; Alice's SECOND device
//! walks the fleet feed and reads the custodian-endpoints row (the
//! convergence-under-a-real-generation proof the W8.3 registration
//! deliberately deferred to this file); Bob's store holds the custodies-held
//! row whose witness verifies for the accepted device and whose budget is
//! the accept's; and not one ceremony payload ever rendered as a chat
//! bubble. Both drives settle to a zero report — the record-then-act loop
//! has nothing owed.
//!
//! Every assertion is on latency-independent state (e2e convention 14): the
//! polls and drives are explicit calls, nothing sleeps.

mod common;
use common::connected_client;
use common::one_to_one_thread;
use common::welcome_bytes_from_inbox;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
use fauna_client::NestClient;
use fauna_client_capabilities::custody_ceremony::CustodyHostingDepositor;
use fauna_client_capabilities::custody_ceremony::{
    CeremonyRecords, CustodyPayloadPoster, CustodyRegistryWriter, DriveReport, OfferParams,
    begin_offer, build_accept, drive_ceremonies,
};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_config::test_helpers::FakeCustodyCeremonyStore;
use fauna_client_conversations::{NestConversationsRpc, StoreCustodyCeremonySink};
use fauna_conversations::Rail;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::backend::{ConversationsRpc, RailBackend, ResolveResult};
use fauna_conversations::backends::fauna_mls::{FaunaMlsBackend, poll_inbound_conv};
use fauna_conversations::compose::ComposeState;
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::message::{BodyFormat, MessageBadges, MessageId};
use fauna_conversations::thread::ThreadId;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP;
use fauna_core::custody_grant::{CustodyScopeSet, custody_entry_key, verify_custody_witness};
use fauna_core::data::Timestamp;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::generation::{
    FleetMember, derive_device_xwing_keypair, escrow_target_record, sign_escrow_receipt,
};
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_mls::wrapped_blob::{GrantBlob, ScopeTuple, generation_wraps::build_mint};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ItemClass};
use fauna_protocol::merge_policy::{
    KIND_CUSTODIAN_ENDPOINTS, KIND_CUSTODIES_HELD, KIND_DEVICE_SET, KIND_ESCROW_RECEIPT,
    KIND_ESCROW_TARGET, KIND_GENERATION_MINT,
};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;

const ALICE_SEED: [u8; 32] = [0xA0; 32];
const BOB_SEED: [u8; 32] = [0xB0; 32];
/// Alice's two device signing secrets (device id = the verifying key).
const ALICE_DEV_1: u8 = 0xA1;
const ALICE_DEV_2: u8 = 0xA2;
/// Bob's serving device — his plane writer AND the custodian key the accept
/// binds (= its peer-plane NodeId).
const BOB_DEV: u8 = 0xB1;
const GRANT_ID: [u8; 16] = [0x1D; 16];
const FAR_FUTURE: u64 = u64::MAX / 2;

fn signing_key(device: u8) -> SigningKey {
    SigningKey::from_bytes(&[device; 32])
}

fn device_id(device: u8) -> [u8; 32] {
    signing_key(device).verifying_key().to_bytes()
}

/// The account's escrow holder — holder-generic (the walk rig's shape): any
/// Ed25519 key signs a verifying receipt; the account's trust is what makes
/// this one count.
fn escrow_holder() -> SigningKey {
    SigningKey::from_bytes(&[0xE5; 32])
}

/// One account's real-door fixtures: the per-device stores, the shared key
/// schedule, and the R14 trust (root = the account identity — production
/// shape, unlike the walk rig's separate fleet root).
struct AccountRig {
    keypair: ActorKeypair,
    seed: [u8; 32],
    schedule: AccountStateKeySchedule,
    trust: GenerationTrust,
}

impl AccountRig {
    fn new(seed: [u8; 32]) -> Self {
        let keypair = ActorKeypair::from_secret(seed);
        Self {
            trust: GenerationTrust {
                root: keypair.actor_id(),
                prior: Vec::new(),
                trusted_holders: vec![escrow_holder().verifying_key().to_bytes()].into(),
            },
            schedule: AccountStateKeySchedule::derive(&BackupKey::derive(&seed)),
            keypair,
            seed,
        }
    }

    async fn store(&self, device: u8) -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            &hex::encode(self.keypair.actor_id().0),
            WriterId(device_id(device)),
        )
        .await
        .unwrap()
    }

    /// This seat's identity's escrow-target key.
    fn target_key(&self) -> String {
        fauna_core::generation::escrow_target_identity_key(&self.keypair.actor_id())
    }

    fn fleet_plane<'a>(
        &'a self,
        store: &'a AccountStore<SqliteBackend>,
        rpc: &'a Arc<NestClient>,
        device: &'a SigningKey,
    ) -> AccountStatePlane<'a, SqliteBackend, Arc<NestClient>> {
        AccountStatePlane::new(
            store,
            rpc,
            &self.schedule,
            device,
            &self.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
    }

    /// A device's real enrollment through the door: root-signed cert + the
    /// X-Wing pubkey production derives from the device secret.
    fn enrollment_value(&self, device: u8) -> Vec<u8> {
        use fauna_core::data::{Capability, DeviceAuthorization};
        let cert = DeviceAuthorization {
            actor_id: self.keypair.actor_id(),
            device_key: device_id(device),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(&self.keypair, &cert).unwrap();
        let authorization = fauna_core::encoding::canonical_encode(
            &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
        )
        .unwrap();
        fauna_core::encoding::canonical_encode(&fauna_core::generation::sign_device_enrollment(
            &SigningKey::from_bytes(&[device; 32]),
            authorization,
            5_000,
        ))
        .unwrap()
        .to_vec()
    }

    async fn enroll_through_door(
        &self,
        store: &AccountStore<SqliteBackend>,
        rpc: &Arc<NestClient>,
        device: u8,
    ) {
        self.fleet_plane(store, rpc, &signing_key(device))
            .put(
                &ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key: hex::encode(device_id(device)),
                },
                self.enrollment_value(device),
                None,
            )
            .await
            .unwrap();
    }

    /// One completed mint, holder-generically receipted, machinery rows
    /// through the door (the walk rig's step-6 shape) — after this a
    /// `GenerationTip` kind seals for real.
    async fn mint_through_door(
        &self,
        store: &AccountStore<SqliteBackend>,
        rpc: &Arc<NestClient>,
        minter: u8,
        members: &[u8],
    ) {
        let fleet: Vec<FleetMember> = members
            .iter()
            .map(|&d| FleetMember {
                device_id: device_id(d),
                xwing_pubkey: derive_device_xwing_keypair(&[d; 32])
                    .public
                    .to_bytes()
                    .to_vec(),
                enrolled_at_ms: 5_000,
            })
            .collect();
        let target = escrow_target_record(&self.seed);
        let built = build_mint(
            &fleet,
            &target,
            &self.target_key(),
            Vec::new(),
            &signing_key(minter),
            6_000,
        )
        .unwrap();
        let receipt = sign_escrow_receipt(
            &escrow_holder(),
            built.generation_id,
            blake3::hash(&built.escrow_wrap).into(),
            &self.target_key(),
            6_500,
        );
        let key = signing_key(minter);
        let p = self.fleet_plane(store, rpc, &key);
        p.put(
            &ItemId {
                kind: KIND_ESCROW_TARGET.into(),
                key: self.target_key(),
            },
            fauna_core::encoding::canonical_encode(&target)
                .unwrap()
                .to_vec(),
            None,
        )
        .await
        .unwrap();
        p.put(
            &ItemId {
                kind: KIND_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(&built.generation_id),
            },
            fauna_core::encoding::canonical_encode(&built.record)
                .unwrap()
                .to_vec(),
            None,
        )
        .await
        .unwrap();
        p.put(
            &ItemId {
                kind: KIND_ESCROW_RECEIPT.into(),
                key: format!(
                    "{}/{}",
                    fauna_core::hex32::encode(&built.generation_id),
                    fauna_core::hex32::encode(&receipt.holder_id)
                ),
            },
            fauna_core::encoding::canonical_encode(&receipt)
                .unwrap()
                .to_vec(),
            None,
        )
        .await
        .unwrap();
    }
}

/// The ceremony driver's registry seam over a REAL fleet plane — the same
/// `custody_rows` door puts the production `AccountStoreHandle` impl calls,
/// minus the store thread (the rig owns its stores directly). The door puts
/// are local writes; the runtime's publish step ships them, and with no
/// runtime here the rig runs that step's fleet leg itself, right after.
struct PlaneWriter<'a> {
    plane: AccountStatePlane<'a, SqliteBackend, Arc<NestClient>>,
    device: [u8; 32],
}

impl PlaneWriter<'_> {
    async fn published(&self, put: anyhow::Result<u64>) -> bool {
        put.is_ok() && self.plane.publish_pending().await.is_ok()
    }
}

impl CustodyRegistryWriter for PlaneWriter<'_> {
    async fn put_custodian_endpoints(
        &self,
        value: &fauna_core::custodian_endpoints::CustodianEndpoints,
    ) -> bool {
        self.published(
            fauna_sync_engine::custody_rows::put_custodian_endpoints(
                &self.plane,
                self.device,
                value,
            )
            .await,
        )
        .await
    }
    async fn put_custodies_held(&self, value: &fauna_core::custodies_held::CustodyHeld) -> bool {
        self.published(
            fauna_sync_engine::custody_rows::put_custodies_held(&self.plane, self.device, value)
                .await,
        )
        .await
    }
    /// The rig removes no device; the list's own pins live in the shared
    /// crates (`fauna-core`, `fauna-peer-sync`, `fauna-client-capabilities`).
    async fn removed_device_ids(&self) -> Vec<[u8; 32]> {
        Vec::new()
    }
}

/// The post door over the production backend's no-Commit custody post.
struct BackendPoster(Arc<FaunaMlsBackend>);

impl CustodyPayloadPoster for BackendPoster {
    async fn post(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        self.0
            .send_custody_payload(channel_hex, bytes)
            .await
            .is_ok()
    }
    async fn post_receipt(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        self.0
            .send_custody_receipt(channel_hex, bytes)
            .await
            .is_ok()
    }
}

async fn start_ceremony_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
    );

    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            // The DM carriage (channel register/send/fetch, KP fetch, welcome
            // deliver), the account-state feed (the registry rows' door), the
            // account-plane state (the ceremony state + grant log), and the
            // capability verbs (the keyless deposit).
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, tmp)
}

/// Device-form ceremonies never reach the hosting seam; the driver still
/// wants one.
struct NoHosting;
impl CustodyHostingDepositor for NoHosting {
    async fn register(
        &self,
        _d: &fauna_client_capabilities::custody_hosting::HostingDeposit,
    ) -> bool {
        false
    }
}

fn fauna_addr(handle: &str, actor: fauna_core::identity::ActorId) -> TypedAddress {
    TypedAddress::Fauna {
        handle: handle.into(),
        actor_id: actor,
    }
}

#[tokio::test]
async fn the_custody_ceremony_runs_end_to_end_over_the_real_nest() {
    // Failures must diagnose themselves (convention 6): the nest runs
    // in-process, so its refusal warns (`fauna_protocol::error` logs the
    // code + log-only details) are this test's only view of WHICH kind
    // refused — a client-side error has already collapsed to localized text.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let (base, state, _tmp) = start_ceremony_nest().await;
    let authority = base.trim_start_matches("http://").to_string();

    // ── The two accounts, registered ─────────────────────────────────────────
    let alice_rig = AccountRig::new(ALICE_SEED);
    let bob_rig = AccountRig::new(BOB_SEED);
    let (alice_id, bob_id) = (alice_rig.keypair.actor_id(), bob_rig.keypair.actor_id());
    // `create_user_with_handle` — the handle row is what the same-nest
    // `fauna.actor.by_handle` probe resolves the DM addressee from.
    state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    // The DM reach policy would knock a stranger's Welcome; the ceremony's
    // premise is an ESTABLISHED conversation, so an open inbox is honest
    // fixture setup, not a bypass (the shipped default for adult accounts).
    state.db.set_inbox_mode(&bob_id.0, "open").await.unwrap();

    // Bob's KPs on the nest, so Alice's DM bootstrap can fetch one.
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(BOB_SEED)).unwrap());
    for (i, pkg) in bob_engine
        .generate_key_packages_bytes(2)
        .unwrap()
        .iter()
        .enumerate()
    {
        state
            .db
            .put_key_package(&format!("bob-kp-{i}"), &bob_id.0, pkg, 0, FAR_FUTURE)
            .await
            .unwrap();
    }

    // ── Alice's production conversations stack + the DM bootstrap ────────────
    let alice_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(ALICE_SEED)).unwrap());
    let alice_nest = connected_client(&base, ActorKeypair::from_secret(ALICE_SEED)).await;
    let alice_backend = Arc::new(FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&alice_nest))) as Arc<dyn ConversationsRpc>,
        format!("alice@{authority}"),
        alice_id,
    ));
    let alice_manager = ConversationsManager::new();
    alice_manager.register_backend(alice_backend.clone());

    let chip = match alice_backend
        .resolve_address(&format!("bob@{authority}"))
        .await
    {
        ResolveResult::Resolved(addr) => addr,
        other => panic!("expected Resolved, got {other:?}"),
    };
    let thread = one_to_one_thread(ThreadId("t-custody".into()), chip);
    let compose = ComposeState {
        body_draft: "custody?".into(),
        ..Default::default()
    };
    alice_backend
        .send(&thread, &compose, &[])
        .await
        .expect("DM bootstrap send (KP fetch, group create, Welcome deliver, channel send)");
    let channel = alice_backend.bound_channels()[0];
    let channel_hex = channel.to_string();

    // ── Bob joins from the delivered Welcome; his production receive side ────
    let inbox = state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one Welcome for Bob");
    let bob_channel = bob_engine
        .join_from_welcome_bytes(&welcome_bytes_from_inbox(&inbox[0].1))
        .expect("Bob joins the DM");
    assert_eq!(bob_channel, channel);

    let bob_nest = connected_client(&base, ActorKeypair::from_secret(BOB_SEED)).await;
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(Arc::clone(&bob_nest))) as Arc<dyn ConversationsRpc>,
        format!("bob@{authority}"),
        bob_id,
    ));
    let bob_manager = ConversationsManager::new();
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(fauna_conversations::backend::RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr(&format!("alice@{authority}"), alice_id),
            recipients: vec![fauna_addr(&format!("bob@{authority}"), bob_id)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup-custody".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel);

    // The PRODUCTION sinks, both sides.
    // Each account's ceremony state (`fauna.state.custody-ceremony` — the
    // account store's in production, joined per record exactly as the shared
    // fake joins): the sink captures into it and the drive reads and marks it.
    let alice_updater = FakeCustodyCeremonyStore::empty();
    let bob_updater = FakeCustodyCeremonyStore::empty();
    alice_backend.set_custody_ceremony_sink(Arc::new(StoreCustodyCeremonySink::new(
        alice_updater.clone(),
        alice_id,
    )));
    bob_backend.set_custody_ceremony_sink(Arc::new(StoreCustodyCeremonySink::new(
        bob_updater.clone(),
        bob_id,
    )));

    // ── Alice's real-door fixtures: two enrolled devices, one generation ─────
    // (Her custodian-endpoints row is GenerationTip-sealed — the put resolves
    // a real, escrow-receipted tip; device 2 is the convergence witness.)
    let alice_store_1 = alice_rig.store(ALICE_DEV_1).await;
    let alice_store_2 = alice_rig.store(ALICE_DEV_2).await;
    alice_rig
        .enroll_through_door(&alice_store_1, &alice_nest, ALICE_DEV_1)
        .await;
    alice_rig
        .enroll_through_door(&alice_store_2, &alice_nest, ALICE_DEV_2)
        .await;
    alice_rig
        .fleet_plane(&alice_store_1, &alice_nest, &signing_key(ALICE_DEV_1))
        .walk()
        .await
        .unwrap();
    alice_rig
        .mint_through_door(
            &alice_store_1,
            &alice_nest,
            ALICE_DEV_1,
            &[ALICE_DEV_1, ALICE_DEV_2],
        )
        .await;

    // ── The ceremony ─────────────────────────────────────────────────────────
    let now = Timestamp::now();
    // The owner's grant log (the succession ledger — the account store's in
    // production); the drive's mint leg records its Mint event here.
    let alice_ledger = fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
        ActorKeypair::from_secret(ALICE_SEED).actor_id(),
    );
    let custodian_key = device_id(BOB_DEV);

    // Step 1 — Alice offers the Account form over the established channel.
    alice_updater
        .update(|cfg| {
            begin_offer(
                cfg,
                &alice_rig.keypair,
                OfferParams {
                    host: bob_id,
                    channel_hex: channel_hex.clone(),
                    scopes: CustodyScopeSet::Account,
                    duration_secs: fauna_client_capabilities::DEFAULT_GRANT_WINDOW_SECS,
                    owner_devices: vec![DeviceEndpoints {
                        node_id: device_id(ALICE_DEV_1),
                        lan_addrs: vec!["192.168.1.7:4433".into()],
                        public_addrs: Vec::new(),
                        relay_url: None,
                    }],
                    owner_nest_url: Some(base.clone()),
                    grant_id: GRANT_ID,
                },
                now,
            )
        })
        .await
        .expect("offer recorded")
        .expect("offer built");

    let alice_poster = BackendPoster(alice_backend.clone());
    let alice_depositor = CapabilitiesClient::new(Arc::clone(&alice_nest));
    let alice_dev_key = signing_key(ALICE_DEV_1);
    let alice_writer = PlaneWriter {
        plane: alice_rig.fleet_plane(&alice_store_1, &alice_nest, &alice_dev_key),
        device: device_id(ALICE_DEV_1),
    };
    let report = drive_ceremonies(
        &alice_updater,
        &alice_ledger,
        &alice_rig.keypair,
        &alice_poster,
        &alice_writer,
        &alice_depositor,
        &NoHosting,
        now,
    )
    .await
    .expect("alice drive 1");
    assert_eq!(report.posted, 1, "the offer went out: {report:?}");
    assert_eq!(report.minted, 0);

    // Step 2 — Bob's poll captures the offer through the production sink…
    let mut bob_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel, &mut bob_seq, 0)
        .await
        .expect("bob poll")
        .ingested;
    assert_eq!(
        ingested, 1,
        "Alice's chat message — the offer is never a bubble"
    );
    let counts = bob_backend.custody_payload_counts();
    assert_eq!((counts.seen, counts.captured), (1, 1), "{counts:?}");

    // …and he consents, binding his serving device + his budget.
    let bob_ledger = fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
        ActorKeypair::from_secret(BOB_SEED).actor_id(),
    );
    bob_updater
        .update(|cfg| {
            build_accept(
                cfg,
                &bob_rig.keypair,
                &GRANT_ID,
                custodian_key,
                DeviceEndpoints {
                    node_id: custodian_key,
                    lan_addrs: Vec::new(),
                    public_addrs: vec!["198.51.100.7:4433".into()],
                    relay_url: None,
                },
                DEFAULT_RETAINED_BYTES_CAP / 2,
                None,
                now,
            )
        })
        .await
        .expect("accept recorded")
        .expect("accept built");

    let bob_poster = BackendPoster(bob_backend.clone());
    let bob_depositor = CapabilitiesClient::new(Arc::clone(&bob_nest));
    let bob_store = bob_rig.store(BOB_DEV).await;
    let bob_dev_key = signing_key(BOB_DEV);
    let bob_writer = PlaneWriter {
        plane: bob_rig.fleet_plane(&bob_store, &bob_nest, &bob_dev_key),
        device: custodian_key,
    };
    let report = drive_ceremonies(
        &bob_updater,
        &bob_ledger,
        &bob_rig.keypair,
        &bob_poster,
        &bob_writer,
        &bob_depositor,
        &NoHosting,
        now,
    )
    .await
    .expect("bob drive 1");
    assert_eq!(report.posted, 1, "the accept went out: {report:?}");

    // Step 3 — Alice's poll captures the accept; her drive mints (real CAS +
    // real deposit), posts the deliver, and writes her registry row through
    // the real door under the resolved generation.
    let mut alice_seq = 0i64;
    poll_inbound_conv(&alice_backend, &alice_manager, &channel, &mut alice_seq, 0)
        .await
        .expect("alice poll");
    let counts = alice_backend.custody_payload_counts();
    assert_eq!((counts.seen, counts.captured), (1, 1), "{counts:?}");

    let report = drive_ceremonies(
        &alice_updater,
        &alice_ledger,
        &alice_rig.keypair,
        &alice_poster,
        &alice_writer,
        &alice_depositor,
        &NoHosting,
        now,
    )
    .await
    .expect("alice drive 2");
    assert_eq!(report.minted, 1, "{report:?}");
    assert_eq!(report.posted, 1, "the deliver went out: {report:?}");
    assert_eq!(
        report.rows_written, 1,
        "custodian-endpoints through the door: {report:?}"
    );
    assert_eq!(report.still_owed, 0, "{report:?}");

    // The nest holds the keyless capability row for the custodian key.
    let row_blob = state
        .db
        .get_capability_grant(&alice_id.0, &GRANT_ID)
        .await
        .unwrap()
        .expect("the (owner, grant_id) custody row landed");
    let blob = GrantBlob::from_canonical_bytes(&row_blob).unwrap();
    assert!(blob.wrapped_keys.is_empty(), "custody is keyless always");
    assert_eq!(blob.holder, custodian_key);
    assert!(
        blob.scope
            .iter()
            .all(|t| t.class == ScopeTuple::CLASS_CUSTODY)
    );

    // The owner log holds the custody-shaped Mint event.
    let stored = alice_ledger.current();
    assert_eq!(stored.grant_events.len(), 1);
    assert!(fauna_client_capabilities::custody_grants::is_custody_grant(
        &stored.grant_events[0].scope
    ));

    // Convergence under the real generation: Alice's SECOND device walks the
    // fleet feed and reads the row (the W8.3 registration's deferred proof).
    alice_rig
        .fleet_plane(&alice_store_2, &alice_nest, &signing_key(ALICE_DEV_2))
        .walk()
        .await
        .unwrap();
    let entry_key = custody_entry_key(&GRANT_ID);
    let row = alice_store_2
        .state(KIND_CUSTODIAN_ENDPOINTS, &entry_key)
        .await
        .unwrap()
        .expect("device 2 converged on the custodian-endpoints row");
    let value: fauna_core::custodian_endpoints::CustodianEndpoints =
        fauna_core::encoding::canonical_decode(&row.value).unwrap();
    assert_eq!(
        value.endpoints.node_id, custodian_key,
        "whom to serve, and how to dial it"
    );

    // Step 4 — Bob's poll captures the deliver (the witness verifies against
    // his accepted key inside the ingest) and his drive writes the
    // custodies-held row through his own door.
    bob_rig
        .enroll_through_door(&bob_store, &bob_nest, BOB_DEV)
        .await;
    bob_rig
        .mint_through_door(&bob_store, &bob_nest, BOB_DEV, &[BOB_DEV])
        .await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel, &mut bob_seq, 0)
        .await
        .expect("bob poll 2");
    let counts = bob_backend.custody_payload_counts();
    assert_eq!((counts.seen, counts.captured), (2, 2), "{counts:?}");

    let report = drive_ceremonies(
        &bob_updater,
        &bob_ledger,
        &bob_rig.keypair,
        &bob_poster,
        &bob_writer,
        &bob_depositor,
        &NoHosting,
        now,
    )
    .await
    .expect("bob drive 2");
    assert_eq!(
        report.rows_written, 1,
        "custodies-held through the door: {report:?}"
    );
    assert_eq!(report.still_owed, 0, "{report:?}");

    let row = bob_store
        .state(KIND_CUSTODIES_HELD, &entry_key)
        .await
        .unwrap()
        .expect("the custodies-held row landed");
    let held: fauna_core::custodies_held::CustodyHeld =
        fauna_core::encoding::canonical_decode(&row.value).unwrap();
    assert_eq!(held.owner, alice_id.0);
    assert_eq!(
        held.retained_bytes_cap,
        DEFAULT_RETAINED_BYTES_CAP / 2,
        "the accept's budget rides into the row"
    );
    assert_eq!(held.owner_nest_url.as_deref(), Some(base.as_str()));
    assert_eq!(held.owner_devices.len(), 1);
    let witness: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&held.witness).unwrap();
    let admission = verify_custody_witness(&witness, &custodian_key, &alice_id, Timestamp::now())
        .expect("the held witness admits the accepted device");
    assert_eq!(admission.scopes, CustodyScopeSet::Account);

    // ── Settled: both drives owe nothing; no ceremony payload ever rendered ──
    let report = drive_ceremonies(
        &alice_updater,
        &alice_ledger,
        &alice_rig.keypair,
        &alice_poster,
        &alice_writer,
        &alice_depositor,
        &NoHosting,
        now,
    )
    .await
    .unwrap();
    assert_eq!(report, DriveReport::default(), "alice settled");
    let report = drive_ceremonies(
        &bob_updater,
        &bob_ledger,
        &bob_rig.keypair,
        &bob_poster,
        &bob_writer,
        &bob_depositor,
        &NoHosting,
        now,
    )
    .await
    .unwrap();
    assert_eq!(report, DriveReport::default(), "bob settled");
    let stored = alice_ledger.current();
    assert_eq!(stored.grant_events.len(), 1, "one mint, ever");

    // ── W8.7 arc 2: the over-budget custodian's check-in reaches the owner ──
    // Bob's budget pass judged his store over cap and evicted payload. The
    // pump's mint step records the signed receipt through the REAL CAS door
    // (never attested → due), his drive posts it over the same MLS channel,
    // Alice's poll captures it through the production sink, her drive folds
    // it onto the registry row — and the row reads DEGRADED, which is T15's
    // "coverage shrinkage surfaces owner-side … never silently", end to end.
    let accepted_cap = DEFAULT_RETAINED_BYTES_CAP / 2;
    let over_budget_meter = fauna_core::custody_policy::CustodyMeter {
        scopes: vec![fauna_core::custody_policy::ScopeMeter {
            scope: "state".into(),
            item_class: "state-entry".into(),
            rows: 12,
            payload_bytes: accepted_cap,
            ..Default::default()
        }],
    };
    let outcome = fauna_sync_engine::custody_leg::CustodyPassOutcome {
        grant_id: GRANT_ID.to_vec(),
        owner: alice_id.0,
        outcome: fauna_sync_engine::custody_leg::CustodyBudgetOutcome {
            judged: over_budget_meter.clone(),
            held: over_budget_meter,
            state: fauna_core::custody_policy::CustodyBudgetState::OverBudget,
            evicted: fauna_account_store::backend::RelayEvicted {
                rows: 3,
                bytes: 4_096,
            },
            held_bytes: accepted_cap,
            unreclaimable: 0,
            cap: accepted_cap,
        },
    };
    let mint_now = Timestamp::now();
    let minted = fauna_sync_engine::custody_leg::mint_due_receipts(
        &bob_updater,
        &bob_dev_key,
        &[outcome],
        mint_now,
    )
    .await;
    assert_eq!(minted, 1, "never attested → the first pass mints");

    let report = drive_ceremonies(
        &bob_updater,
        &bob_ledger,
        &bob_rig.keypair,
        &bob_poster,
        &bob_writer,
        &bob_depositor,
        &NoHosting,
        mint_now,
    )
    .await
    .expect("bob drive 3");
    assert_eq!(report.posted, 1, "the receipt went out: {report:?}");

    poll_inbound_conv(&alice_backend, &alice_manager, &channel, &mut alice_seq, 0)
        .await
        .expect("alice poll 2");
    let counts = alice_backend.custody_receipt_counts();
    assert_eq!(
        (counts.seen, counts.captured),
        (1, 1),
        "the receipt was verified against the accept-bound key and recorded: {counts:?}"
    );

    let report = drive_ceremonies(
        &alice_updater,
        &alice_ledger,
        &alice_rig.keypair,
        &alice_poster,
        &alice_writer,
        &alice_depositor,
        &NoHosting,
        mint_now,
    )
    .await
    .expect("alice drive 3");
    assert_eq!(
        report.rows_written, 1,
        "a fresh receipt re-opens the row write: {report:?}"
    );

    let row = alice_store_1
        .state(KIND_CUSTODIAN_ENDPOINTS, &entry_key)
        .await
        .unwrap()
        .expect("the owner's custody row, receipt included");
    let value: fauna_core::custodian_endpoints::CustodianEndpoints =
        fauna_core::encoding::canonical_decode(&row.value).unwrap();
    let receipt_bytes = value
        .latest_receipt
        .expect("the row carries the attestation — 'no receipt yet' is over");
    let env: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&receipt_bytes).unwrap();
    let receipt = fauna_core::custody_receipt::verify_custody_receipt(&env, &custodian_key)
        .expect("signed by the accept-bound custodian device, verbatim across the wire");
    assert!(
        receipt.is_degraded(),
        "the eviction is receipt-visible on the owner's row"
    );
    assert_eq!(receipt.evicted_bytes, 4_096);
    assert_eq!(receipt.held_bytes, accepted_cap);
    assert_eq!(receipt.retained_bytes_cap, accepted_cap);
    assert_eq!(receipt.attested_at, mint_now);

    // And Bob's record agrees: minted, posted, degraded — settled again.
    let bob_stored = bob_updater.current();
    let held_rec = &bob_stored.held[0];
    assert!(held_rec.receipt_posted && held_rec.receipt_degraded);
    assert_eq!(held_rec.receipt_minted_at, mint_now);

    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    assert!(
        detail
            .messages
            .iter()
            .all(|m| m.body == "<<setup>>" || m.body == "custody?"),
        "no ceremony payload may surface in the transcript: {:?}",
        detail.messages.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
    // The fleet feed carried only sealed forms — sanity that the rows exist
    // as StateEntry items on the fleet scope, never plaintext anywhere else.
    let _ = ItemClass::StateEntry;
}
