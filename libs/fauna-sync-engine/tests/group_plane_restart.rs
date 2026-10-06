//! Row 66 — the group plane's store proven end to end: a ceremony's plane
//! rows and held-root row land in a store and **survive a restart**, the
//! joiner's reception keypair rests durably and its admission bundle still
//! opens after one, and the scope's roster + generation tip resolve from
//! persisted state alone (`docs/goal/architecture/account-data-plane.md`
//! § Implementation status today, the group-scope paragraph).
//!
//! Two real `fauna-account-store` replicas of **two different accounts**
//! (cross-user — the group plane's whole point), no nest and no transport
//! anywhere: the ceremony runs in memory exactly as the capstone test drives
//! it, and every durable claim is asserted against a store **reopened from
//! disk**. The walk test then proves the serve shape: one member's sealed
//! relay rows, served as a feed, land on another member's replica through the
//! machinery-root trial-open — and stay dark to a holder without the root
//! (the custodian posture).
//!
//! The ceremony record's durability is deliberately out of scope: it
//! rides the `fauna.state.group-share-ceremony` door, proven by that kind's own
//! conformance test (`peer_leg_convergence.rs`). The stores are
//! what row 66 built; the cfgs stay in memory here.

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode, sign_envelope};
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_ceremony::{GroupShareDeliver, verify_group_share_deliver};
use fauna_core::group_generation::{
    GroupHeldRootRecord, GroupReceptionKeyRecord, group_generation_key_commitment,
    resolve_admissible_group_tip,
};
use fauna_core::group_scope::{GroupAuthority, GroupBirthRecord, RosterView};
use fauna_core::identity::ActorKeypair;
use fauna_mls::wrapped_blob::group_generation_wraps::open_group_admission_bundle;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ItemClass};
use fauna_protocol::group_state::{
    GROUP_BIRTH_KEY, KIND_GROUP_BIRTH, KIND_GROUP_GENERATION_MINT, KIND_GROUP_ROSTER,
};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT, KIND_GROUP_MACHINERY_ROOT,
    KIND_GROUP_RECEPTION_KEY,
};
use fauna_protocol::scope::GroupScope;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::{decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;
use fauna_sync_engine::group_state_plane::{
    GroupStatePlane, NoFeed, write_held_root_row, write_reception_key_row,
};

use fauna_client_capabilities::group_ceremony::{
    admit_group_share, begin_group_share, build_group_accept, build_group_deliver,
    ingest_group_frame, mark_group_accept_posted, mark_group_delivered, mark_group_offer_posted,
};

const ESCROW_SEED: [u8; 32] = [0x55u8; 32];

fn holder_key() -> SigningKey {
    SigningKey::from_bytes(&[0x66u8; 32])
}

fn now() -> Timestamp {
    Timestamp(1_700_000_000)
}

fn now_ms() -> i64 {
    1_700_000_000_000
}

/// One member seat: an account (its own actor + `BackupKey`) with one device
/// replica (the store's writer = the device signing key).
struct Seat {
    actor: ActorKeypair,
    device_seed: u8,
    schedule: AccountStateKeySchedule,
    store: AccountStore<SqliteBackend>,
    dir: tempfile::TempDir,
}

impl Seat {
    async fn open(actor_seed: u8, device_seed: u8, key_seed: u8) -> Seat {
        let dir = tempfile::tempdir().expect("seat dir");
        Seat {
            actor: ActorKeypair::from_secret([actor_seed; 32]),
            device_seed,
            schedule: AccountStateKeySchedule::derive(&BackupKey::derive(&[key_seed; 32])),
            store: Self::open_store(&dir, actor_seed, device_seed).await,
            dir,
        }
    }

    async fn open_store(
        dir: &tempfile::TempDir,
        actor_seed: u8,
        device_seed: u8,
    ) -> AccountStore<SqliteBackend> {
        let actor_hex = hex::encode(ActorKeypair::from_secret([actor_seed; 32]).actor_id().0);
        AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &actor_hex,
            WriterId(
                SigningKey::from_bytes(&[device_seed; 32])
                    .verifying_key()
                    .to_bytes(),
            ),
        )
        .await
        .unwrap()
    }

    /// The restart: a fresh store handle over the same on-disk replica.
    async fn restart(&mut self) {
        let actor_hex = hex::encode(self.actor.actor_id().0);
        self.store = AccountStore::open(
            SqliteBackend::open(self.dir.path()).unwrap(),
            &actor_hex,
            WriterId(self.device().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
    }

    fn device(&self) -> SigningKey {
        SigningKey::from_bytes(&[self.device_seed; 32])
    }

    fn trust(&self) -> GenerationTrust {
        GenerationTrust {
            root: self.actor.actor_id(),
            prior: Vec::new(),
            trusted_holders: vec![holder_key().verifying_key().to_bytes()].into(),
        }
    }

    /// The device's actor-signed `DeviceAuthorization` in the `EmbedAsBytes`
    /// carriage roster entries, mints, and enrollments embed.
    fn device_cert(&self) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: self.actor.actor_id(),
            device_key: self.device().verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&self.actor, &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("encode carriage")
            .to_vec()
    }

    /// A fleet-scope class-2 write through the pull-only account plane —
    /// journal + entry + relay row, no publish (no nest in this file).
    async fn fleet_put(&self, item: &ItemId, value: Vec<u8>) {
        let requester = NoFeed;
        let sk = self.device();
        let trust = self.trust();
        let plane = AccountStatePlane::new_pull_only(
            &self.store,
            &requester,
            &self.schedule,
            &sk,
            &trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        plane.put(item, value, None).await.expect("fleet put");
    }

    /// Stand up this account's R14 (account-data-plane.md § The ratified decisions) machinery — enroll the device, mint the
    /// first fleet generation, land the trusted holder's escrow receipt — so
    /// the tip-sealed group custody kinds can seal (the same fixture shape as
    /// the peer-leg convergence file's top-up test).
    async fn establish_fleet_tip(&self) {
        let writer_hex = fauna_core::hex32::encode(&self.device().verifying_key().to_bytes());
        let authorization = self.device_cert();
        let enrollment = canonical_encode(&fauna_core::generation::sign_device_enrollment(
            &self.device(),
            authorization,
            5_000,
        ))
        .unwrap()
        .to_vec();
        self.fleet_put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: writer_hex.clone(),
            },
            enrollment,
        )
        .await;

        let escrow = fauna_core::generation::EscrowTargetRecord {
            xwing_escrow_pubkey: fauna_core::generation::derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let member = fauna_core::generation::FleetMember {
            device_id: self.device().verifying_key().to_bytes(),
            xwing_pubkey: fauna_core::generation::derive_device_xwing_keypair(
                &[self.device_seed; 32],
            )
            .public
            .to_bytes()
            .to_vec(),
            enrolled_at_ms: 5_000,
        };
        let built = fauna_mls::wrapped_blob::generation_wraps::build_mint(
            &[member],
            &escrow,
            &fauna_core::generation::escrow_target_identity_key(&self.actor.actor_id()),
            Vec::new(),
            &self.device(),
            7_000,
        )
        .expect("fleet mint");
        self.fleet_put(
            &ItemId {
                kind: KIND_GENERATION_MINT.into(),
                key: fauna_core::hex32::encode(&built.generation_id),
            },
            canonical_encode(&built.record).unwrap().to_vec(),
        )
        .await;
        let receipt = fauna_core::generation::sign_escrow_receipt(
            &holder_key(),
            built.generation_id,
            blake3::hash(&built.escrow_wrap).into(),
            &fauna_core::generation::escrow_target_identity_key(&self.actor.actor_id()),
            7_100,
        );
        self.fleet_put(
            &ItemId {
                kind: KIND_ESCROW_RECEIPT.into(),
                key: format!(
                    "{}/{}",
                    fauna_core::hex32::encode(&built.generation_id),
                    fauna_core::hex32::encode(&receipt.holder_id)
                ),
            },
            canonical_encode(&receipt).unwrap().to_vec(),
        )
        .await;
    }

    /// Write the two group custody kinds through the fleet plane — the
    /// ceremony driver's account-plane seam under test.
    async fn write_custody_rows(
        &self,
        held_root: &GroupHeldRootRecord,
        reception: &GroupReceptionKeyRecord,
    ) {
        let requester = NoFeed;
        let sk = self.device();
        let trust = self.trust();
        let plane = AccountStatePlane::new_pull_only(
            &self.store,
            &requester,
            &self.schedule,
            &sk,
            &trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        write_reception_key_row(&plane, reception)
            .await
            .expect("reception key row");
        write_held_root_row(&plane, held_root)
            .await
            .expect("held root row");
    }
}

/// The whole two-seat ceremony, driven in memory; hands back everything the
/// drivers wrote and the values the assertions compare against.
struct CeremonyRun {
    scope_id: [u8; 32],
    generation_id: [u8; 32],
    alice_reception: GroupReceptionKeyRecord,
    bob_reception: GroupReceptionKeyRecord,
    alice_root_row: GroupHeldRootRecord,
    bob_root_row: GroupHeldRootRecord,
    plane_rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    bob_rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    bob_entry_id: [u8; 32],
    bob_cfg: GroupShareConfig,
}

fn run_ceremony(alice: &Seat, bob: &Seat) -> CeremonyRun {
    let mut alice_cfg = GroupShareConfig::default();
    let mut bob_cfg = GroupShareConfig::default();

    let begun = begin_group_share(&mut alice_cfg, &alice.actor, bob.actor.actor_id(), now())
        .expect("begin");
    let scope_id = begun.scope_id;
    mark_group_offer_posted(&mut alice_cfg, &scope_id, &bob.actor.actor_id());

    ingest_group_frame(
        &mut bob_cfg,
        &bob.actor.actor_id(),
        &alice.actor.actor_id(),
        &begun.frame,
        now(),
    )
    .expect("bob ingests offer");

    let bob_reception = GroupReceptionKeyRecord::mint(now_ms());
    let accept_frame =
        build_group_accept(&mut bob_cfg, &bob.actor, &scope_id, &bob_reception, now())
            .expect("accept");
    mark_group_accept_posted(&mut bob_cfg, &scope_id);

    ingest_group_frame(
        &mut alice_cfg,
        &alice.actor.actor_id(),
        &bob.actor.actor_id(),
        &accept_frame,
        now(),
    )
    .expect("alice ingests accept");

    let alice_reception = GroupReceptionKeyRecord::mint(now_ms());
    let delivered = build_group_deliver(
        &mut alice_cfg,
        &alice.actor,
        &alice.device(),
        alice.device_cert(),
        &alice_reception,
        &scope_id,
        &bob.actor.actor_id(),
        now(),
    )
    .expect("deliver");
    mark_group_delivered(&mut alice_cfg, &scope_id, &bob.actor.actor_id());

    ingest_group_frame(
        &mut bob_cfg,
        &bob.actor.actor_id(),
        &alice.actor.actor_id(),
        &delivered.frame,
        now(),
    )
    .expect("bob ingests deliver");

    let admitted =
        admit_group_share(&bob_cfg, &bob.actor, &bob_reception, &scope_id, now()).expect("admit");

    CeremonyRun {
        scope_id,
        generation_id: delivered.generation_id,
        alice_reception,
        bob_reception,
        alice_root_row: begun.held_root_row,
        bob_root_row: admitted.held_root_row,
        plane_rows: delivered.plane_rows,
        bob_rows: admitted.rows,
        bob_entry_id: admitted.entry_id,
        bob_cfg,
    }
}

/// Resolve the scope's roster + tip from a store's PERSISTED group rows alone
/// (plus the retained keys the reloaded admission bundle yields) — the
/// "scope is still there" assertion, shared by both seats.
fn resolve_from_store_rows(
    rows: &[fauna_account_store::types::StateEntry],
    scope_id: &[u8; 32],
    authority: &fauna_core::identity::ActorId,
    retained: &[([u8; 32], fauna_core::crypto::GenerationKey)],
) -> [u8; 32] {
    let roster: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let authority = &GroupAuthority::build(scope_id, authority, &[], std::iter::empty());
    let view = RosterView::build(scope_id, authority, roster.iter().copied());
    let mints: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_GENERATION_MINT)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let resolution = resolve_admissible_group_tip(
        &view,
        authority,
        mints.iter().copied(),
        std::iter::empty(),
        |id, core, _wraps| {
            retained.iter().any(|(rid, key)| {
                rid == id && group_generation_key_commitment(key) == core.key_commitment
            })
        },
    );
    resolution
        .tip
        .unwrap_or_else(|| {
            panic!(
                "persisted rows resolve no keyable tip (invalid: {:?})",
                resolution.invalid
            )
        })
        .generation_id
}

/// The account runtime's door-put verdict rests on this answer: a
/// tip-sealed origination runs the first-need mint (network) exactly while
/// no admissible tip resolves, and a `Gen0` kind never does — so a door put
/// served inside a pass once a tip resolves is store work only
/// (`AccountStatePlane::origination_mints`; `account-data-plane.md` § The
/// client-side lifecycle, the pump bullet → *Commands and passes*).
#[tokio::test]
async fn a_tip_sealed_origination_mints_only_while_no_tip_resolves() {
    /// A publishing plane's requester with nothing behind it: the question
    /// is answered from the store alone, so any request is the failure.
    #[derive(Debug)]
    struct NoNest(&'static str);
    impl std::fmt::Display for NoNest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test: {}", self.0)
        }
    }
    impl fauna_protocol::RpcErrorClass for NoNest {
        fn is_rejection(&self) -> bool {
            false
        }
    }
    struct Offline;
    impl fauna_protocol::RpcRequester for Offline {
        type Error = NoNest;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, NoNest>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(NoNest(kind))
        }
    }

    let seat = Seat::open(0x41, 0xC3, 0x09).await;
    let requester = Offline;
    let sk = seat.device();
    let trust = seat.trust();
    let fleet = AccountStatePlane::new(
        &seat.store,
        &requester,
        &seat.schedule,
        &sk,
        &trust,
        ACCOUNT_STATE_FLEET_SCOPE,
    )
    .unwrap();

    assert!(
        fleet
            .origination_mints(KIND_GROUP_RECEPTION_KEY)
            .await
            .unwrap(),
        "no tip yet: the door would mint"
    );
    assert!(
        !fleet.origination_mints(KIND_DEVICE_SET).await.unwrap(),
        "a Gen0 machinery kind never mints"
    );

    seat.establish_fleet_tip().await;
    for kind in [KIND_GROUP_RECEPTION_KEY, KIND_GROUP_MACHINERY_ROOT] {
        assert!(
            !fleet.origination_mints(kind).await.unwrap(),
            "{kind}: an acked tip resolves, so the door writes without minting"
        );
    }
}

