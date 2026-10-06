//! The T10 principal-bundle carriage — the slot attributes beside the writer
//! key (W5 (account-data-plane.md § Workstreams).4a; charter `account-data-plane.md` § The store device principal).
//!
//! One [`PrincipalBundle`] per assembled runtime, resolved **inside the store's
//! migration/adoption critical section** exactly like the writer key: every
//! bundle item is a probe-then-act on state shared by all co-located
//! processes, and W5.3 measured what an unserialized mint-or-load does to the
//! loser (`account_runtime`'s writer-key comment tells that story).
//!
//! The durable bundle this module carries, under the writer key's namespace
//! ([`crate::account_runtime::CRED_NAMESPACE`]) with sibling account
//! attributes (`<actor_id_hex>/<suffix>` — slash-separated attributes are the
//! store's shipped idiom, e.g. `fauna/index`):
//!
//! - **`<hex>/device-auth`** — the root-signed [`DeviceAuthorization`] over
//!   this store's writer key, persisted as the same hex-over-canonical-dag-cbor
//!   [`EmbedAsBytes`] wire the ceremony registers on the nest, so any
//!   co-located process can re-verify and re-register it without the seed.
//!   Loaded, never minted, here: minting is the enrollment ceremony's job
//!   (W5.4b), and an absent or unusable value simply reads as "not enrolled
//!   yet" — the ceremony heals it at the next seed-holding sign-in.
//! - **`<hex>/backup-key`** — the owner `BackupKey`. On a seed-holding
//!   assembly this is write-mostly carriage (the seed derives it fresh every
//!   time); its consumer is the seedless *process* — W5.5's app-dead agent
//!   mounts the store from this slot and can derive nothing. Because the
//!   derived value is definitionally correct for the account seed, a
//!   mismatched slot value is **overwritten** (loudly) rather than refused —
//!   the deliberate asymmetry with the writer key, whose re-mint would fork
//!   the store identity where re-writing the backup key cannot fork anything.
//! - **`<hex>/generation-keys`** — the retained generation keys: the R14 (account-data-plane.md § The ratified decisions)
//!   carriage requirement (`owner-key-material.md` § Path A-sibling-2 →
//!   *bundle carriage*: "current tip + every generation still held for
//!   reading, in the same credential-slot custody class as the `BackupKey`").
//!   "Still held for reading" is load-bearing: a **shredded** generation is
//!   exactly the one no longer held — "deleting a generation = devices drop
//!   it + escrow holders delete the wrap" (the charter's crypto-shred
//!   ruling) — so this slot drops an entry the moment its mint's `Shredded`
//!   state is observed ([`crate::generation_tip::RetainedKeyCustody`], duty
//!   three), and the carriage's read value is the *live*-generation windows
//!   the plane cannot bridge: a store re-syncing from scratch, a sealed row
//!   walked ahead of its writer's mint row, a top-up that raced enrollment.
//!   Distribution between devices stays plane-native (mint-entry wraps + the
//!   top-up kind) — this is custody, never a transport.
//!
//! Session bearers are deliberately **absent**: each process mints its own
//! over `fauna.auth.device_handshake` (T10 — "no cross-process refresh race,
//! and the sessions list stays an honest per-process record").
//!
//! **One bundle, every host** (`account-client-lifecycle.md` § The
//! client-side lifecycle → *Ruling (4)'s build decisions*, decision (c)):
//! [`PrincipalBundle`] is generic over the two things that differ per host —
//! the secret store the attributes rest in ([`SecretStore`]: the native
//! `fauna_credential_store::CredentialStore`, web's
//! `LocalStorageSecretStore`) and the section its read-modify-writes
//! serialize under ([`SlotSection`]: natively the store dir's
//! `migration.lock`, on web the tab's own section). The attribute names,
//! the encodings, the latch, the refusal, the staged removals and the
//! retained-key carriage are this module's alone, so a web slot and a
//! native slot are the same bytes. The native instantiation is
//! `fauna_sync_engine::principal_bundle::PrincipalSlot`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub use fauna_client_accounts::SecretStore;
use fauna_core::crypto::{BackupKey, GenerationKey};
use fauna_core::data::DeviceAuthorization;
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, canonical_encode, decode_signed_bytes, verify_envelope,
};
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::generation_tip::RetainedKeyCustody;
use crate::principal_custody::{
    EnrollmentRefusal, LoadedDeviceAuthorization, PrincipalBundleStatus, PrincipalCustody,
};

/// The section every slot read-modify-write serializes under — the seam's
/// second half beside [`SecretStore`]. Natively the store dir's
/// `migration.lock` (the W5.3 lock, reused — one lock for every
/// probe-then-act on shared per-store slot state); on web the tab's own
/// section, which decision (c) accepts because the only list written that way
/// (the staged removals) is written by a human gesture and completed by the
/// one pump holder.
pub trait SlotSection: Send + Sync {
    /// Held for the read-modify-write; dropping it leaves the section.
    type Guard;

    /// Enter the section. `None` = degraded: the implementer could not
    /// serialize and has said so; the caller proceeds unserialized, and
    /// `degraded` names what that costs it (degrade is open, matching the
    /// store).
    fn enter(&self, degraded: &str) -> Option<Self::Guard>;
}

/// The widest value one slot attribute may hold — the credential store's
/// tightest backend (Windows Credential Manager), which the retained-key
/// carriage trims to fit. The owner is
/// `fauna_credential_store::MAX_ITEM_VALUE_BYTES`; the native instantiation
/// pins the two equal at compile time, and web's `localStorage` takes far
/// more, so the bound costs it nothing.
pub const MAX_SLOT_VALUE_BYTES: usize = 2560;

/// Account-attribute suffixes for the bundle items (`<actor_id_hex>/<suffix>`).
const ATTR_DEVICE_AUTH: &str = "device-auth";
const ATTR_BACKUP_KEY: &str = "backup-key";
const ATTR_GENERATION_KEYS: &str = "generation-keys";
/// The ceremony's nest-leg latch (W5.4b): holds the slot encoding of the
/// grant wire that `fauna.sync.register` + `fauna.sync.device_grant.register`
/// last succeeded for, so the pump's registration step is content-addressed —
/// a re-minted grant mismatches and re-registers; an unchanged one costs no
/// RPC. Lives beside the grant (not in `store_meta`) because it is enrollment
/// state with the grant's own lifecycle: the per-actor sweep
/// (`AccountRegistry::clear_all` / `remove`, via `account_scoped_aux_stores`)
/// clears both together — sign-out, account removal and factory reset all go
/// through it; see `long-term-store.md` § Cleanup contract.
const ATTR_GRANT_REGISTERED: &str = "grant-registered";
/// The nest's last **refusal** of this machine's enrollment, beside the latch
/// that records its success (2026-09-15) — today one value,
/// [`EnrollmentRefusal::DeviceLimitExceeded`]'s [`EnrollmentRefusal::slot_value`].
/// Written by the pump's enrollment pass in WHICHEVER process holds the pump
/// (on a desktop that is usually the co-located sync agent, not the app), and
/// read fresh off the credential store by every process, which is what lets
/// the app's Devices page render a refusal it never met itself
/// (`ui/devices.md` § Errors & edge cases). Cleared by the next successful
/// register ([`PrincipalBundle::record_grant_registered_on`]) and swept with the
/// rest of the bundle at sign-out (`fauna-client-accounts`' per-actor key
/// list, which names this suffix literally).
const ATTR_ENROLLMENT_REFUSED: &str = "enrollment-refused";
/// The devices page's removals whose fleet leg is not known finished
/// (`fauna_core::fleet_removal::StagedFleetRemoval`, its `encode_pending`
/// spelling — each intent plus the reconcile's first sighting of its row
/// still on the roster) — the durable intent staged BEFORE `fauna.sync.devices.delete`
/// so a crash, a failed write or a down runtime between the two legs cannot
/// leave the removed device a fleet member for ever
/// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (4), *The completion rule*). In the credential slot
/// rather than the replicated store on purpose: it is this machine's own
/// unfinished business, it must be writable before any plane write is, and
/// every process sharing the slot (the co-located agent usually holds the
/// pump) must see it. Public values only — rows, fleet ids and times, no key
/// material.
const ATTR_PENDING_FLEET_REMOVALS: &str = "pending-fleet-removals";
/// What a degraded [`PrincipalBundle::slot_write_section`] costs the retained
/// generation keys' read-merge-write.
const RETAINED_KEYS_UNSERIALIZED: &str = "writing the retained generation keys unserialized \
     (a racing sibling's write may be lost; the plane re-supplies records, and a shred \
     re-drops on next read)";
