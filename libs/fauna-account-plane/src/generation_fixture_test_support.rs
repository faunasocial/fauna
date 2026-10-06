//! Shared test-fixture scaffolding for the generation-writer test suites
//! (`generation_topup`, `generation_unkeyable`, `generation_escrow_recover`,
//! `generation_tip`) — the first two hand-rolled the identical `Fixture` (a
//! throwaway SQLite-backed `AccountStore` behind a pull-only `NoNest`, plus a
//! fixed root/device/escrow key set) to drive `mint_over` against before
//! asserting their own writer's behavior, and the in-memory `Bundle` custody
//! is shared the same way. One owner here; each suite's own additional
//! test-only helpers (`removal_row`, `topup_rows`, …) stay local — they
//! diverge per suite.

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey, GenerationKey};
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::generation::{
    DeviceSetRecord, EscrowTargetRecord, FleetMember, GenerationMintRecord, MintCore,
    derive_device_xwing_keypair, derive_escrow_xwing_keypair, sign_device_enrollment,
};
use fauna_core::identity::ActorKeypair;
use fauna_mls::wrapped_blob::generation_wraps::build_mint;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::merge_policy::{KIND_DEVICE_SET, KIND_GENERATION_MINT, home_scope_for_kind};

use crate::account_state_plane::AccountStatePlane;
use crate::generation_tip::{GenerationTrust, RetainedKeyCustody};

pub const ROOT_SEED: [u8; 32] = [0x77u8; 32];
pub const US: [u8; 32] = [0x07u8; 32];
pub const THEM: [u8; 32] = [0xB1u8; 32];
pub const ESCROW_SEED: [u8; 32] = [0x55u8; 32];

pub fn root() -> ActorKeypair {
    ActorKeypair::from_secret(ROOT_SEED)
}

/// The fixture identity's escrow-target key — the string every wrap seals
/// under and every receipt names, so the resolver (which filters by the
/// trust root's key) counts the fixture's receipts.
pub fn target_key() -> String {
    fauna_core::generation::escrow_target_identity_key(&root().actor_id())
}

/// A device's production key shape: `[seed; 32]` is the Ed25519 secret, the
/// id is its public half, and the device KEM keypair derives from the same
/// secret bytes (`derive_device_xwing_keypair(&writer_key.to_bytes())`).
pub fn device_key(seed: [u8; 32]) -> SigningKey {
    SigningKey::from_bytes(&seed)
}

pub fn device_id_of(seed: [u8; 32]) -> [u8; 32] {
    device_key(seed).verifying_key().to_bytes()
}

pub fn member_of(seed: [u8; 32]) -> FleetMember {
    FleetMember {
        device_id: device_id_of(seed),
        xwing_pubkey: derive_device_xwing_keypair(&seed)
            .public
            .to_bytes()
            .to_vec(),
        enrolled_at_ms: 5_000,
    }
}

pub fn enrollment_row(seed: [u8; 32]) -> StateEntry {
    let id = device_id_of(seed);
    let cert = DeviceAuthorization {
        actor_id: root().actor_id(),
        device_key: id,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&root(), &cert).unwrap();
    let authorization =
        fauna_core::encoding::canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
    // Production's own shape: self-signed, the KEM half derived from the
    // secret that signs (`sign_device_enrollment`).
    machinery_row(
        KIND_DEVICE_SET,
        fauna_core::hex32::encode(&id),
        &sign_device_enrollment(&device_key(seed), authorization, 5_000),
    )
}

/// An UNSIGNED enrollment of `seed` carrying its valid cert — what a
/// vandal holding the `BackupKey` can file at a member's cell: never a
/// member at the view, still a lattice element, displaced by the device's
/// own signed row.
pub fn unsigned_enrollment_row(seed: [u8; 32]) -> StateEntry {
    let signed = enrollment_row(seed);
    let DeviceSetRecord::Enrolled {
        xwing_pubkey,
        authorization,
        enrolled_at_ms,
        ..
    } = fauna_core::encoding::canonical_decode(&signed.value).unwrap()
    else {
        unreachable!()
    };
    machinery_row(
        KIND_DEVICE_SET,
        signed.key,
        &DeviceSetRecord::Enrolled {
            xwing_pubkey,
            authorization,
            enrolled_at_ms,
            device_sig: Vec::new(),
        },
    )
}

pub fn machinery_row<T: serde::Serialize>(kind: &str, key: String, value: &T) -> StateEntry {
    StateEntry {
        kind: kind.into(),
        key,
        scope: home_scope_for_kind(kind).unwrap().into(),
        value: fauna_core::encoding::canonical_encode(value).unwrap(),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    }
}

/// Pull-only plane: a put that reached the wire would be the failure.
#[derive(Clone)]
pub struct NoNest;

impl RpcRequester for NoNest {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> anyhow::Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        anyhow::bail!("pull-only put reached the wire ({kind}) — it must be local-only")
    }
}

pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub store: AccountStore<SqliteBackend>,
    pub schedule: AccountStateKeySchedule,
    pub writer_key: SigningKey,
    pub trust: GenerationTrust,
}

pub async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer_key = device_key(US);
    let store = AccountStore::open(
        SqliteBackend::open(dir.path()).unwrap(),
        &root().actor_id_hex(),
        WriterId(writer_key.verifying_key().to_bytes()),
    )
    .await
    .unwrap();
    Fixture {
        _dir: dir,
        store,
        schedule: AccountStateKeySchedule::derive(&BackupKey::derive(&ROOT_SEED)),
        writer_key,
        trust: GenerationTrust {
            root: root().actor_id(),
            prior: Vec::new(),
            trusted_holders: Default::default(),
        },
    }
}

impl Fixture {
    pub fn plane(&self) -> AccountStatePlane<'_, SqliteBackend, NoNest> {
        AccountStatePlane::new_pull_only(
            &self.store,
            &NoNest,
            &self.schedule,
            &self.writer_key,
            &self.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .expect("plane")
    }

    pub async fn put(&self, entry: StateEntry) {
        self.store.put_state(entry).await.unwrap();
    }

    pub async fn mint_over(&self, members: &[FleetMember]) -> ([u8; 32], GenerationKey, MintCore) {
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            members,
            &escrow,
            &target_key(),
            Vec::new(),
            &device_key(US),
            7_000,
        )
        .expect("mint");
        self.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
            &built.record,
        ))
        .await;
        let core = match &built.record {
            GenerationMintRecord::Minted { core, .. } => core.clone(),
            GenerationMintRecord::Shredded { core, .. } => core.clone(),
        };
        (built.generation_id, built.gen_key, core)
    }
}

/// An in-memory retained-key bundle — the custody every suite hands the
/// generation reader (`RetainedKeyCustody`'s three duties, nothing more).
#[derive(Default)]
pub struct Bundle(std::sync::Mutex<std::collections::BTreeMap<[u8; 32], [u8; 32]>>);

impl RetainedKeyCustody for Bundle {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        self.0
            .lock()
            .unwrap()
            .get(generation)
            .map(|k| GenerationKey::from_bytes(*k))
    }
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        self.0.lock().unwrap().insert(*generation, *key.as_bytes());
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.0.lock().unwrap().remove(generation);
    }
}