/// The success criterion, verbatim: mint on one seat, admit on
/// another, restart, and the scope is still there with its roster and
/// generation tip resolvable — from disk, both seats.
#[tokio::test(flavor = "multi_thread")]
async fn the_ceremony_scope_survives_a_restart_on_both_seats() {
    let mut alice = Seat::open(0x21, 0xA1, 0x07).await;
    let mut bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();

    // The driver writes, both seats: custody rows through the fleet-scope
    // account plane, machinery rows through the group plane.
    alice
        .write_custody_rows(&run.alice_root_row, &run.alice_reception)
        .await;
    bob.write_custody_rows(&run.bob_root_row, &run.bob_reception)
        .await;

    let alice_root = run.alice_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane = GroupStatePlane::new(&alice.store, &requester, &alice_root, &sk, &run.scope_id)
            .unwrap();
        let report = plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
        assert_eq!(report.adopted, 4, "birth + 2 roster + mint: {report:?}");
    }
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = bob.device();
        let plane =
            GroupStatePlane::new(&bob.store, &requester, &bob_root, &sk, &run.scope_id).unwrap();
        let report = plane.adopt_rows(&run.bob_rows).await.expect("bob adopts");
        assert_eq!(report.adopted, 4, "{report:?}");
    }

    // ── The restart ─────────────────────────────────────────────────────────
    alice.restart().await;
    bob.restart().await;

    // Both seats: the scope's four machinery rows are on disk.
    for (name, seat) in [("alice", &alice), ("bob", &bob)] {
        let rows = seat.store.group_scope_states(&scope_str).await.unwrap();
        assert_eq!(rows.len(), 4, "{name}: persisted group rows");
        assert!(
            rows.iter()
                .any(|r| r.kind == KIND_GROUP_BIRTH && r.key == GROUP_BIRTH_KEY),
            "{name}: birth row persisted"
        );
    }

    // Bob's custody rows rest in the reopened account store (the local
    // replica's plaintext half — the sealed halves ride the relay plane).
    let reception_key = run.bob_reception.logical_key().unwrap();
    let reception_entry = bob
        .store
        .state(KIND_GROUP_RECEPTION_KEY, &reception_key)
        .await
        .unwrap()
        .expect("reception keypair row persisted");
    let reloaded_reception: GroupReceptionKeyRecord =
        canonical_decode(&reception_entry.value).unwrap();
    let root_entry = bob
        .store
        .state(
            KIND_GROUP_MACHINERY_ROOT,
            &GroupHeldRootRecord::logical_key_for(&run.scope_id),
        )
        .await
        .unwrap()
        .expect("held root row persisted");
    let reloaded_root_row: GroupHeldRootRecord = canonical_decode(&root_entry.value).unwrap();

    // The reloaded root verifies against the PERSISTED birth record's
    // commitment — the id-bound-at-birth check, from disk.
    let birth_entry = bob
        .store
        .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
        .await
        .unwrap()
        .expect("birth row");
    let birth: GroupBirthRecord = canonical_decode(&birth_entry.value).unwrap();
    assert_eq!(
        reloaded_root_row.machinery_root().unwrap().commitment(),
        birth.machinery_root_commit,
        "reloaded root commits to the persisted birth record"
    );

    // The admission bundle still opens after the restart, with the RELOADED
    // reception secret — and its retained keys make the persisted mint DAG
    // resolvable.
    let invited = run
        .bob_cfg
        .invited
        .iter()
        .find(|r| r.scope_id == run.scope_id)
        .expect("bob's ceremony record");
    let deliver_env: EmbedAsBytes = canonical_decode(&invited.deliver).unwrap();
    let deliver: GroupShareDeliver =
        verify_group_share_deliver(&deliver_env, &alice.actor.actor_id()).expect("deliver");
    let keypair = reloaded_reception.keypair().unwrap();
    let (opened_root, retained) = open_group_admission_bundle(
        &deliver.admission_wrap,
        &keypair.secret,
        &run.scope_id,
        &run.bob_entry_id,
        &birth,
    )
    .expect("admission bundle opens with the reloaded reception secret");
    assert_eq!(
        opened_root.commitment(),
        birth.machinery_root_commit,
        "the re-opened root is the scope's"
    );

    // The scope resolves from persisted rows alone, both seats.
    let bob_rows = bob.store.group_scope_states(&scope_str).await.unwrap();
    assert_eq!(
        resolve_from_store_rows(&bob_rows, &run.scope_id, &alice.actor.actor_id(), &retained),
        run.generation_id,
        "bob's persisted scope resolves the delivered tip"
    );
    let alice_rows = alice.store.group_scope_states(&scope_str).await.unwrap();
    assert_eq!(
        resolve_from_store_rows(
            &alice_rows,
            &run.scope_id,
            &alice.actor.actor_id(),
            &retained
        ),
        run.generation_id,
        "alice's persisted scope resolves the same tip"
    );
}

