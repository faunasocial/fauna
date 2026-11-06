//! The group plane's two-seat fixtures, shared by the group-plane test
//! files (`group_plane_restart.rs`, `group_feed_over_wire.rs`): two real
//! `fauna-account-store` replicas of two different accounts, each with an
//! enrolled device and an established fleet generation tip, and the offline
//! share ceremony driven between them in memory. Each file uses a subset.
#![allow(dead_code)]

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_client_capabilities::group_ceremony::{
    admit_group_share, begin_group_share, build_group_accept, build_group_deliver,
    ingest_group_frame, mark_group_accept_posted, mark_group_delivered, mark_group_offer_posted,
    record_recipient_candidates,
};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode, sign_envelope};
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_file::{GroupFileChange, GroupFileCore, GroupFileRecord};
use fauna_core::group_generation::{GroupHeldRootRecord, GroupReceptionKeyRecord};
use fauna_core::group_name::{GroupNameCore, GroupNameRecord};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::group_state::{
    GROUP_NAME_KEY, KIND_GROUP_FILE, KIND_GROUP_NAME, KIND_GROUP_ROSTER,
};
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT, LwwStamp,
};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_sync_engine::generation_tip::GenerationTrust;
use fauna_sync_engine::group_state_plane::{
    GroupStatePlane, NoFeed, write_held_root_row, write_reception_key_row,
};

/// The wire half — the share seat and the seat's store doors — for the
/// files built with the group leg.
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub mod wire;

pub const ESCROW_SEED: [u8; 32] = [0x55u8; 32];

pub fn holder_key() -> SigningKey {
    SigningKey::from_bytes(&[0x66u8; 32])
}

pub fn now() -> Timestamp {
    Timestamp(1_700_000_000)
}

pub fn now_ms() -> i64 {
    1_700_000_000_000
}

/// One member seat: an account (its own actor + `BackupKey`) with one device
/// replica (the store's writer = the device signing key).
pub struct Seat {
    pub actor: ActorKeypair,
    pub device_seed: u8,
    pub schedule: AccountStateKeySchedule,
    pub store: AccountStore<SqliteBackend>,
    pub dir: tempfile::TempDir,
}

impl Seat {
    pub async fn open(actor_seed: u8, device_seed: u8, key_seed: u8) -> Seat {
        let dir = tempfile::tempdir().expect("seat dir");
        Seat {
            actor: ActorKeypair::from_secret([actor_seed; 32]),
            device_seed,
            schedule: AccountStateKeySchedule::derive(&BackupKey::derive(&[key_seed; 32])),
            store: Self::open_store(&dir, actor_seed, device_seed).await,
            dir,
        }
    }