/// Splits the latch's two halves — `<device_id_hex>:<grant encoding>`. A colon
/// cannot occur in either half (both are hex), so the split is unambiguous; a
/// value carrying no separator at all is not a latch this code writes, and
/// reads as unregistered (see [`PrincipalBundle::grant_registration_row`]).
const LATCH_SEPARATOR: char = ':';

fn attr(actor_id_hex: &str, suffix: &str) -> String {
    format!("{actor_id_hex}/{suffix}")
}

/// The slot's memory that its writer key was MINTED but no store has stamped
/// it yet (`account-replica-posture.md` § The store device principal,
/// refinement 11). Set by the mint-or-load resolver in the same section as
/// the mint, cleared by the assembly once `AccountStore::open` has stamped
/// the store. The journal-bound writer's inverse arm keys on a key LOADED
/// over a store with no stamped writer — and a concurrent cold assembly
/// (two processes on one store dir, the app beside its agent) loads the key
/// its sibling minted moments ago, before that sibling's open stamps the
/// store: without this marker it would retire a key that never had a
/// journal to lose. A crash between the mint and the first open leaves the
/// marker, and the next launch adopts the key instead of retiring it — the
/// right answer too, since a key no store ever stamped never published.
const WRITER_UNSTAMPED_SUFFIX: &str = "writer-unstamped";

/// Record that the slot's writer key is freshly minted and no store is
/// stamped with it yet.
pub fn mark_writer_unstamped<S: SecretStore + ?Sized>(credentials: &S, actor_id_hex: &str) {
    credentials.set(&attr(actor_id_hex, WRITER_UNSTAMPED_SUFFIX), "1");
}

/// Void `actor_id_hex`'s grant-registration latch in `credentials` — the slot
/// seam's [`PrincipalCustody::void_grant_registration`], reachable without a
/// bundle so a host's device-principal client can call it the moment its
/// handshake is answered `not_registered` (`account-replica-posture.md`
/// § The store device principal: a `not_registered` answer voids the latch
/// and re-registers through the owner session, never a bare retry).
///
/// [`PrincipalCustody::void_grant_registration`]: crate::principal_custody::PrincipalCustody::void_grant_registration
pub fn void_grant_registration<S: SecretStore + ?Sized>(credentials: &S, actor_id_hex: &str) {
    if credentials
        .get(&attr(actor_id_hex, ATTR_GRANT_REGISTERED))
        .is_some()
    {
        tracing::info!(
            "principal bundle: the nest answered this machine's device handshake \
             `not_registered` — the registration latch is void, and the next owner-session \
             pass registers the grant again"
        );
        credentials.delete(&attr(actor_id_hex, ATTR_GRANT_REGISTERED));
    }
}

/// A store is stamped with the slot's writer: the mint marker is spent.
pub fn clear_writer_unstamped<S: SecretStore + ?Sized>(credentials: &S, actor_id_hex: &str) {
    credentials.delete(&attr(actor_id_hex, WRITER_UNSTAMPED_SUFFIX));
}

/// Whether the slot's writer key was minted and never yet stamped into a
/// store — a mint in flight in a sibling process, or a crash before the
/// first open.
pub fn writer_unstamped<S: SecretStore + ?Sized>(credentials: &S, actor_id_hex: &str) -> bool {
    credentials
        .get(&attr(actor_id_hex, WRITER_UNSTAMPED_SUFFIX))
        .is_some()
}

/// Read a slot value that holds **key material**, in the one custody shape all
/// of them use: the hex string is scrubbed when it drops.
///
/// Every read of the `backup-key` and `generation-keys` attributes goes through
/// here — one function rather than four call sites each remembering to wrap,
/// because fixing one member of a custody family and leaving the others bare is
/// exactly how the family splits (`key-material-hierarchy.md` § Carrier shape).
///
/// The `device-auth` attribute deliberately does **not** use this: a
/// root-signed [`DeviceAuthorization`] is a public, signature-verified
/// capability, not secret material, and wrapping it would blur a boundary
/// worth keeping legible.
fn read_secret_slot_value<S: SecretStore + ?Sized>(
    credentials: &S,
    key: &str,
) -> Option<Zeroizing<String>> {
    credentials.get(key).map(Zeroizing::new)
}

/// Compile-level custody pin (`key-material-hierarchy.md` § Carrier shape →
/// *Pinned at compile time*): revert [`read_secret_slot_value`] to a bare
/// `String` and the build fails **here**, at the pin, rather than only at
/// whichever call site happened to be strictly typed. A runtime test cannot do
/// this job — it can observe neither an absent `Drop` nor an implicit copy.
const _SECRET_SLOT_READ_IS_ZEROIZING: fn(
    &(dyn SecretStore + 'static),
    &str,
) -> Option<Zeroizing<String>> = read_secret_slot_value::<dyn SecretStore + 'static>;

/// The at-rest record behind `<hex>/generation-keys` — hex-over-canonical-
/// dag-cbor, a named-field record so later fields (a tip marker, retirement
/// stamps) can join additively. Public only so the native slot's tests can
/// measure the carriage ([`test_support`]).
#[doc(hidden)]
#[derive(Serialize, Deserialize, Zeroize)]
pub struct RetainedGenerationKeysRecord {
    pub entries: Vec<RetainedGenerationKeyEntry>,
}

#[doc(hidden)]
#[derive(Serialize, Deserialize, Zeroize)]
pub struct RetainedGenerationKeyEntry {
    #[serde(with = "serde_bytes")]
    pub generation: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub key: [u8; 32],
}

/// The T10 slot, resolved once per assembly and owned by the runtime worker.
///
/// Interior mutability because the read paths that *obtain* generation keys
/// (`generation_tip`, the plane walk) hold `&self` — recording a newly
/// unwrapped key must not need the worker's cooperation.
pub struct PrincipalBundle<S: ?Sized, X> {
    /// `Arc` because the native worker re-resolves the slot on every
    /// reassembly (the stale-writer heal) while owning ONE injected store — a
    /// `CredentialStore` is not clonable (its sealed arm holds live unlocked
    /// state), so the handle is shared, never forked.
    credentials: Arc<S>,
    actor_id_hex: String,
    /// The section slot read-modify-writes serialize under against sibling
    /// processes (natively the store dir's `migration.lock`).
    section: X,
    device_authorization: Mutex<Option<LoadedDeviceAuthorization>>,
    retained: Mutex<BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>>,
    /// The keys this process obtained that the carriage dropped for want of
    /// room ([`persist_retained`]'s trim). They stay in the in-memory view
    /// for the life of the process, whatever later records rebuild it from:
    /// the trim limits what survives a restart, never what this process can
    /// open. A shred drops one here too ([`Self::drop_generation_key`]).
    overflow: Mutex<BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>>,
}

impl<S: ?Sized, X: std::fmt::Debug> std::fmt::Debug for PrincipalBundle<S, X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalSlot")
            .field("actor_id_hex", &self.actor_id_hex)
            .field("section", &self.section)
            .finish_non_exhaustive()
    }
}