/// A canned group-scope feed serving one member's relay rows — the share
/// serve set's shape without its transport (that wiring is the `p2p-share`
/// workstream's). Honors the per-writer frontier so a caught-up walk
/// terminates on an empty page.
struct FakeGroupFeed {
    scope: String,
    rows: Vec<SyncChange>,
}

impl fauna_protocol::RpcRequester for FakeGroupFeed {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::ensure!(
            kind == "fauna.sync.changes.list",
            "the canned feed serves the feed only, got {kind}"
        );
        let req: SyncChangesListRequest =
            decode_strict(&encode_canonical(&payload)?).expect("feed request shape");
        anyhow::ensure!(req.scope.as_deref() == Some(self.scope.as_str()));
        let frontier = req.frontier.unwrap_or_default();
        let reply = SyncChangesListReply {
            changes: self
                .rows
                .iter()
                .filter(|r| {
                    let writer = r.origin_writer.as_deref().unwrap_or_default();
                    r.origin_seq.unwrap_or(0) > frontier.get(writer).copied().unwrap_or(0)
                })
                .cloned()
                .collect(),
            ..Default::default()
        };
        Ok(decode_strict(&encode_canonical(&reply)?)?)
    }
}

/// One member's sealed relay rows as the feed a peer would serve.
async fn feed_of(seat: &Seat, scope_str: &str) -> FakeGroupFeed {
    let relay = seat
        .store
        .relay_rows(scope_str, ItemClass::StateEntry.as_wire(), &[], 100)
        .await
        .unwrap();
    let rows = relay
        .iter()
        .enumerate()
        .map(|(i, r)| SyncChange {
            seq: (i + 1) as i64,
            path_hash: hex::encode(r.item_key.as_slice()),
            change_type: r.op.clone(),
            origin_writer: Some(r.writer.to_hex()),
            origin_seq: Some(r.writer_seq as i64),
            entry: r.entry.clone().map(Into::into),
            ..Default::default()
        })
        .collect();
    FakeGroupFeed {
        scope: scope_str.to_string(),
        rows,
    }
}