    pub async fn open_store(
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
    pub async fn restart(&mut self) {
        let actor_hex = hex::encode(self.actor.actor_id().0);
        self.store = AccountStore::open(
            SqliteBackend::open(self.dir.path()).unwrap(),
            &actor_hex,
            WriterId(self.device().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
    }

    pub fn device(&self) -> SigningKey {
        SigningKey::from_bytes(&[self.device_seed; 32])
    }

    pub fn trust(&self) -> GenerationTrust {
        GenerationTrust {
            root: self.actor.actor_id(),
            prior: Vec::new(),
            trusted_holders: vec![holder_key().verifying_key().to_bytes()].into(),
        }
    }

    /// The device's actor-signed `DeviceAuthorization` in the `EmbedAsBytes`
    /// carriage roster entries, mints, and enrollments embed.
    pub fn device_cert(&self) -> Vec<u8> {
        self.device_cert_for(&self.device())
    }

    /// The same carriage for another of this actor's devices — a second
    /// authority device, say.
    pub fn device_cert_for(&self, device: &SigningKey) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: self.actor.actor_id(),
            device_key: device.verifying_key().to_bytes(),
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
    pub async fn fleet_put(&self, item: &ItemId, value: Vec<u8>) {
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
    pub async fn establish_fleet_tip(&self) {
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
    pub async fn write_custody_rows(
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
pub struct CeremonyRun {
    pub scope_id: [u8; 32],
    pub generation_id: [u8; 32],
    pub alice_reception: GroupReceptionKeyRecord,
    pub bob_reception: GroupReceptionKeyRecord,
    pub alice_root_row: GroupHeldRootRecord,
    pub bob_root_row: GroupHeldRootRecord,
    pub plane_rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    pub bob_rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    pub bob_entry_id: [u8; 32],
    pub bob_cfg: GroupShareConfig,
    /// The initiator's ceremony replica — its record holds Bob's
    /// compare-code candidates ([`BOB_CODE_LAN`]).
    pub alice_cfg: GroupShareConfig,
}

/// The LAN candidate Bob's compare code reads out in every fixture ceremony,
/// kept in Alice's record as the initiator's seat keeps it at begin.
pub const BOB_CODE_LAN: &str = "10.0.0.2:7001";

pub fn run_ceremony(alice: &Seat, bob: &Seat) -> CeremonyRun {
    run_ceremony_with(alice, bob, None)
}

/// [`run_ceremony`] with the initiator's endpoints carried in the deliver —
/// decision 8's bootstrap row.
pub fn run_ceremony_with(
    alice: &Seat,
    bob: &Seat,
    bootstrap: Option<&fauna_core::device_endpoints::DeviceEndpointsEntry>,
) -> CeremonyRun {
    let mut alice_cfg = GroupShareConfig::default();
    let mut bob_cfg = GroupShareConfig::default();

    let begun = begin_group_share(&mut alice_cfg, &alice.actor, bob.actor.actor_id(), now())
        .expect("begin");
    let scope_id = begun.scope_id;
    record_recipient_candidates(
        &mut alice_cfg,
        &scope_id,
        &bob.actor.actor_id(),
        &[BOB_CODE_LAN.parse().unwrap()],
    );
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
        bootstrap,
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
        alice_cfg,
    }
}

/// Both seats through the ceremony with their custody rows written and the
/// machinery adopted — every content-plane test starts here.
pub async fn admitted_pair() -> (Seat, Seat, CeremonyRun) {
    admitted_pair_with(None).await
}

/// [`admitted_pair`] with the initiator's endpoints entry for `bootstrap`
/// carried in the deliver (its `node_id` set to Alice's actor here). Every
/// snapshot row — the stamped bootstrap row included — is adopted on both
/// sides, none refused.
pub async fn admitted_pair_with(
    bootstrap: Option<fauna_core::device_endpoints::DeviceEndpointsEntry>,
) -> (Seat, Seat, CeremonyRun) {
    let alice = Seat::open(0x21, 0xA1, 0x07).await;
    let bob = Seat::open(0x31, 0xB2, 0x08).await;
    alice.establish_fleet_tip().await;
    bob.establish_fleet_tip().await;
    let bootstrap = bootstrap.map(|mut entry| {
        entry.node_id = alice.actor.actor_id().0;
        entry
    });
    let run = run_ceremony_with(&alice, &bob, bootstrap.as_ref());
    alice
        .write_custody_rows(&run.alice_root_row, &run.alice_reception)
        .await;
    bob.write_custody_rows(&run.bob_root_row, &run.bob_reception)
        .await;
    for (seat, root_row, rows) in [
        (&alice, &run.alice_root_row, &run.plane_rows),
        (&bob, &run.bob_root_row, &run.bob_rows),
    ] {
        let root = root_row.machinery_root().unwrap();
        let sk = seat.device();
        let plane = GroupStatePlane::new(&seat.store, &NoFeed, &root, &sk, &run.scope_id).unwrap();
        let report = plane.adopt_rows(rows).await.expect("adopt");
        assert_eq!(
            (report.adopted, report.refused),
            (rows.len(), 0),
            "every snapshot row adopts: {report:?}"
        );
    }
    (alice, bob, run)
}

/// `seat`'s roster entry core in the ceremony's snapshot.
pub fn entry_core_of(run: &CeremonyRun, seat: &Seat) -> fauna_core::group_scope::RosterEntryCore {
    run.plane_rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .find_map(|r| match canonical_decode(&r.value).ok()? {
            fauna_core::group_scope::GroupRosterRecord::Enrolled { core, .. }
                if core.member_actor == seat.actor.actor_id() =>
            {
                Some(core)
            }
            _ => None,
        })
        .expect("the seat's roster entry")
}

/// A signed `fauna.group.name` value and the stamp it merges under, authored
/// by `author`'s device — honoured only when that device is a current
/// authority device.
pub fn name_row(
    run: &CeremonyRun,
    author: &Seat,
    name: &str,
    at_ms: i64,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    name_row_by(run, author, &author.device(), name, at_ms)
}

/// [`name_row`] signed by another of `author`'s devices.
pub fn name_row_by(
    run: &CeremonyRun,
    author: &Seat,
    device: &SigningKey,
    name: &str,
    at_ms: i64,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    let core = GroupNameCore {
        scope_id: run.scope_id,
        name: name.to_string(),
        stamped_at_ms: at_ms,
    };
    let record = GroupNameRecord::sign(core, device, author.device_cert_for(device)).unwrap();
    let stamp = LwwStamp {
        at_ms,
        writer: device.verifying_key().to_bytes(),
    };
    (
        ItemId {
            kind: KIND_GROUP_NAME.into(),
            key: GROUP_NAME_KEY.into(),
        },
        canonical_encode(&record).unwrap().to_vec(),
        stamp.encode().unwrap(),
    )
}

/// A signed `fauna.group.file` value and the stamp it merges under, authored
/// by `author`'s device.
pub fn file_row(
    run: &CeremonyRun,
    author: &Seat,
    entry: Option<fauna_core::group_scope::RosterEntryCore>,
    path_byte: u8,
    at_ms: i64,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    file_row_in(run.generation_id, run, author, entry, path_byte, at_ms)
}

/// [`file_row`] naming `generation_id` — a row sealed under a generation
/// minted after the ceremony's.
pub fn file_row_in(
    generation_id: [u8; 32],
    run: &CeremonyRun,
    author: &Seat,
    entry: Option<fauna_core::group_scope::RosterEntryCore>,
    path_byte: u8,
    at_ms: i64,
) -> (ItemId, Vec<u8>, Vec<u8>) {
    let core = GroupFileCore {
        scope_id: run.scope_id,
        path_hash: [path_byte; 32],
        sealed_path: vec![path_byte; 24],
        change: GroupFileChange::Put {
            manifest_hash: [path_byte ^ 0xFF; 32],
            size_bytes: 5,
        },
        generation_id,
        stamped_at_ms: at_ms,
        writer_entry: entry,
        base_manifest: None,
    };
    let record = GroupFileRecord::sign(core, &author.device(), author.device_cert()).unwrap();
    let stamp = LwwStamp {
        at_ms,
        writer: author.device().verifying_key().to_bytes(),
    };
    (
        ItemId {
            kind: KIND_GROUP_FILE.into(),
            key: record.logical_key(),
        },
        canonical_encode(&record).unwrap().to_vec(),
        stamp.encode().unwrap(),
    )
}