impl<S: SecretStore + ?Sized, X: SlotSection> PrincipalBundle<S, X> {
    /// Resolve the bundle from the slot. **Caller holds the store's
    /// migration/adoption section** — this is the same probe-then-act class
    /// as the writer key, resolved in the same place
    /// (`account_runtime`'s assembly block).
    ///
    /// Nothing here is fatal: the writer key (resolved by the caller, before
    /// this) is the only bundle item whose absence forks a store. A missing
    /// device authorization is "not enrolled yet"; a missing backup key is
    /// written from the seed-derived value; missing retained keys are an
    /// empty set (they re-enter plane-natively). Corrupt values warn and take
    /// the same recovery: re-ceremony, re-derive, re-unwrap.
    ///
    /// `derived_backup` is `None` for a **seedless** caller (W5.5's app-dead
    /// agent): it derives nothing, so it has no authoritative value to heal the
    /// slot with — its own key was read *out of* this slot moments earlier.
    /// Healing stays the seed-holding side's job, where "definitionally correct
    /// for this account" is actually true.
    ///
    /// `section` is anything the host's [`SlotSection`] converts from — the
    /// native slot passes its store dir.
    pub fn resolve(
        credentials: Arc<S>,
        actor_id_hex: String,
        section: impl Into<X>,
        writer_pub: &[u8; 32],
        derived_backup: Option<&BackupKey>,
    ) -> Self {
        let device_authorization =
            load_device_authorization(&*credentials, &actor_id_hex, writer_pub);
        if let Some(derived) = derived_backup {
            resolve_backup_key(&*credentials, &actor_id_hex, derived);
        }
        let retained = load_retained(&*credentials, &actor_id_hex);
        PrincipalBundle {
            credentials,
            actor_id_hex,
            section: section.into(),
            device_authorization: Mutex::new(device_authorization),
            retained: Mutex::new(retained),
            overflow: Mutex::new(BTreeMap::new()),
        }
    }

    /// What the slot carries right now.
    pub fn status(&self) -> PrincipalBundleStatus {
        PrincipalBundleStatus {
            device_authorization: self
                .device_authorization
                .lock()
                .expect("principal-slot mutex")
                .as_ref()
                .map(|l| l.authorization.clone()),
            retained_generations: self.retained.lock().expect("principal-slot mutex").len(),
            backup_key_persisted: self
                .credentials
                .get(&attr(&self.actor_id_hex, ATTR_BACKUP_KEY))
                .is_some(),
        }
    }

    /// The verified enrollment grant, wire included — `None` until a ceremony
    /// has run on this machine (W5.4b mints it; W5.7 consumes it as the peer
    /// leg's witness).
    pub fn device_authorization(&self) -> Option<LoadedDeviceAuthorization> {
        self.device_authorization
            .lock()
            .expect("principal-slot mutex")
            .clone()
    }

    /// The loaded grant's **canonical `EmbedAsBytes` carriage** — the exact
    /// bytes a group-plane row, a roster entry or a generation mint embeds as
    /// its `authorization`.
    ///
    /// One derivation, two consumers: the group-ceremony authority seam
    /// (`account_runtime`'s `Cmd::GroupCeremonyAuthority`) and the
    /// authority-device severance pass. `None` when this machine has run no
    /// enrollment ceremony, and also when the wire will not re-encode — the
    /// same quiet, self-healing absence in both cases, because the value heals
    /// through the ceremony and refusing the assembly over it would strand a
    /// machine that can otherwise work.
    pub fn device_authorization_carriage(&self) -> Option<Vec<u8>> {
        let loaded = self.device_authorization()?;
        match canonical_encode(&loaded.wire) {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(e) => {
                tracing::warn!(
                    "principal bundle: device-authorization carriage will not re-encode ({e}) \
                     — treating this machine as carrying no grant"
                );
                None
            }
        }
    }

    /// Persist a freshly minted enrollment grant (the ceremony's write half).
    /// Validates exactly like the loader — a wire that would not load back is
    /// refused rather than persisted — and read-back-verifies the write
    /// (`SecretStore::set` is infallible by contract; a dropped write would
    /// re-run the ceremony forever).
    pub fn store_device_authorization(
        &self,
        wire: EmbedAsBytes,
        writer_pub: &[u8; 32],
    ) -> anyhow::Result<LoadedDeviceAuthorization> {
        let loaded = validate_device_authorization(&wire, &self.actor_id_hex, writer_pub)
            .map_err(|why| anyhow::anyhow!("refusing to persist a device authorization: {why}"))?;
        let key = attr(&self.actor_id_hex, ATTR_DEVICE_AUTH);
        let encoded = hex::encode(
            canonical_encode(&wire)
                .map_err(|e| anyhow::anyhow!("device authorization encode: {e}"))?,
        );
        self.credentials.set(&key, &encoded);
        if self.credentials.get(&key).as_deref() != Some(encoded.as_str()) {
            anyhow::bail!(
                "the credential store did not retain the device authorization — \
                 the enrollment ceremony would re-run on every launch"
            );
        }
        *self
            .device_authorization
            .lock()
            .expect("principal-slot mutex") = Some(loaded.clone());
        note_slot_write();
        Ok(loaded)
    }

    /// **Which `sync_devices` row** the ceremony's nest legs last succeeded on,
    /// for the grant the slot currently carries — `None` when the grant is
    /// unregistered, or when a re-ceremony has since minted a different wire
    /// (the latch stays content-addressed; see [`ATTR_GRANT_REGISTERED`]).
    ///
    /// **The row is half the latch** — the row this machine enrolled on, which
    /// the This-device marker reads (`devices.md` § This-device marker). Since
    /// the one-credential shape (`sync-agent-credentials.md` § Credential model,
    /// the RULED 2026-09-28 block) it is always the machine's named row; the
    /// row half was added (2026-08-15) when a transitional writer-pub-hex
    /// placeholder could hold the enrollment instead, and a latch recording only
    /// *whether* the grant was registered could not tell the two apart.
    ///
    /// A value without the row half is not a shape any writer produces; it
    /// reads as unregistered, and the next owner-session pass re-registers.
    pub fn grant_registration_row(&self) -> Option<String> {
        let current = self.current_grant_encoding()?;
        let stored = self
            .credentials
            .get(&attr(&self.actor_id_hex, ATTR_GRANT_REGISTERED))?;
        match stored.split_once(LATCH_SEPARATOR) {
            Some((row, encoding)) if encoding == current => Some(row.to_string()),
            _ => None,
        }
    }

    /// Record that the ceremony's nest legs succeeded for the grant the slot
    /// currently carries, **on `device_id_hex`**. Raced by a sibling process
    /// only with the identical value (both registered the same wire on the same
    /// row — the row is derived from shared state, not chosen per process), so
    /// no lock is needed.
    pub fn record_grant_registered_on(&self, device_id_hex: &str) {
        let Some(current) = self.current_grant_encoding() else {
            tracing::warn!(
                "principal bundle: asked to record a registration with no grant in the slot"
            );
            return;
        };
        self.credentials.set(
            &attr(&self.actor_id_hex, ATTR_GRANT_REGISTERED),
            &format!("{device_id_hex}{LATCH_SEPARATOR}{current}"),
        );
        // A register the nest accepted supersedes whatever it last refused:
        // the standing notice comes down the moment the remedy lands.
        self.credentials
            .delete(&attr(&self.actor_id_hex, ATTR_ENROLLMENT_REFUSED));
    }

    /// Void the registration latch ([`PrincipalCustody::void_grant_registration`]
    /// owns why) — a fresh write to the shared credential store, so the
    /// co-located process whose device-principal client met the refusal and
    /// the runtime that re-registers need not be the same one.
    ///
    /// [`PrincipalCustody::void_grant_registration`]: crate::principal_custody::PrincipalCustody::void_grant_registration
    pub fn void_grant_registration(&self) {
        void_grant_registration(&*self.credentials, &self.actor_id_hex);
    }