/// The walk's first contact holds a served birth row to its scope. Bob holds
/// the root, so he can seal a birth row under the scope's coordinates — here
/// one naming HIMSELF as authority, which hashes to another scope's id. Carol
/// holds the root with no birth row yet (a sibling device fresh off the fleet)
/// and is served Bob's row first: it must be skipped, never landed, because
/// the birth kind is `Immutable` — landed, it would root the scope in Bob for
/// good and refuse Alice's real record after it. Served next, the real one
/// lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_birth_row_that_is_another_scopes_never_lands_on_first_contact() {
    let alice = Seat::open(0x21, 0xA1, 0x07).await;
    let bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let root = run.alice_root_row.machinery_root().unwrap();
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane =
            GroupStatePlane::new(&alice.store, &requester, &root, &sk, &run.scope_id).unwrap();
        plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
    }

    // Bob's forgery, sealed under the real scope's coordinates onto his own
    // relay plane (below the plane's own write funnel, which refuses it).
    let real_birth: GroupBirthRecord = canonical_decode(
        &run.plane_rows
            .iter()
            .find(|r| r.kind == KIND_GROUP_BIRTH)
            .expect("the snapshot carries its birth row")
            .value,
    )
    .unwrap();
    let forged = GroupBirthRecord {
        authority_actor: bob.actor.actor_id(),
        ..real_birth.clone()
    };
    assert_ne!(
        fauna_core::group_scope::group_scope_id(&forged).unwrap(),
        run.scope_id
    );
    let bob_writer = bob.store.writer();
    let sealed = fauna_core::account_entry_crypto::seal_entry(
        &fauna_core::crypto::GroupMachinerySchedule::derive(&root).for_kind(KIND_GROUP_BIRTH),
        &fauna_core::account_entry_crypto::EntryCoordinates {
            writer_id: bob_writer.0,
            writer_seq: 1,
            scope: &scope_str,
        },
        &fauna_core::account_entry_crypto::EntryPlaintext {
            kind: KIND_GROUP_BIRTH.into(),
            key: GROUP_BIRTH_KEY.into(),
            merge_meta: None,
            value: canonical_encode(&forged).unwrap().into(),
            tombstone: false,
        },
        &bob.device(),
    )
    .unwrap();
    bob.store
        .record_relay_row(&fauna_account_store::types::RelayRow {
            scope: scope_str.clone(),
            item_class: ItemClass::StateEntry.as_wire().to_string(),
            writer: bob_writer,
            writer_seq: 1,
            item_key: sealed.item_key.to_vec(),
            op: fauna_protocol::account_state::OP_STATE_PUT.to_string(),
            entry: Some(sealed.envelope),
            feed_seq: None,
        })
        .await
        .unwrap();

    let carol = Seat::open(0x51, 0xD4, 0x0A).await;
    let sk = carol.device();
    let forged_feed = feed_of(&bob, &scope_str).await;
    let plane =
        GroupStatePlane::new(&carol.store, &forged_feed, &root, &sk, &run.scope_id).unwrap();
    let report = plane.walk().await.expect("carol walks bob's feed");
    assert_eq!(
        (report.rows, report.unmergeable, report.applied),
        (1, 1, 0),
        "{report:?}"
    );
    assert!(
        carol
            .store
            .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
            .await
            .unwrap()
            .is_none(),
        "a birth row that is another scope's never lands"
    );

    let real_feed = feed_of(&alice, &scope_str).await;
    let plane = GroupStatePlane::new(&carol.store, &real_feed, &root, &sk, &run.scope_id).unwrap();
    let report = plane.walk().await.expect("carol walks alice's feed");
    assert_eq!(report.applied, 4, "{report:?}");
    let held = carol
        .store
        .group_state(&scope_str, KIND_GROUP_BIRTH, GROUP_BIRTH_KEY)
        .await
        .unwrap()
        .expect("the real birth row lands after the refused one");
    assert_eq!(held.value, canonical_encode(&real_birth).unwrap().to_vec());
}