    /// The nest's last refusal of this machine's enrollment, if one stands
    /// ([`ATTR_ENROLLMENT_REFUSED`]) — a fresh read off the shared credential
    /// store, so a refusal the co-located agent's pump met is visible to the
    /// app's page on its next hydrate. `None` = no refusal recorded, or a
    /// value this build does not know (a process of another build sharing the credential store wrote it; the
    /// fail-safe is to render nothing, never a guessed sentence).
    pub fn enrollment_refusal(&self) -> Option<EnrollmentRefusal> {
        let stored = self
            .credentials
            .get(&attr(&self.actor_id_hex, ATTR_ENROLLMENT_REFUSED))?;
        EnrollmentRefusal::from_slot_value(&stored)
    }

    /// The staged removal intents ([`ATTR_PENDING_FLEET_REMOVALS`]) — a fresh
    /// read, so an intent a sibling process staged is completed by whichever
    /// process pumps next.
    pub fn pending_fleet_removals(&self) -> Vec<fauna_core::fleet_removal::StagedFleetRemoval> {
        self.credentials
            .get(&attr(&self.actor_id_hex, ATTR_PENDING_FLEET_REMOVALS))
            .map(|stored| fauna_core::fleet_removal::decode_pending(&stored))
            .unwrap_or_default()
    }

    /// Read-modify-write the staged removal intents: `change` edits a fresh
    /// read and answers whether it changed anything; only then is the list
    /// written back. Serialized under [`Self::slot_write_section`] like every
    /// slot read-modify-write, so a co-located process's stage, sighting or
    /// clear is never overwritten by a stale copy. The write is **verified by
    /// read-back, decoded** — the store's `set` is infallible by signature,
    /// and an intent that silently did not persist, or that does not survive
    /// its own spelling (a row the codec cannot carry), is exactly the leak it
    /// exists to close.
    pub fn update_pending_fleet_removals(
        &self,
        change: impl FnOnce(&mut Vec<fauna_core::fleet_removal::StagedFleetRemoval>) -> bool,
    ) -> anyhow::Result<bool> {
        let _section = self.slot_write_section(
            "writing the staged device removals unserialized (a racing sibling's stage \
             or clear may be lost)",
        );
        let mut pending = self.pending_fleet_removals();
        if !change(&mut pending) {
            return Ok(false);
        }
        let key = attr(&self.actor_id_hex, ATTR_PENDING_FLEET_REMOVALS);
        if pending.is_empty() {
            self.credentials.delete(&key);
            return Ok(true);
        }
        self.credentials
            .set(&key, &fauna_core::fleet_removal::encode_pending(&pending));
        if self.pending_fleet_removals() != pending {
            anyhow::bail!("the removal intent did not persist in the credential slot");
        }
        Ok(true)
    }

    /// Record that the nest refused this machine's enrollment, so every
    /// process sharing the slot can render it. Raced by a sibling only with
    /// the identical verdict (the nest answers the same to both), so no lock.
    pub fn record_enrollment_refused(&self, refusal: EnrollmentRefusal) {
        self.credentials.set(
            &attr(&self.actor_id_hex, ATTR_ENROLLMENT_REFUSED),
            refusal.slot_value(),
        );
    }

    /// The slot encoding of the currently loaded grant wire — the same
    /// hex-over-canonical form `store_device_authorization` persists, so the
    /// latch compares byte-for-byte with what a sibling process persisted.
    fn current_grant_encoding(&self) -> Option<String> {
        let loaded = self
            .device_authorization
            .lock()
            .expect("principal-slot mutex");
        let wire = &loaded.as_ref()?.wire;
        match canonical_encode(wire) {
            Ok(bytes) => Some(hex::encode(bytes)),
            Err(e) => {
                tracing::warn!("principal bundle: grant wire re-encode failed: {e}");
                None
            }
        }
    }

    /// The retained key for `generation`, when it rode the bundle — the read
    /// side [`crate::generation_tip`] consults before (and, for a shredded
    /// mint, instead of) the plane's wraps.
    pub fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        self.retained
            .lock()
            .expect("principal-slot mutex")
            .get(generation)
            .map(|bytes| GenerationKey::from_bytes(**bytes))
    }

    /// Record a generation key this device just obtained (unwrapped from the
    /// plane, or minted). Idempotent; a same-id-different-key insert keeps
    /// the existing entry and warns — both sides were commitment-verified at
    /// obtain time, so a disagreement means one of them is not what its mint
    /// committed to, and clobbering verified custody is the losing move.
    ///
    /// Persistence is a read-merge-write **against the slot** under the
    /// store's migration lock, so two co-located processes recording
    /// different keys both survive; a degraded lock warns and writes
    /// unserialized (the W5.3 posture — a lost update here is
    /// plane-recoverable, never data loss). The in-memory view is replaced by
    /// the merged slot state, never merged back INTO it: an in-memory entry
    /// the slot no longer holds is a key a sibling process dropped on shred,
    /// and resurrecting it would defeat the crypto-shred
    /// ([`Self::drop_generation_key`]).
    pub fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        {
            let map = self.retained.lock().expect("principal-slot mutex");
            match map.get(generation) {
                Some(existing) if **existing == *key.as_bytes() => return,
                Some(_) => {
                    tracing::warn!(
                        generation = %fauna_core::hex32::encode(generation),
                        "a different key for an already-retained generation was offered — \
                         keeping the existing entry"
                    );
                    return;
                }
                None => {}
            }
        }
        let _section = self.slot_write_section(RETAINED_KEYS_UNSERIALIZED);
        let mut merged = load_retained(&*self.credentials, &self.actor_id_hex);
        merged.insert(*generation, Zeroizing::new(*key.as_bytes()));
        // `keep` = the generation this call is about: a capacity trim may drop
        // any other entry, never the one the caller just obtained. A
        // capacity-dropped key is still perfectly usable by this process, and
        // unlike a shred-drop there is nothing to defeat by holding it, so it
        // moves to the overflow and the view keeps it for the life of the
        // process. Rebuilding the view from the trimmed slot alone lost it at
        // the next record: an escrow recovery of more generations than the
        // carriage holds then left the tip unkeyed, so the pass could neither
        // seal nor re-present, the unkeyed hold stood, and the next tip-sealed
        // write minted a generation more (`account-client-lifecycle.md`
        // § Implementation status today → *the unkeyed hold*).
        let dropped = persist_retained(
            &*self.credentials,
            &self.actor_id_hex,
            &merged,
            Some(generation),
        );
        self.set_view(merged, &dropped);
    }

    /// Replace the in-memory view with `merged` (the slot's state plus what
    /// this call changed) and the overflow: first moving the entries the
    /// carriage just `dropped` for room into the overflow, and taking out of
    /// it whatever the slot carries again.
    fn set_view(&self, mut merged: BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>, dropped: &[[u8; 32]]) {
        let mut view = self.retained.lock().expect("principal-slot mutex");
        let mut overflow = self.overflow.lock().expect("principal-slot mutex");
        for id in dropped {
            if let Some(key) = merged.get(id) {
                overflow.insert(*id, key.clone());
            }
        }
        overflow.retain(|id, _| !merged.contains_key(id) || dropped.contains(id));
        for (id, key) in overflow.iter() {
            merged.entry(*id).or_insert_with(|| key.clone());
        }
        *view = merged;
    }

    /// The succession rider's key carriage (`owner-key-material.md` § Path
    /// A-sibling-2 → *Rotation* → *What crosses*, the key). A device that
    /// lives through an identity succession opens a FRESH slot under the
    /// successor's actor id (every bundle item is `<actor-id-hex>/<suffix>`),
    /// while the generation keys it held rest in the predecessor's slot on
    /// this same machine — and the home nest has already burned the
    /// predecessor's escrow wraps, so those device-held copies are the only
    /// ones left. Record every retained key found under each attested
    /// predecessor's slot here (idempotent — [`Self::record_generation_key`]'s
    /// merge; a conflicting key for an already-retained generation is
    /// refused there, not here). The predecessor's slot is left as is — its
    /// store is that identity's local-only history. Returns how many keys
    /// were new to this slot.
    ///
    /// `predecessors` is the caller's ATTESTED set
    /// (`AccountRuntimeParams::attested_predecessors`): the actor ids whose
    /// seeds this device's registry holds — possession is the attestation, so
    /// no replica-asserted list can point this read at a slot the owner never
    /// had.
    ///
    /// **Never call it while holding this slot's section.** Each key it
    /// records enters the section itself, and the native section is a `flock`
    /// on the store dir's `migration.lock`, which a second acquire from the
    /// same thread waits on for ever — the successor then never reaches ready.
    pub fn carry_predecessor_generation_keys(&self, predecessors: &[ActorId]) -> usize {
        let mut carried = 0usize;
        for predecessor in predecessors {
            let predecessor_hex = fauna_core::hex32::encode(&predecessor.0);
            if predecessor_hex == self.actor_id_hex {
                continue;
            }
            for (generation, key) in load_retained(&*self.credentials, &predecessor_hex) {
                if self.retained_generation_key(&generation).is_some() {
                    continue;
                }
                self.record_generation_key(&generation, &GenerationKey::from_bytes(*key));
                carried += 1;
            }
        }
        carried
    }

    /// Drop a shredded generation's key — the crypto-shred contract's
    /// device-side half ("deleting a generation = devices drop it"; the
    /// charter's generation-axis ruling). Removes the entry from the slot
    /// under the same serialized read-modify-write as
    /// [`Self::record_generation_key`], and from this process's view.
    /// Idempotent.
    pub fn drop_generation_key(&self, generation: &[u8; 32]) {
        let in_memory = self
            .retained
            .lock()
            .expect("principal-slot mutex")
            .contains_key(generation);
        self.overflow
            .lock()
            .expect("principal-slot mutex")
            .remove(generation);
        let _section = self.slot_write_section(RETAINED_KEYS_UNSERIALIZED);
        let mut merged = load_retained(&*self.credentials, &self.actor_id_hex);
        let in_slot = merged.remove(generation).is_some();
        if !in_memory && !in_slot {
            return;
        }
        if in_slot {
            // A drop only shrinks the set, so no trim can fire and there is
            // nothing to protect from one.
            persist_retained(&*self.credentials, &self.actor_id_hex, &merged, None);
        }
        tracing::info!(
            generation = %fauna_core::hex32::encode(generation),
            "dropped a shredded generation's retained key (crypto-shred, device-side half)"
        );
        self.set_view(merged, &[]);
    }

    /// The section every slot read-modify-write serializes under
    /// ([`SlotSection`]). Degrade is open, matching the store; `degraded`
    /// says what that costs the caller.
    fn slot_write_section(&self, degraded: &str) -> Option<X::Guard> {
        self.section.enter(degraded)
    }
}

/// The custody face the generation machinery consults
/// ([`crate::generation_tip::RetainedKeyCustody`] owns the contract).
/// The slot seam (`principal_custody`): every method forwards to the inherent
/// one, so the bundle's mechanics stay in one place and the driver reads
/// them through the trait alone.
impl<S: SecretStore + ?Sized, X: SlotSection> PrincipalCustody for PrincipalBundle<S, X> {
    fn status(&self) -> PrincipalBundleStatus {
        PrincipalBundle::status(self)
    }

    fn device_authorization(&self) -> Option<LoadedDeviceAuthorization> {
        PrincipalBundle::device_authorization(self)
    }

    fn device_authorization_carriage(&self) -> Option<Vec<u8>> {
        PrincipalBundle::device_authorization_carriage(self)
    }

    fn grant_registration_row(&self) -> Option<String> {
        PrincipalBundle::grant_registration_row(self)
    }

    fn record_grant_registered_on(&self, device_id_hex: &str) {
        PrincipalBundle::record_grant_registered_on(self, device_id_hex)
    }

    fn void_grant_registration(&self) {
        PrincipalBundle::void_grant_registration(self)
    }

    fn enrollment_refusal(&self) -> Option<EnrollmentRefusal> {
        PrincipalBundle::enrollment_refusal(self)
    }

    fn record_enrollment_refused(&self, refusal: EnrollmentRefusal) {
        PrincipalBundle::record_enrollment_refused(self, refusal)
    }

    fn pending_fleet_removals(&self) -> Vec<fauna_core::fleet_removal::StagedFleetRemoval> {
        PrincipalBundle::pending_fleet_removals(self)
    }

    fn update_pending_fleet_removals(
        &self,
        change: &mut dyn FnMut(&mut Vec<fauna_core::fleet_removal::StagedFleetRemoval>) -> bool,
    ) -> anyhow::Result<bool> {
        PrincipalBundle::update_pending_fleet_removals(self, change)
    }
}

impl<S: SecretStore + ?Sized, X: SlotSection> RetainedKeyCustody for PrincipalBundle<S, X> {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        PrincipalBundle::retained_generation_key(self, generation)
    }

    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        PrincipalBundle::record_generation_key(self, generation, key)
    }

    fn drop_generation_key(&self, generation: &[u8; 32]) {
        PrincipalBundle::drop_generation_key(self, generation)
    }
}

/// Load + verify `<hex>/device-auth`. Any failure is a warn + `None`: the
/// value heals through the enrollment ceremony (which holds the seed and can
/// re-sign), so refusing the assembly over it would violate client-state
/// recoverability for a value that is not load-bearing to open the store.
fn load_device_authorization<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
    writer_pub: &[u8; 32],
) -> Option<LoadedDeviceAuthorization> {
    let raw = credentials.get(&attr(actor_id_hex, ATTR_DEVICE_AUTH))?;
    let bytes = match hex::decode(&raw) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "the slot's device authorization is not hex ({e}) — treating as not \
                 enrolled; the next seed-holding sign-in re-runs the ceremony"
            );
            return None;
        }
    };
    let wire: EmbedAsBytes = match canonical_decode(&bytes) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(
                "the slot's device authorization does not decode ({e}) — treating as \
                 not enrolled; the next seed-holding sign-in re-runs the ceremony"
            );
            return None;
        }
    };
    match validate_device_authorization(&wire, actor_id_hex, writer_pub) {
        Ok(loaded) => Some(loaded),
        Err(why) => {
            tracing::warn!(
                "the slot's device authorization is unusable ({why}) — treating as not \
                 enrolled; the next seed-holding sign-in re-runs the ceremony"
            );
            None
        }
    }
}

/// The one validation both the loader and the ceremony's persist share: the
/// envelope verifies against the authorization's own actor (sign-over-CID,
/// root-signed), the actor is THIS account, and the covered device key is
/// THIS store's writer key — a grant over any other key cannot sign this
/// process's handshakes and would only masquerade as enrollment.
fn validate_device_authorization(
    wire: &EmbedAsBytes,
    actor_id_hex: &str,
    writer_pub: &[u8; 32],
) -> Result<LoadedDeviceAuthorization, String> {
    let (inner_bytes, env) = wire
        .clone()
        .into_signed()
        .map_err(|e| format!("envelope split: {e}"))?;
    let authorization: DeviceAuthorization =
        decode_signed_bytes(&inner_bytes).map_err(|e| format!("inner decode: {e}"))?;
    verify_envelope(&authorization, &inner_bytes, &env)
        .map_err(|e| format!("signature verify: {e}"))?;
    if fauna_core::hex32::encode(&authorization.actor_id.0) != actor_id_hex {
        return Err("the authorization names a different account".to_string());
    }
    if authorization.device_key != *writer_pub {
        return Err(
            "the authorization covers a different device key than this store's \
                    writer key"
                .to_string(),
        );
    }
    Ok(LoadedDeviceAuthorization {
        authorization,
        wire: wire.clone(),
    })
}