/// The serve shape: a root-holding member walks another member's sealed rows
/// off a feed and lands all four through the machinery-root trial-open, with
/// the frontier accounting the walk; a holder WITHOUT the root — the
/// custodian posture — opens none of them.
#[tokio::test(flavor = "multi_thread")]
async fn sealed_rows_walk_member_to_member_and_stay_dark_without_the_root() {
    let alice = Seat::open(0x21, 0xA1, 0x07).await;
    let bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;

    let run = run_ceremony(&alice, &bob);
    let scope_str = GroupScope::new(run.scope_id).to_string();
    let alice_root = run.alice_root_row.machinery_root().unwrap();

    // Alice's own plane writes — journal + sealed relay rows.
    {
        let requester = NoFeed;
        let sk = alice.device();
        let plane = GroupStatePlane::new(&alice.store, &requester, &alice_root, &sk, &run.scope_id)
            .unwrap();
        plane
            .adopt_rows(&run.plane_rows)
            .await
            .expect("alice adopts");
    }
    let feed = feed_of(&alice, &scope_str).await;
    assert_eq!(feed.rows.len(), 4, "alice serves four sealed rows");

    // Bob — holding the root from his admission — walks the feed: every row
    // opens, lands, and is accounted.
    let bob_root = run.bob_root_row.machinery_root().unwrap();
    {
        let sk = bob.device();
        let plane = GroupStatePlane::new(&bob.store, &feed, &bob_root, &sk, &run.scope_id).unwrap();
        let report = plane.walk().await.expect("bob walks");
        assert_eq!(report.applied, 4, "{report:?}");
        assert_eq!(report.unopened, 0, "{report:?}");
        let rows = bob.store.group_scope_states(&scope_str).await.unwrap();
        assert_eq!(rows.len(), 4);
        // A second walk serves nothing: the frontier accounted the first.
        let again = plane.walk().await.expect("bob re-walks");
        assert_eq!(again.rows, 0, "{again:?}");
    }

    // A holder without the scope's root — a custodian, or any stranger —
    // sees nothing past the coordinates: every row stays unopened.
    let mallory = Seat::open(0x41, 0xC3, 0x09).await;
    let wrong_root = fauna_core::crypto::GroupMachineryRoot::mint();
    let sk = mallory.device();
    let plane =
        GroupStatePlane::new(&mallory.store, &feed, &wrong_root, &sk, &run.scope_id).unwrap();
    let report = plane.reconcile().await.expect("mallory walks");
    assert_eq!(report.unopened, 4, "{report:?}");
    assert_eq!(
        report.applied + report.merged + report.kept,
        0,
        "{report:?}"
    );
    assert!(
        mallory
            .store
            .group_scope_states(&scope_str)
            .await
            .unwrap()
            .is_empty(),
        "nothing lands without the root"
    );
}