/// Load `<hex>/backup-key` — the **seedless** side of [`resolve_backup_key`],
/// and the reason that carriage exists at all (W5.4a persisted it "for the
/// seedless consumer (W5.5's app-dead agent)").
///
/// The app-dead sync agent hosting the account-store runtime derives nothing:
/// it holds no identity seed, so this slot value *is* its `BackupKey` — the
/// account-state key schedule and every sealed read it
/// performs come from here (`account-data-plane.md` § The store device
/// principal, T10).
///
/// `None` means the machine has never completed a seed-holding assembly for
/// this account, which is exactly the case a seedless host must refuse to
/// start on rather than proceed with a fabricated key: an agent that guessed
/// would write ciphertext no app in the fleet could open. Corrupt (non-hex or
/// wrong length) reads as absent, loudly — same recovery, a seed-holding app
/// re-persists it on its next assembly.
pub fn load_backup_key<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> Option<BackupKey> {
    let raw = read_secret_slot_value(credentials, &attr(actor_id_hex, ATTR_BACKUP_KEY))?;
    let mut bytes = Zeroizing::new([0u8; 32]);
    match hex::decode_to_slice(raw.as_bytes(), bytes.as_mut()) {
        Ok(()) => Some(BackupKey::from_bytes(*bytes)),
        Err(e) => {
            tracing::warn!(
                "the slot's backup key is unreadable ({e}) — a seedless host cannot serve \
                 this account until a signed-in app re-persists it"
            );
            None
        }
    }
}

/// **Load-only** read of the store principal's signing key — the writer key at
/// the slot's root attribute, which is also the machine's device key
/// (`account-data-plane.md` § The store device principal, T10/T11: one Ed25519
/// device keypair per (machine, account), and the writer key *is* it).
///
/// # Why this exists beside `resolve_writer_key_serialized`
///
/// That function is **mint-or-load**: an absent slot makes it generate a writer
/// identity. That is exactly right at assembly, where minting is the enrollment,
/// and exactly wrong for every *consumer* that merely wants to authenticate as
/// the principal — the sync agent's bearer-renewal loop above all (T11's
/// convergence). A renewal loop that minted would create a writer identity no
/// nest has a grant for, so every renewal under it would fail; worse, it would
/// mint an identity for a store the app has not enrolled yet, which is the
/// second-writer-key divergence W5.3 measured (the loser's store refuses its own
/// key forever). **A consumer authenticates as a principal that already exists,
/// or it does not authenticate at all** — hence `Option`, never a mint.
///
/// `None` means no signed-in app has enrolled this machine for this account yet.
/// Callers must treat that as "no candidate" — there is no other renewal
/// credential to fall back to — never as a reason to create one.
///
/// ⚠ **The converse does NOT hold, and a caller must not read it in** (finding
/// ): `Some` means only that a writer key was *minted* — either by a
/// completed enrollment, or by this very slot's own mint-or-load path
/// ([`mint_or_load_writer_key`]) having run with nobody's grant registered yet. The
/// mint always precedes the grant. A caller that needs "this machine's
/// principal is actually registered on a nest" — the sync agent's
/// principal-support advertisement, above all — must additionally consult
/// [`PrincipalBundle::grant_registration_row`], never treat this function's
/// `Some` alone as enrollment evidence.
pub fn load_writer_key<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> Option<ed25519_dalek::SigningKey> {
    let raw = read_secret_slot_value(credentials, actor_id_hex)?;
    let mut bytes = Zeroizing::new([0u8; 32]);
    match hex::decode_to_slice(raw.as_bytes(), bytes.as_mut()) {
        Ok(()) => Some(ed25519_dalek::SigningKey::from_bytes(&bytes)),
        Err(e) => {
            tracing::warn!(
                "the slot's writer key is unreadable ({e}) — a consumer that wanted to \
                 authenticate as the store principal will fall back rather than mint one"
            );
            None
        }
    }
}

/// Which of its two arms [`mint_or_load_writer_key`] took. The journal-bound
/// writer's heal (`account-replica-posture.md` § The store device principal,
/// refinement 11) keys on it: it tells a first launch (the key this assembly
/// minted a moment ago, over the fresh store its open will stamp) from the
/// inverse shape (a key LOADED from the slot over a store with no stamped
/// writer, whose history that journal cannot vouch for). The mint-or-load
/// resolver is the only honest source of it; nothing about the key bytes says
/// which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterKeyProvenance {
    /// Minted into an empty slot by this very assembly.
    Minted,
    /// Read out of a slot that already held it — a previous assembly's mint.
    Loaded,
}

/// **Mint-or-load** the store's writer signing key — this machine's device
/// principal — at the slot's root attribute, saying which it did. Every host's
/// assembly resolves its writer through here: natively inside the store dir's
/// migration section (`fauna_sync_engine::account_runtime`), on web inside
/// `web_host`'s assembly (one tab; § [`SlotSection`] says why web's section is
/// the tab's own).
///
/// **Corrupt-slot policy is refuse, never re-mint**: an unreadable value may
/// still be a recoverable key, and a silent re-mint would fork the store's
/// writer identity. A fresh mint writes the unstamped marker FIRST
/// ([`mark_writer_unstamped`] says why) and reads the key back — a secret store
/// may silently drop a write (`SecretStore::set` is infallible by contract),
/// and a key that did not persist would re-mint on the next launch, exactly
/// the divergence the store's identity check then refuses.
pub fn mint_or_load_writer_key<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> anyhow::Result<(ed25519_dalek::SigningKey, WriterKeyProvenance)> {
    if let Some(hex_value) = read_secret_slot_value(credentials, actor_id_hex) {
        let mut secret = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(hex_value.as_bytes(), secret.as_mut()).map_err(|e| {
            anyhow::anyhow!(
                "account runtime: the credential slot for this account holds an unreadable \
                 writer key ({e}) — refusing to overwrite a value that may still be a \
                 recoverable key. Recovery: restore the credential slot; or clear the slot \
                 entry — an EMPTY slot over this store self-heals at the next launch (the \
                 store's writer is retired and its unpublished local rows are re-authored \
                 under a fresh writer; nothing is deleted)."
            )
        })?;
        return Ok((
            ed25519_dalek::SigningKey::from_bytes(&secret),
            WriterKeyProvenance::Loaded,
        ));
    }
    // Fresh mint. `ActorKeypair::generate` is the crate-local source of 32
    // fresh random bytes (an Ed25519 seed is exactly that); the keypair
    // wrapper is dropped and zeroized immediately.
    let minted = fauna_core::identity::ActorKeypair::generate();
    let key = ed25519_dalek::SigningKey::from_bytes(minted.secret_bytes());
    mark_writer_unstamped(credentials, actor_id_hex);
    credentials.set(actor_id_hex, &hex::encode(minted.secret_bytes()));
    note_slot_write();
    if credentials.get(actor_id_hex).is_none() {
        anyhow::bail!(
            "account runtime: the credential store did not retain the writer key — refusing \
             to open the store with a key that would be lost on restart"
        );
    }
    tracing::info!("account runtime: minted this machine's store writer key");
    Ok((key, WriterKeyProvenance::Minted))
}

/// The principal writer key and the grant certifying it, when the grant
/// carries `SyncWrite` — the one load both signer shapes below share.
fn load_sync_write_principal<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id: &[u8; 32],
) -> Option<(ed25519_dalek::SigningKey, LoadedDeviceAuthorization)> {
    let actor_id_hex = hex::encode(actor_id);
    let key = load_writer_key(credentials, &actor_id_hex)?;
    let loaded =
        load_device_authorization(credentials, &actor_id_hex, &key.verifying_key().to_bytes())?;
    let grants_sync_write = loaded.authorization.capabilities.iter().any(|c| {
        matches!(
            c,
            fauna_core::data::Capability::SyncWrite | fauna_core::data::Capability::All
        )
    });
    if !grants_sync_write {
        tracing::warn!(
            "this machine's enrollment grant does not carry SyncWrite — its change records \
             go out unsigned until the next seed-holding sign-in re-certifies it"
        );
        return None;
    }
    Some((key, loaded))
}

/// This machine's **change-record signer** for `actor_id`, load-only: the
/// principal writer key paired with the root-signed `DeviceAuthorization`
/// that certifies it — the one delegated signer every engine host signs its
/// change records with (`mls-group-key-material.md` § M2 → *Writer-signed
/// change records* (1)). `None` when either half is absent or the grant does
/// not carry `SyncWrite` (a `[RenewBearer]`-only enrollment the next
/// seed-holding sign-in re-certifies): the caller records unsigned, loudly,
/// and never mints — the same contract as [`load_writer_key`].
pub fn load_change_signer<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id: &[u8; 32],
) -> Option<fauna_protocol::sync_writer_sig::ChangeSigner> {
    let (key, loaded) = load_sync_write_principal(credentials, actor_id)?;
    Some(fauna_protocol::sync_writer_sig::ChangeSigner::delegated(
        *actor_id,
        key,
        loaded.wire,
    ))
}

/// The same signer as [`load_change_signer`], as the **carriage** a capability
/// host that mounts no slot is provisioned with (ruling (1), *The capability
/// host* — the apple File Provider extension): the writer secret and the
/// grant's canonical `EmbedAsBytes` bytes, which
/// [`fauna_protocol::sync_writer_sig::ChangeSigner::from_delegated_carriage`]
/// rebuilds and re-checks on the host's side. Load-only, `None` exactly when
/// [`load_change_signer`] is.
pub fn load_change_signer_carriage<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id: &[u8; 32],
) -> Option<ChangeSignerCarriage> {
    let (key, loaded) = load_sync_write_principal(credentials, actor_id)?;
    let device_authorization = match canonical_encode(&loaded.wire) {
        Ok(bytes) => bytes.to_vec(),
        Err(e) => {
            tracing::warn!("the slot's device authorization will not re-encode ({e})");
            return None;
        }
    };
    Some(ChangeSignerCarriage {
        writer_secret: Zeroizing::new(key.to_bytes()),
        device_authorization,
    })
}

/// A delegated change signer in its provisioned form — see
/// [`load_change_signer_carriage`].
pub struct ChangeSignerCarriage {
    /// The principal writer key's secret.
    pub writer_secret: Zeroizing<[u8; 32]>,
    /// The canonical `EmbedAsBytes` encoding of its `DeviceAuthorization`.
    pub device_authorization: Vec<u8>,
}

/// Every write to a slot's writer key, grant or retained generation keys in
/// THIS process bumps this counter — the only in-process signal that a
/// provisioned copy went stale: the signer ([`load_change_signer_carriage`]:
/// a first enrollment, the `SyncWrite` re-certification, a succession or
/// lost-slot re-mint) or the retained-key carriage an out-of-process
/// capability host is mirrored ([`retained_generation_keys`]: a generation
/// recovered from escrow, unwrapped, carried or shredded). A host that
/// provisions a capability host re-reads the slot when it moves.
static SLOT_WRITES: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::channel(0).0);

/// Subscribe to [`SLOT_WRITES`].
pub fn slot_writes() -> tokio::sync::watch::Receiver<u64> {
    SLOT_WRITES.subscribe()
}

/// Record a write to a slot's carried state (see [`SLOT_WRITES`]) —
/// public because the writer key's mint-or-load is the host's
/// (`fauna_sync_engine::account_runtime` natively).
pub fn note_slot_write() {
    SLOT_WRITES.send_modify(|n| *n = n.wrapping_add(1));
}

/// The retained-key carriage `<hex>/generation-keys` as it rests now, read
/// fresh — every (generation, key) this machine's slot carries. What the app
/// mirrors into an out-of-process capability host's own custody on every
/// slot write ([`note_slot_write`]); empty for an absent or corrupt carriage.
pub fn retained_generation_keys<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> Vec<([u8; 32], Zeroizing<[u8; 32]>)> {
    load_retained(credentials, actor_id_hex)
        .into_iter()
        .collect()
}

/// A slot's retained-key carriage as a [`RetainedKeyCustody`] of its own, for
/// a process that shares the machine's slot but holds no bundle — a
/// **capability host** in the app's process (android's SAF provider;
/// `on-demand-files.md` § Shared sets on a capability host, decision 1′: the
/// slot's custody is the host's, and its own unwraps are recorded there
/// exactly as the runtime's are).
///
/// Unlike [`PrincipalBundle`], which loads the carriage once at assembly and
/// keeps it in memory, every consult here reads the slot fresh: the host is
/// long-lived beside a runtime that may record a generation it recovered from
/// escrow at any time, and that key is on no wrap the host could open itself.
/// Records and drops are the bundle's own read-merge-write under the same
/// [`SlotSection`] (so a record never clobbers the runtime's), trimmed the same
/// way.
pub struct RetainedKeyCarriage<S: SecretStore + ?Sized, X: SlotSection> {
    credentials: Arc<S>,
    actor_id_hex: String,
    section: X,
}

impl<S: SecretStore + ?Sized, X: SlotSection> RetainedKeyCarriage<S, X> {
    /// The carriage of `actor_id_hex`'s slot in `credentials`, its writes
    /// serialized under `section` — natively the account store dir's, the
    /// same section the runtime's bundle writes under.
    pub fn over(credentials: Arc<S>, actor_id_hex: String, section: impl Into<X>) -> Self {
        Self {
            credentials,
            actor_id_hex,
            section: section.into(),
        }
    }
}

impl<S: SecretStore + ?Sized, X: SlotSection> RetainedKeyCustody for RetainedKeyCarriage<S, X> {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        load_retained(&*self.credentials, &self.actor_id_hex)
            .get(generation)
            .map(|key| GenerationKey::from_bytes(**key))
    }

    /// The bundle's record rule: an equal entry is a no-op, a different key
    /// for a held generation keeps the held one.
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        let _section = self.section.enter(RETAINED_KEYS_UNSERIALIZED);
        let mut merged = load_retained(&*self.credentials, &self.actor_id_hex);
        if let Some(existing) = merged.get(generation) {
            if **existing != *key.as_bytes() {
                tracing::warn!(
                    generation = %fauna_core::hex32::encode(generation),
                    "a different key for an already-retained generation was offered — \
                     keeping the existing entry"
                );
            }
            return;
        }
        merged.insert(*generation, Zeroizing::new(*key.as_bytes()));
        persist_retained(
            &*self.credentials,
            &self.actor_id_hex,
            &merged,
            Some(generation),
        );
    }

    fn drop_generation_key(&self, generation: &[u8; 32]) {
        let _section = self.section.enter(RETAINED_KEYS_UNSERIALIZED);
        let mut merged = load_retained(&*self.credentials, &self.actor_id_hex);
        if merged.remove(generation).is_some() {
            persist_retained(&*self.credentials, &self.actor_id_hex, &merged, None);
        }
    }
}

/// Mint-or-load for `<hex>/backup-key`, seed-holding side: absent → persist
/// the derived value; present-and-equal → nothing; present-and-different →
/// **overwrite with the derived value, loudly** (the doc-comment asymmetry:
/// the derivation is definitionally correct for this account's seed, and a
/// wrong slot value would hand W5.5's seedless agent a key that unseals
/// nothing). A dropped write warns rather than fails — this process holds the
/// derived value and loses nothing; only the future seedless consumer is
/// short-changed, and the next assembly retries.
fn resolve_backup_key<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
    derived: &BackupKey,
) {
    let key = attr(actor_id_hex, ATTR_BACKUP_KEY);
    let derived_hex = Zeroizing::new(hex::encode(derived.to_bytes()));
    match read_secret_slot_value(credentials, &key) {
        Some(stored) if stored.as_str() == derived_hex.as_str() => return,
        Some(_) => {
            tracing::warn!(
                "the slot's backup key does not match the one this account's seed \
                 derives — overwriting with the derived value (a seedless process \
                 reading the old value could unseal nothing)"
            );
        }
        None => {}
    }
    credentials.set(&key, &derived_hex);
    let read_back = read_secret_slot_value(credentials, &key);
    if read_back.as_ref().map(|s| s.as_str()) != Some(derived_hex.as_str()) {
        tracing::warn!(
            "the credential store did not retain the backup key — carriage for the \
             app-dead agent (W5.5) is missing until a later assembly persists it"
        );
    }
}

/// Load `<hex>/generation-keys`. Absent → empty; corrupt → warn + empty (the
/// keys re-enter plane-natively through the wraps, and — unlike the writer
/// key — nothing refuses a re-obtained generation key).
fn load_retained<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
) -> BTreeMap<[u8; 32], Zeroizing<[u8; 32]>> {
    // `Zeroizing` for the same reason `resolve_backup_key` wraps its read: this
    // string is the hex of *every* retained generation key, the widest-reaching
    // key material the slot holds, and a bare `String` drops it into freed heap
    // unscrubbed.
    let Some(raw) = read_secret_slot_value(credentials, &attr(actor_id_hex, ATTR_GENERATION_KEYS))
    else {
        return BTreeMap::new();
    };
    let bytes = match hex::decode(raw.as_bytes()) {
        Ok(b) => Zeroizing::new(b),
        Err(e) => {
            tracing::warn!(
                "the slot's retained generation keys are not hex ({e}) — starting \
                 empty; keys re-enter from the plane's wraps"
            );
            return BTreeMap::new();
        }
    };
    let record: RetainedGenerationKeysRecord = match canonical_decode(&bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "the slot's retained generation keys do not decode ({e}) — starting \
                 empty; keys re-enter from the plane's wraps"
            );
            return BTreeMap::new();
        }
    };
    let map = record
        .entries
        .iter()
        .map(|e| (e.generation, Zeroizing::new(e.key)))
        .collect();
    drop(Zeroizing::new(record)); // zeroize the decoded key bytes
    map
}

/// Persist the merged retained set, read-back-verifying like every other
/// bundle write (warn-only: plane-recoverable).
///
/// **Bounded before it is written.** The set has no natural ceiling — R14 asks
/// for "current tip + every generation still held for reading"
/// (`owner-key-material.md` § Path A-sibling-2 → *bundle carriage*), and the
/// only shrinking force is a `Shredded` observation — while the value it
/// serializes into is ONE credential item, which
/// [`MAX_SLOT_VALUE_BYTES`] caps on every backend. So
/// this trims to fit rather than handing the store a value it would silently
/// refuse: entries are dropped in generation-id order (deterministic, and
/// arbitrary — see below) until the encoded value fits, never dropping `keep`,
/// the entry whose obtain motivated this write.
///
/// The bound is checked against the **encoded length**, not an entry count, so
/// it holds regardless of how the record encodes: each entry is two 32-byte
/// ids, each a 34-byte CBOR byte string (`serialization.md` § "Fixed-size
/// byte arrays"), so the ceiling lands at fifteen generations (it was near
/// nine while serde wrote them as integer arrays).
/// The exact figure is measured, not asserted, by
/// `the_retained_carriage_is_bounded_by_the_credential_item_cap`.
///
/// Dropping in id order is **not** a retention policy — generation ids are
/// content-derived hashes, so the order is meaningless, and every retained
/// generation is by definition still readable. A real policy needs a rotation
/// cadence to size it, and today only triggers (a) first-need and (d)
/// succession mint: (b) removal-observed and (c) the cadence constant are
/// owed (`owner-key-material.md` § Implementation status today). The cap is
/// therefore meant to be unreachable in shipped behavior, and **whoever builds
/// (c) owns the retention question** — this trim exists so that the day it
/// becomes reachable it degrades loudly and recoverably instead of silently,
/// not to pre-empt that design. It was reached on a remote box by an account
/// signed in afresh on every launch (2026-10-05): the trim then must not cost
/// the process its keys, so the ids it dropped are returned and the bundle
/// keeps those keys in memory ([`PrincipalBundle::record_generation_key`]).
fn persist_retained<S: SecretStore + ?Sized>(
    credentials: &S,
    actor_id_hex: &str,
    retained: &BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>,
    keep: Option<&[u8; 32]>,
) -> Vec<[u8; 32]> {
    let mut dropped = Vec::new();
    let mut carried = retained.clone();
    let encoded = loop {
        let record = RetainedGenerationKeysRecord {
            entries: carried
                .iter()
                .map(|(id, key)| RetainedGenerationKeyEntry {
                    generation: *id,
                    key: **key,
                })
                .collect(),
        };
        let encoded = match canonical_encode(&record) {
            Ok(b) => Zeroizing::new(hex::encode(Zeroizing::new(b))),
            Err(e) => {
                tracing::warn!("retained generation keys encode failed ({e}) — not persisted");
                drop(Zeroizing::new(record));
                // Nothing reached the slot: the process keeps every key.
                return retained.keys().copied().collect();
            }
        };
        drop(Zeroizing::new(record));
        if encoded.len() <= MAX_SLOT_VALUE_BYTES {
            break encoded;
        }
        let victim = carried
            .keys()
            .find(|id| keep != Some(*id))
            .copied()
            .expect("a single retained entry cannot exceed the item cap");
        carried.remove(&victim);
        dropped.push(victim);
        tracing::warn!(
            generation = %fauna_core::hex32::encode(&victim),
            retained_after = carried.len(),
            "the retained generation keys no longer fit one credential item — dropped \
             this generation from the slot's carriage. The key is NOT gone: this \
             process keeps it in memory, and any device re-obtains it plane-natively \
             (mint wraps / top-up). What is lost is the offline bridge for a store \
             re-syncing from scratch, and for W5.5's seedless agent, which has no \
             other source"
        );
    };
    let key = attr(actor_id_hex, ATTR_GENERATION_KEYS);
    credentials.set(&key, &encoded);
    // The mirror an out-of-process capability host is provisioned from moves
    // with the carriage ([`retained_generation_keys`]).
    note_slot_write();
    let read_back = read_secret_slot_value(credentials, &key);
    if read_back.as_ref().map(|s| s.as_str()) != Some(encoded.as_str()) {
        // Deliberately not "until the next obtain re-persists it": the next
        // obtain re-runs this same write with a set that is the same size or
        // larger, so nothing about repetition heals it. It stays missing until
        // the set shrinks (a shred) or the slot is re-enrolled.
        tracing::warn!(
            "the credential store did not retain the generation keys — the carriage \
             is missing and stays missing (re-writing the same value cannot heal it); \
             this process keeps its in-memory keys, and other devices re-obtain them \
             plane-natively"
        );
        return retained.keys().copied().collect();
    }
    dropped
}

/// The carriage's at-rest spellings, for the native slot's tests
/// (`fauna_sync_engine::principal_bundle`'s suite measures the slot itself,
/// over the file-backend credential store). Never a consumer surface: the
/// attribute names are this module's alone.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support {
    pub use super::{RetainedGenerationKeyEntry, RetainedGenerationKeysRecord};

    pub const ATTR_DEVICE_AUTH: &str = super::ATTR_DEVICE_AUTH;
    pub const ATTR_BACKUP_KEY: &str = super::ATTR_BACKUP_KEY;
    pub const ATTR_GENERATION_KEYS: &str = super::ATTR_GENERATION_KEYS;
    pub const ATTR_ENROLLMENT_REFUSED: &str = super::ATTR_ENROLLMENT_REFUSED;

    /// `<actor_id_hex>/<suffix>` — the attribute key shape.
    pub fn attr(actor_id_hex: &str, suffix: &str) -> String {
        super::attr(actor_id_hex, suffix)
    }
}
