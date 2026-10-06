//! Backup encryption key derivation and chunk encryption.
//!
//! This module provides the two seed-derived backup keys of
//! `docs/goal/architecture/key-material-hierarchy.md`, distinguished by **who is
//! allowed to hold them**:
//!
//! - [`BackupKey`] (§ Path A) — seals **client-originated** data (ordinary file
//!   sets, `__drafts`, library media,
//!   held-for-friends). **Never nest-held, unconditionally.** Also carries the
//!   legacy framed `encrypt_backup_chunk` / `decrypt_backup_chunk` AEAD used for
//!   whole-blob seals outside the chunk route.
//! - [`NestBackupKey`] (§ Path A-sibling-0) — seals cross-location backups of the
//!   **nest-originated message kinds** (`__mail`, `__conv/<channel>`, `__post`,
//!   `__calendar`, `__card`). The user's client grants it to **their own source
//!   nest**, whose in-process coordinator seals its own local segment files
//!   before uploading them to destination nests. Sound because that nest already
//!   hosts and floor-reads those segments — co-locating a key with the data it
//!   seals adds no exposure; the key's entire marginal value is blinding the
//!   *destination* (metadata floor included).
//!
//! The two are independent BLAKE3 `derive_key` outputs over distinct context
//! strings, so a source nest holding `NestBackupKey` learns nothing about the
//! owner's `BackupKey`-sealed data. Both expose a domain-separated
//! `convergent_chunk_root()` for the deterministic chunk seal the nest chunk
//! route's F9 anti-poisoning check requires.
//!
//! This is intentionally separate from `chunk_crypto`, which derives per-chunk
//! keys + deterministic nonces from the chunk content hash (convergent
//! encryption) under an MLS group's epoch secret for cross-user shared folders.

use anyhow::{Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use zeroize::{Zeroize, Zeroizing};

/// Version byte prepended to every ciphertext produced by this module.
const VERSION: u8 = 0x01;

/// Nonce length for ChaCha20-Poly1305.
const NONCE_LEN: usize = 12;

/// A 256-bit symmetric backup encryption key derived from the user's identity.
///
/// The key is deterministic — given the same Ed25519 seed it always produces
/// the same `BackupKey`, so no separate key file or password prompt is needed.
#[derive(Clone)]
pub struct BackupKey([u8; 32]);

impl BackupKey {
    /// Derive a `BackupKey` from a 32-byte Ed25519 seed.
    ///
    /// Uses BLAKE3 key derivation with a domain-separated context string so the
    /// backup key is cryptographically independent from the signing key even
    /// though both come from the same seed.
    pub fn derive(ed25519_seed: &[u8; 32]) -> BackupKey {
        let key_bytes = blake3::derive_key("fauna backup encryption key 2026-03-12", ed25519_seed);
        BackupKey(key_bytes)
    }

    /// Construct a `BackupKey` directly from raw bytes (useful in tests).
    pub fn from_bytes(bytes: [u8; 32]) -> BackupKey {
        BackupKey(bytes)
    }

    fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Return the raw bytes of this key by value.
    ///
    /// Mirrors [`BackupKey::from_bytes`] — the two are inverse operations.
    /// Use this to transfer the key over a channel (e.g. FFI) where an owned
    /// `[u8; 32]` is more convenient than a reference.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The 32-byte `chunk_crypto` root for the owner's **convergent** backup-chunk
    /// seal (`mls-group-key-material.md` § M2 *At-rest blob keying*, FS-BIND
    /// FOLLOW-ON A — ratified by the user 2026-07-07).
    ///
    /// Backup chunks uploaded through the nest's content-addressed chunk route
    /// must present a body whose BLAKE3 hash equals their store key (the F9
    /// anti-poisoning check), so the ciphertext must be a *stable* content
    /// address — which the random-nonce [`encrypt_backup_chunk`] frame is not.
    /// Deriving a domain-separated root and sealing through the deterministic
    /// `chunk_crypto` primitive (key + nonce from the chunk's plaintext hash)
    /// makes identical plaintext seal to identical ciphertext under one owner,
    /// preserving destination-side dedup and idempotent retry. The accepted
    /// privacy property: the backup destination (the owner's own other nest, or
    /// a held-for-friends holder) learns which of *this owner's* chunks are
    /// equal — nothing more (the root is secret, so a destination holding a
    /// candidate plaintext cannot confirm the backup contains it, and equality
    /// never leaks across owners).
    ///
    /// Domain-separated from the raw AEAD use of the key by BLAKE3 `derive_key`
    /// (and `chunk_crypto` derives again under `"fauna.chunk.v1"`), so this root
    /// is cryptographically independent of both the `BackupKey` itself and any
    /// M2 content key. The context string is the one the frozen decision review
    /// ratified (tracked internally — never the raw key, never M2's
    /// `"fauna.chunk.v1"`).
    pub fn convergent_chunk_root(&self) -> [u8; 32] {
        blake3::derive_key("fauna.backup.chunk.v1", &self.0)
    }
}

impl Drop for BackupKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A 256-bit symmetric key sealing cross-location backups of the **nest-originated
/// message kinds** — `key-material-hierarchy.md` § Path A-sibling-0, ratified
/// 2026-07-23.
///
/// Derived from the same Ed25519 identity seed as [`BackupKey`] but under a
/// distinct BLAKE3 context, and **deliberately delegated**: the user's client
/// grants it to their own source nest at first destination-enroll (revocable,
/// surfaced in the nests-page trust facet), and the nest's in-process backup
/// coordinator seals its own local segment files under it before uploading to
/// each destination. That is what makes backup freshness independent of any user
/// device being awake — the motivating property of the redesign.
///
/// Why delegating this key is sound while [`BackupKey`] stays never-nest-held:
/// the segments it seals are the source nest's *own* data, which it hosts and
/// floor-reads by design (record payloads inside are already sealed by each
/// kind's ingest authority). Handing it over reveals nothing the nest does not
/// already have; the key's only job is blinding the **destination**, framing and
/// metadata floor included.
///
/// Deliberately **symmetric, not asymmetric**: the sealer holds the plaintext by
/// construction, so encrypt-to-public earns nothing, and convergent
/// (deterministic) sealing preserves idempotent retries + per-owner dedup.
/// Deliberately **static, no rotation**: epoch/rotation schemes bound *delegates*
/// whose access should be windowable, but this seal must open the owner's whole
/// back-catalogue forever, and the nest cannot be windowed away from data it
/// hosts anyway.
///
/// A compromised source nest gains no *read* power from holding this (it hosts
/// the data regardless); what it gains is destination *write/supersede* power,
/// bounded by the destination-side custody grace window (T = 30 d) plus the
/// seed-holding clients' audit loop — `message-segment-store.md` § Cross-location
/// backup protocol.
#[derive(Clone)]
pub struct NestBackupKey([u8; 32]);

impl NestBackupKey {
    /// Derive a `NestBackupKey` from a 32-byte Ed25519 seed.
    ///
    /// The context string is ratified in `key-material-hierarchy.md`
    /// § Path A-sibling-0 and pinned by a known-answer test. Editing it silently
    /// breaks every already-uploaded backup, so it is frozen for the life of the
    /// key.
    pub fn derive(ed25519_seed: &[u8; 32]) -> NestBackupKey {
        NestBackupKey(blake3::derive_key(
            "fauna nest backup key 2026-07-23",
            ed25519_seed,
        ))
    }

    /// Construct a `NestBackupKey` directly from raw bytes.
    ///
    /// This is also the **grant-receiving** constructor: the source nest never
    /// derives the key (it holds no seed) — it receives the 32 bytes the owner's
    /// client granted it and reconstructs the key here.
    pub fn from_bytes(bytes: [u8; 32]) -> NestBackupKey {
        NestBackupKey(bytes)
    }

    /// Return the raw bytes of this key by value — the wire form of the grant,
    /// and the FFI form for clients handing it to the enroll flow.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The 32-byte `chunk_crypto` root for the **convergent** seal every
    /// nest-originated segment chunk is uploaded under.
    ///
    /// Mirrors [`BackupKey::convergent_chunk_root`] exactly in shape and purpose
    /// — a stable content address, so the destination's F9 anti-poisoning check
    /// (`blake3(body) == store key`) accepts these chunks through the *same*
    /// route with no new arm, and identical plaintext dedups per owner across
    /// idempotent retries. Domain-separated by its own context string, so a nest
    /// holding this root can neither derive nor confirm equality of the owner's
    /// `BackupKey`-sealed client-originated chunks.
    pub fn convergent_chunk_root(&self) -> [u8; 32] {
        blake3::derive_key("fauna.nest-backup.chunk.v1", &self.0)
    }
}

impl Drop for NestBackupKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Domain-separation context for the **content-index master key** —
/// `key-material-hierarchy.md` § Path A-sibling (derivation ratified
/// 2026-08-04), `content-index.md` § Encryption posture. Versioned + dated per
/// that doc's rule #3, and pinned by a known-answer test: editing it silently
/// orphans every master-class index segment at rest, so it is frozen for the
/// life of generation 0. A future rotation slice mints generation N as its own
/// frozen context string rather than editing this one — concurrent rotations
/// to the same generation then derive identical bytes, so no fleet race can
/// fork the key.
pub const INDEX_MASTER_KEY_DERIVE_CONTEXT: &str = "fauna.index.master.v1 2026-08-04";

/// Derive the user's 32-byte **content-index master key** (generation 0) from
/// their Ed25519 identity seed — the third seed-derived sibling beside
/// [`BackupKey`] and [`NestBackupKey`], independent of both by context-string
/// domain separation, so rotating it (a future generation bump + rewrap pass)
/// invalidates only index segments and never a `BackupKey`-sealed corpus.
///
/// Deterministic on purpose: every seed-holding client derives the same key
/// with nothing stored and nothing synced — which is exactly the ratified
/// audience ("every one of the user's clients holds the index master key"),
/// rules out the two-device first-login mint race a random-minted key would
/// carry through a plane merge, and survives total device loss with no
/// escrow beyond the seed's own. The MDA bridge and the nest hold no seed, so
/// rule #7's boundary (master key unreachable from a MUA credential;
/// never nest-held) holds structurally rather than by handling discipline.
///
/// Consumed opaquely by `fauna_index::IndexMasterKey::from_bytes` — this
/// function is the derivation that crate's docs defer to, kept here beside
/// its seed-derived siblings (the `derive_index_segment_key` /
/// `fauna-mls` split is the same shape for the MSEK-derived key class).
/// # Custody of the returned bytes
///
/// The return type is [`Zeroizing`] rather than a bare `[u8; 32]`, and that is
/// a **mechanism, not a style choice**. A bare array is `Copy`, so it is
/// silently duplicated on every assignment and pass-by-value and is never
/// zeroized; `Zeroizing` is neither (`Copy` and `Drop` are mutually exclusive
/// in Rust), so the derivation's own copy of the key cannot leak implicit
/// duplicates and dies zeroized. A later session must not "simplify" this back
/// to `[u8; 32]` believing the property was cosmetic — the pins directly below
/// exist to stop exactly that.
///
/// The **custody type for this key is `fauna_index::IndexMasterKey`**, not a
/// second newtype here: `fauna-index` cannot depend on `fauna-core` (its
/// dependency list is curated to keep the wasm build possible at all), so a
/// `fauna-core` newtype would mean two custody types for one key with a bare
/// `[u8; 32]` hop between them via `to_bytes()` — the very gap it would claim
/// to close. `Zeroizing` closes the window without inventing that second type,
/// and applies identically to the MSEK-derived sibling
/// (`fauna_mls::wrapped_blob::derive_index_segment_key`), so the index key
/// family keeps one shape. Ratified in `key-material-hierarchy.md`
/// § Path A-sibling → *Custody of the derived bytes*.
#[must_use]
pub fn derive_index_master_key(ed25519_seed: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(blake3::derive_key(
        INDEX_MASTER_KEY_DERIVE_CONTEXT,
        ed25519_seed,
    ))
}

/// Compile-time pin: [`derive_index_master_key`] returns a non-`Copy`,
/// zeroize-on-drop type.
///
/// **Named mutation** — revert the return type to `[u8; 32]` and the
/// build must fail here with a type mismatch. That is the whole pin: the
/// non-`Copy`, zeroize-on-drop property is `Zeroizing`'s own (it implements
/// `Drop`, and `Copy` + `Drop` are mutually exclusive), so pinning the
/// signature to that exact type is what carries the property forward. A
/// runtime test cannot observe an absent `Drop`, so compile-time is the only
/// honest pin.
///
/// **Do not "strengthen" this with the conflicting-impl trick**
/// (`trait NotCopy {} impl<T: Copy> NotCopy for T {} impl NotCopy for
/// Zeroizing<[u8; 32]> {}`) — it was tried and does not compile. That trick
/// needs a **local** type; against a foreign one rustc's coherence rejects the
/// second impl outright (E0119, "upstream crates may add a new impl of trait
/// `Copy` … in future versions"), so it fails whether or not the type is
/// `Copy` and pins nothing.
const _INDEX_MASTER_KEY_IS_NOT_COPY: fn(&[u8; 32]) -> Zeroizing<[u8; 32]> = derive_index_master_key;

/// Domain-separation context for the account-data plane's **entry seal root** —
/// `owner-key-material.md` § Path A-sibling-2 (frozen 2026-08-10 as T14, the
/// account-data plane's pre-W2 (account-data-plane.md § Workstreams) gate), `account-data-plane.md` § The class-2
/// entry form.
///
/// Versioned + dated per the key-hierarchy doc's rule #3 and pinned by a
/// known-answer test: this string is **frozen**, because editing it orphans
/// every sealed account-state entry — at rest on every replica *and* on every
/// custodian holding relayed copies. The delegable branch this string roots
/// never rotates at all (R14 (account-data-plane.md § The ratified decisions): it never gains a generation axis — a rotating
/// delegable key would orphan standing grants); the branch that does rotate is
/// the **fleet-only** pair, and it rotates by *key material* — generation N
/// derives from `gen_key_N` in place of [`BackupKey`] under the **same** frozen
/// strings ([`FleetOnlySchedule::derive_for_generation`]) — never by minting
/// new context strings (`owner-key-material.md` § The schedule build design).
pub const ACCOUNT_STATE_SEAL_DERIVE_CONTEXT: &str = "fauna.account-state.seal.v1 2026-08-10";

/// Domain-separation context for the account-data plane's **item-blind root** —
/// the naming axis, deliberately independent of the sealing axis above so a
/// capability grant can be reasoned about one kind at a time.
///
/// Same freeze discipline as [`ACCOUNT_STATE_SEAL_DERIVE_CONTEXT`], with a
/// different blast radius: editing this one leaves entries openable but makes
/// every stored item key unroutable, since routing, supersession and
/// latest-per-writer retention all key on it.
pub const ACCOUNT_STATE_ITEM_KEY_DERIVE_CONTEXT: &str =
    "fauna.account-state.item-key.v1 2026-08-10";

/// Domain-separation context for the **fleet-only** branch's entry-seal root
/// (R13, `account-data-plane.md` § The audience ladder; derivation owner:
/// `owner-key-material.md` § Path A-sibling-2 → the audience-ladder branch
/// split).
///
/// Additive beside [`ACCOUNT_STATE_SEAL_DERIVE_CONTEXT`] — nothing sealed under
/// the delegable strings is touched — and frozen on the same terms: for the life
/// of generation 0, because editing it orphans every fleet-only entry at rest and
/// on every custodian.
///
/// The whole point of the second string is **domain separation**: possession of
/// every delegable kind key reveals nothing about any fleet-only key, so a
/// capability grant — which can only ever carry delegable material
/// ([`DelegableKindKeys::to_grant`]) — is cryptographically incapable of reaching
/// an operational secret.
pub const ACCOUNT_STATE_FLEET_SEAL_DERIVE_CONTEXT: &str =
    "fauna.account-state.fleet-only.seal.v1 2026-08-11";

/// Domain-separation context for the **fleet-only** branch's item-blind root —
/// the naming axis of the fleet-only branch, independent of its sealing axis for
/// the same reason the delegable pair is split.
///
/// Same freeze discipline as [`ACCOUNT_STATE_FLEET_SEAL_DERIVE_CONTEXT`].
pub const ACCOUNT_STATE_FLEET_ITEM_KEY_DERIVE_CONTEXT: &str =
    "fauna.account-state.fleet-only.item-key.v1 2026-08-11";

/// Which audience rung a class-2 kind is registered at — the second frozen
/// registry column beside its merge policy (R13, `account-data-plane.md`
/// § The audience ladder; the per-kind registry itself lives at
/// `fauna_protocol::merge_policy`, which is where the two columns are declared
/// together).
///
/// Only the two rungs that *seal onto the plane* appear here. `Device-only` and
/// `Ceremony-only` are rungs of the ladder but not of this schedule: they never
/// enter any sync plane, so there is no branch for them to key — representing
/// them here would invite someone to seal one.
///
/// The class axis (merge mechanics) and this axis (who can open) are orthogonal
/// and must never be conflated: DNS provider credentials are *recreatable*
/// (whole-record LWW) **and** *secret* (fleet-only), so reusing a merge table as
/// an audience split ships credentials to grantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudienceRung {
    /// Inside the grant-mintable universe: a user-minted, revocable, audited
    /// grant can hand this kind's `{entry_key, item_blind}` pair to a third
    /// party. The deliberate exception, argued per kind.
    Delegable,
    /// Sealed under the sibling branch the grant machinery structurally cannot
    /// reach. **The default** — a kind widens only on an argued need, because a
    /// kind admitted too wide has had its plaintext sealed under a key grants
    /// can reach, and there is no quiet withdrawal.
    FleetOnly,
}

/// Which **key material seals** a class-2 kind's entries — the third frozen
/// registry column beside merge policy and audience rung (R14 build design,
/// `account-data-plane.md` § The generation machinery; the registry lives at
/// `fauna_protocol::merge_policy`).
///
/// Orthogonal to [`AudienceRung`] by design: the rung says *who can open*
/// (fleet vs. the grant universe — outside it, unchanged R13), the epoch says
/// *which key material seals*. Conflating them is what made the generation
/// machinery look paradoxically self-gated — the machinery kinds are
/// fleet-only rung **and** [`SealingEpoch::Gen0`], which is exactly the
/// bootstrap stratification (`owner-key-material.md` § Path A-sibling-2 →
/// *The schedule build design*): a fresh enrolled device must read the whole
/// mint DAG before it holds any generation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealingEpoch {
    /// The root-derived, derivable-forever generation 0 — the frozen branch
    /// pair this schedule already ships. Delegable kinds are `Gen0` **by
    /// construction** (their branch never gains a generation axis — a rotating
    /// delegable key would orphan standing grants), and the R14 machinery
    /// kinds are deliberately `Gen0` too (stratification, above).
    Gen0,
    /// Seals under the current **admissible, escrow-acked generation tip** —
    /// random per-generation key material, minted and distributed by the R14
    /// machinery. The writer door refuses a `GenerationTip` origination while
    /// no such tip exists (tip resolution — the gate's final shape, landing as
    /// the schedule build's last step; until then the rung-based boolean gate
    /// stands and this column is registry data only).
    GenerationTip,
}

/// The owner's **account-state key schedule** — the two roots the account-data
/// plane's class-2 sync units derive from (`owner-key-material.md`
/// § Path A-sibling-2, frozen 2026-08-10).
///
/// Rooted on the owner [`BackupKey`] bytes, **not** the identity seed, and the
/// forcing argument is custody rather than cryptography: the plane's device
/// principal bundle (`account-data-plane.md` § The store device principal)
/// carries `BackupKey` and never the seed, so rooting one step below the seed
/// makes the whole schedule derivable by exactly the principals allowed to read
/// (R7 reading replicas) with nothing new distributed. It stays transitively
/// seed-rooted, so restore-from-seed reproduces everything.
///
/// Two axes, deliberately independent:
///
/// - **entry keys** seal values — `entry_key(kind)`, the AEAD key of the sealed
///   entry envelope ([`crate::account_entry_crypto`]).
/// - **item blinds** name items — `item_key(kind, logical_key)`, the opaque
///   32-byte routing key a relay sees.
///
/// Splitting them is what makes the per-kind grant unit meaningful: a granted
/// position holds one kind's `{entry_key, item_blind}` pair and can find and
/// open exactly that kind, while a *naming*-only position could route without
/// reading. Keeping them one key would collapse that distinction.
///
/// # The audience-ladder branch split (R13)
///
/// Both axes above exist **twice**, once per grantable posture: a
/// [`DelegableSchedule`] and a [`FleetOnlySchedule`], each with its own frozen
/// context strings, domain-separated so possession of every delegable key
/// reveals nothing about any fleet-only key. A kind's registered
/// [`AudienceRung`] picks the branch ([`Self::for_rung`]), so *which key branch
/// sealed an entry* is what enforces its audience — never client-side
/// classification code. An old binary meeting a new kind never decides its
/// audience: it relays ciphertext it cannot open.
///
/// # Why item keys are blinded when file-sync's `path_hash` is not
///
/// The `path_hash` floor carve-out (unkeyed BLAKE3 of the path —
/// `encryption-at-rest.md` § Carve-outs) stands for file scopes, where paths
/// are arbitrary user strings. Class-2 logical keys are drawn from an
/// **enumerable** vocabulary (registered kinds × known field names), so an
/// unkeyed hash would be dictionary-reversible and would tell every custodian
/// and the nest *which setting* changed. The keyed blind makes item keys
/// stable-but-opaque.
///
/// # Custody
///
/// Both roots are [`Zeroizing`] for the reason spelled out at
/// [`derive_index_master_key`]: a bare `[u8; 32]` is `Copy`, so it duplicates
/// silently and never zeroizes. The pin below the tests enforces it.
pub struct AccountStateKeySchedule {
    delegable: DelegableSchedule,
    fleet_only: FleetOnlySchedule,
}

/// One branch's two roots. Private: the branches are distinguished by *type*
/// ([`DelegableSchedule`] / [`FleetOnlySchedule`]) precisely so a caller cannot
/// hold "a branch" generically and forget which one it is.
struct Branch {
    seal_root: Zeroizing<[u8; 32]>,
    item_blind_root: Zeroizing<[u8; 32]>,
}

impl Branch {
    /// `root` is [`BackupKey`] bytes for generation 0 and [`GenerationKey`]
    /// bytes for generation N — same frozen context strings either way
    /// (R14: independence between generations comes from key material, never
    /// from new strings).
    fn derive(root: &[u8; 32], seal_context: &str, item_key_context: &str) -> Branch {
        Branch {
            seal_root: Zeroizing::new(blake3::derive_key(seal_context, root)),
            item_blind_root: Zeroizing::new(blake3::derive_key(item_key_context, root)),
        }
    }

    /// The second BLAKE3 step of the `derive_key`-then-`keyed_hash` shape this
    /// codebase uses everywhere (`chunk_crypto`, `manifest_crypto`,
    /// `path_crypto`).
    fn for_kind(&self, kind: &str) -> AccountStateKindKeys {
        AccountStateKindKeys {
            kind: kind.to_string(),
            entry_key: Zeroizing::new(
                *blake3::keyed_hash(&self.seal_root, kind.as_bytes()).as_bytes(),
            ),
            item_blind: Zeroizing::new(
                *blake3::keyed_hash(&self.item_blind_root, kind.as_bytes()).as_bytes(),
            ),
        }
    }
}

/// The **delegable** branch — the one grants reach.
///
/// Its [`for_kind`](Self::for_kind) returns [`DelegableKindKeys`], the only type
/// in this module with a [`to_grant`](DelegableKindKeys::to_grant) method. That
/// is the mechanism behind R13's "structurally outside the grant-mintable
/// universe", and it is a **typed-path** guarantee: no typed value below a root
/// leads from a fleet-only kind to a grant — [`FleetOnlySchedule`] hands back
/// plain [`AccountStateKindKeys`], which cannot be turned into a grant, and its
/// roots are crate-private. The raw root itself stays one derivation away from
/// either branch (see [`AccountStateKindKeys`]); that route is guarded by
/// review, not by the types.
pub struct DelegableSchedule(Branch);

/// The **fleet-only** branch — sealed under sibling context strings the grant
/// machinery cannot reach.
///
/// Operational secrets live here: every replica in the world may carry the
/// ciphertext (that is the durability story) and no grant can ever open it.
///
/// Its roots are crate-private: a public root is one `keyed_hash` away from a
/// fleet-only kind's raw pair, which [`AccountStateKindKeys::from_grant`] would
/// then accept as a working grant tuple. Pinned from outside the crate — a
/// doctest sees only public items:
///
/// ```compile_fail,E0624
/// use fauna_core::crypto::*;
/// let s = FleetOnlySchedule::derive_for_generation(&GenerationKey::mint());
/// let _ = s.seal_root();
/// ```
/// ```compile_fail,E0624
/// use fauna_core::crypto::*;
/// let s = FleetOnlySchedule::derive_for_generation(&GenerationKey::mint());
/// let _ = s.item_blind_root();
/// ```
pub struct FleetOnlySchedule(Branch);

impl AccountStateKeySchedule {
    /// Derive both branches from the owner's [`BackupKey`].
    ///
    /// Deterministic: every reading principal derives identical roots with
    /// nothing stored and nothing synced.
    pub fn derive(backup_key: &BackupKey) -> AccountStateKeySchedule {
        AccountStateKeySchedule {
            delegable: DelegableSchedule::derive(backup_key),
            fleet_only: FleetOnlySchedule(Branch::derive(
                backup_key.as_bytes(),
                ACCOUNT_STATE_FLEET_SEAL_DERIVE_CONTEXT,
                ACCOUNT_STATE_FLEET_ITEM_KEY_DERIVE_CONTEXT,
            )),
        }
    }

    /// The delegable branch — grant minting starts here and nowhere else.
    pub fn delegable(&self) -> &DelegableSchedule {
        &self.delegable
    }

    /// The fleet-only branch.
    pub fn fleet_only(&self) -> &FleetOnlySchedule {
        &self.fleet_only
    }

    /// Route one kind to its branch by the rung it is **registered** at.
    ///
    /// The rung is passed in rather than looked up because the registry lives one
    /// layer up (`fauna_protocol::merge_policy`, which declares a kind's merge
    /// policy and its rung together); `fauna_protocol::merge_policy::kind_keys`
    /// is the call every production path uses, and it is the only place the
    /// lookup happens. Taking the rung explicitly here is what keeps this crate
    /// free of the registry without re-creating "client-side classification
    /// code": nothing *decides* a rung at this layer, it only fans out.
    pub fn for_rung(&self, rung: AudienceRung, kind: &str) -> AccountStateKindKeys {
        match rung {
            AudienceRung::Delegable => self.delegable.for_kind(kind).into_keys(),
            AudienceRung::FleetOnly => self.fleet_only.for_kind(kind),
        }
    }
}

impl DelegableSchedule {
    /// The delegable branch alone, from an owner's [`BackupKey`] — the same
    /// two frozen context strings [`AccountStateKeySchedule::derive`] uses, so
    /// the per-kind keys are identical to that schedule's delegable half.
    ///
    /// For a holder that must open an identity's delegable rows and nothing
    /// wider: a successor's walk trial-opens its attested predecessors'
    /// generation-0 delegable rows under this
    /// (`succession-aftermath.md` § Re-key scope), and the type carries no
    /// fleet branch for it to reach.
    pub fn derive(backup_key: &BackupKey) -> DelegableSchedule {
        DelegableSchedule(Branch::derive(
            backup_key.as_bytes(),
            ACCOUNT_STATE_SEAL_DERIVE_CONTEXT,
            ACCOUNT_STATE_ITEM_KEY_DERIVE_CONTEXT,
        ))
    }

    /// Fan out both axes for one delegable kind. The result is exactly the
    /// capability-grant unit for `kind`.
    pub fn for_kind(&self, kind: &str) -> DelegableKindKeys {
        DelegableKindKeys(self.0.for_kind(kind))
    }

    /// The entry-seal root. Pinned by a known-answer test.
    pub fn seal_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.seal_root
    }

    /// The item-blind root. Pinned by a known-answer test.
    pub fn item_blind_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.item_blind_root
    }
}

impl FleetOnlySchedule {
    /// The generation-N fleet branch: both roots derive from the generation's
    /// random key **in place of** [`BackupKey`], under the **same** two frozen
    /// context strings — R14's ratified rotation shape
    /// (`owner-key-material.md` § The schedule build design): independence
    /// between generations, and from generation 0, comes from key material,
    /// never from new strings. Only the fleet branch exists per generation —
    /// the delegable branch never gains a generation axis, so there is no
    /// generation-N [`AccountStateKeySchedule`], deliberately.
    pub fn derive_for_generation(gen_key: &GenerationKey) -> FleetOnlySchedule {
        FleetOnlySchedule(Branch::derive(
            gen_key.as_bytes(),
            ACCOUNT_STATE_FLEET_SEAL_DERIVE_CONTEXT,
            ACCOUNT_STATE_FLEET_ITEM_KEY_DERIVE_CONTEXT,
        ))
    }

    /// Fan out both axes for one fleet-only kind. Deliberately **not** a
    /// [`DelegableKindKeys`]: there is no path from here to a grant.
    pub fn for_kind(&self, kind: &str) -> AccountStateKindKeys {
        self.0.for_kind(kind)
    }

    /// ONE fleet-only kind's generation-0 keys, from an owner's [`BackupKey`]
    /// — the pair [`AccountStateKeySchedule::derive`]'s fleet branch fans out
    /// for `kind`, with the branch dropped before this returns.
    ///
    /// For a holder that must open one fleet-only kind of an identity and
    /// nothing else of its branch: a successor's fleet walk trial-opens its
    /// attested predecessors' generation-mint rows under this
    /// (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
    /// succession rider), and holds no value that could open that identity's
    /// device-set, wrap, escrow or reach rows.
    pub fn generation_0_kind_keys(backup_key: &BackupKey, kind: &str) -> AccountStateKindKeys {
        Branch::derive(
            backup_key.as_bytes(),
            ACCOUNT_STATE_FLEET_SEAL_DERIVE_CONTEXT,
            ACCOUNT_STATE_FLEET_ITEM_KEY_DERIVE_CONTEXT,
        )
        .for_kind(kind)
    }

    /// The entry-seal root. Pinned by a known-answer test.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "KAT-only; crate-private per R13")
    )]
    pub(crate) fn seal_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.seal_root
    }

    /// The item-blind root. Pinned by a known-answer test.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "KAT-only; crate-private per R13")
    )]
    pub(crate) fn item_blind_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.item_blind_root
    }
}

// ── The group machinery root (T20 sealing ruling, 2026-08-17) ───────────────
//
// A storage group's MACHINERY kinds — birth record, roster entries, mint rows,
// top-up rows, the unkeyable signal — seal under a dedicated root: 32 random
// bytes minted by the initiating account at scope birth, immutable for the
// scope's life, handed to each member at admission inside the same X-Wing wrap
// bundle as the retained generation bundle (`key-material-hierarchy.md`
// § Audience: a storage group, the machinery-root bullet). It is the group's
// `SealingEpoch::Gen0` analogue with **possession replacing derivability**:
// members share no cross-account root secret to derive from, so "derivable
// forever from `BackupKey`" becomes "held forever from admission". Content
// kinds are untouched — they seal under the group generation tip.

/// Domain-separation context for the **machinery-root↔scope-id commitment** —
/// `commit = BLAKE3::derive_key(this, root)`, carried in the group's birth
/// record ([`crate::group_scope::GroupBirthRecord::machinery_root_commit`])
/// and therefore covered by the content-derived scope id. Frozen: a joiner
/// re-derives the scope id from the birth record it unwrapped and refuses a
/// root whose commitment does not match, so editing this string would make
/// every honest root verify as substituted.
pub const GROUP_MACHINERY_ROOT_COMMIT_CONTEXT: &str =
    "fauna.group.machinery-root-commit.v1 2026-08-17";

/// Domain-separation context for the group machinery branch's **entry-seal
/// root**. A new frozen string, deliberately not the account-state pair: the
/// account plane reuses its contexts *across generations of one schedule*,
/// while a different plane gets its own named pair — the `chunk_crypto` /
/// `path_crypto` precedent (`key-material-hierarchy.md` § Audience: a storage
/// group, *Derivations*). Frozen: editing it orphans every sealed machinery
/// entry of every group at rest on every member and custodian.
pub const GROUP_MACHINERY_SEAL_DERIVE_CONTEXT: &str = "fauna.group.machinery.seal.v1 2026-08-17";

/// Domain-separation context for the group machinery branch's **item-blind
/// root** — the naming axis, independent of the sealing axis for the same
/// reason every schedule here splits them. Same freeze discipline as
/// [`GROUP_MACHINERY_SEAL_DERIVE_CONTEXT`].
pub const GROUP_MACHINERY_ITEM_KEY_DERIVE_CONTEXT: &str =
    "fauna.group.machinery.item-key.v1 2026-08-17";

/// One storage group's machinery root — the 32 random bytes every machinery
/// kind of that scope seals under.
///
/// **Random, never derived, never rotated** (the sealing ruling): severance is
/// wrap targeting on generations plus roster supersession, and rotating the
/// root would re-seal all machinery and sever nothing — so every ever-admitted
/// member holds it for the scope's life, and a custodian without it sees
/// nothing past the custody floor. Minted once per scope ([`Self::mint`]);
/// reaches members only inside their admission wrap bundle.
///
/// [`Zeroizing`] custody for the reason at [`derive_index_master_key`]: a bare
/// `[u8; 32]` is `Copy`, silently duplicated and never zeroized.
pub struct GroupMachineryRoot(Zeroizing<[u8; 32]>);

impl GroupMachineryRoot {
    /// Mint a fresh scope's machinery root from the OS CSPRNG.
    pub fn mint() -> GroupMachineryRoot {
        GroupMachineryRoot(Zeroizing::new(
            ChaCha20Poly1305::generate_key(&mut OsRng).into(),
        ))
    }

    /// Reconstruct from raw bytes — an unwrapped admission-bundle slot.
    /// Callers verify [`Self::commitment`] against the birth record before
    /// trusting the bytes (root substitution at admission is detected at the
    /// joiner, never trusted from the ceremony).
    pub fn from_bytes(bytes: [u8; 32]) -> GroupMachineryRoot {
        GroupMachineryRoot(Zeroizing::new(bytes))
    }

    /// The raw root material — the admission-bundle plaintext and the branch
    /// root.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The root's commitment under
    /// [`GROUP_MACHINERY_ROOT_COMMIT_CONTEXT`] — the birth record's
    /// `machinery_root_commit` value, and what a joiner recomputes at unwrap.
    #[must_use]
    pub fn commitment(&self) -> [u8; 32] {
        blake3::derive_key(GROUP_MACHINERY_ROOT_COMMIT_CONTEXT, &*self.0)
    }
}

/// The group machinery branch — both axes of the two-step
/// `derive_key`-then-`keyed_hash` shape, rooted on one scope's
/// [`GroupMachineryRoot`] under the two frozen group-named contexts above.
/// The per-kind fan-out returns the same `{entry_key, item_blind}` pair shape
/// the account-state branches produce, because the sealed entry envelope and
/// the blinded item key are the same class-2 mechanics on a different plane.
///
/// Non-delegable like the fleet-only branch, so its roots are crate-private
/// for the same reason (see [`FleetOnlySchedule`]):
///
/// ```compile_fail,E0624
/// use fauna_core::crypto::*;
/// let s = GroupMachinerySchedule::derive(&GroupMachineryRoot::mint());
/// let _ = s.seal_root();
/// ```
/// ```compile_fail,E0624
/// use fauna_core::crypto::*;
/// let s = GroupMachinerySchedule::derive(&GroupMachineryRoot::mint());
/// let _ = s.item_blind_root();
/// ```
pub struct GroupMachinerySchedule(Branch);

impl GroupMachinerySchedule {
    /// Derive the branch for one scope's root.
    pub fn derive(root: &GroupMachineryRoot) -> GroupMachinerySchedule {
        GroupMachinerySchedule(Branch::derive(
            root.as_bytes(),
            GROUP_MACHINERY_SEAL_DERIVE_CONTEXT,
            GROUP_MACHINERY_ITEM_KEY_DERIVE_CONTEXT,
        ))
    }

    /// Fan out both axes for one machinery kind. Deliberately not a
    /// [`DelegableKindKeys`]: there is no path from a group's machinery to a
    /// grant.
    pub fn for_kind(&self, kind: &str) -> AccountStateKindKeys {
        self.0.for_kind(kind)
    }

    /// The entry-seal root. Pinned by a known-answer test.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "KAT-only; crate-private per R13")
    )]
    pub(crate) fn seal_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.seal_root
    }

    /// The item-blind root. Pinned by a known-answer test.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "KAT-only; crate-private per R13")
    )]
    pub(crate) fn item_blind_root(&self) -> &Zeroizing<[u8; 32]> {
        &self.0.item_blind_root
    }
}

/// 32 fresh CSPRNG bytes — the plain salt mint shared by surfaces that need
/// a random 32 with no custody semantics (birth salts, admission salts).
/// The custody-typed siblings ([`GenerationKey::mint`],
/// [`GroupMachineryRoot::mint`]) stay the KEY mints — key material always
/// gets a Zeroizing type, a salt is public the moment it is used.
#[must_use]
pub fn random_salt_32() -> [u8; 32] {
    ChaCha20Poly1305::generate_key(&mut OsRng).into()
}

/// Domain-separation context for the R14 **key↔id commitment** —
/// `commit = BLAKE3(gen_key)` under this context, carried in the mint's
/// [`crate::generation::MintCore`] and covered by the content-derived
/// generation id (`owner-key-material.md` § The schedule build design →
/// *Key↔id binding*). Frozen: every unwrap recomputes the commitment against
/// the mint it resolved, so editing this string would make every honest key
/// verify as substituted.
pub const GENERATION_KEY_COMMIT_CONTEXT: &str = "fauna.generation.key-commit.v1 2026-08-13";

/// One generation's random key material — the root the generation-N fleet
/// branch derives from ([`FleetOnlySchedule::derive_for_generation`]).
///
/// **Random, never derived** (R14's whole point: `BackupKey`-derived material
/// is derivable forever by every reading replica, removed devices included —
/// wrap targeting is what severs, so the key must be reachable only through a
/// wrap). Minted once per generation ([`Self::mint`]); reaches other devices
/// only as X-Wing wraps (`fauna_mls::wrapped_blob::generation_wraps`) and the
/// escrow holder as the identity-targeted escrow wrap.
///
/// [`Zeroizing`] custody for the reason at [`derive_index_master_key`]: a bare
/// `[u8; 32]` is `Copy`, silently duplicated and never zeroized.
pub struct GenerationKey(Zeroizing<[u8; 32]>);

impl GenerationKey {
    /// Mint a fresh generation's key from the OS CSPRNG.
    pub fn mint() -> GenerationKey {
        GenerationKey(Zeroizing::new(
            ChaCha20Poly1305::generate_key(&mut OsRng).into(),
        ))
    }

    /// Reconstruct from raw bytes — an unwrapped wrap payload, a bundle slot.
    /// Callers verify the commitment before trusting the bytes
    /// ([`Self::commitment`]; the unwrap doors in `generation_wraps` do it for
    /// them).
    pub fn from_bytes(bytes: [u8; 32]) -> GenerationKey {
        GenerationKey(Zeroizing::new(bytes))
    }

    /// The raw key material — the wrap plaintext and the branch root.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// `BLAKE3(gen_key)` under the frozen commit context — the value a mint
    /// entry carries as [`crate::generation::MintCore::key_commitment`] and
    /// every unwrap recomputes: a key that does not match its mint's
    /// commitment is refused at the unwrapping device, never trusted from the
    /// wire.
    pub fn commitment(&self) -> [u8; 32] {
        blake3::derive_key(GENERATION_KEY_COMMIT_CONTEXT, &*self.0)
    }
}

/// A delegable kind's key pair — [`AccountStateKindKeys`] plus the one thing the
/// fleet-only branch must never be able to do: [`to_grant`](Self::to_grant).
///
/// Deref'ing to the inner keys keeps sealing and opening uniform across rungs
/// (the entry form does not fork by audience — R7), while grant minting stays
/// reachable only from this type.
pub struct DelegableKindKeys(AccountStateKindKeys);

impl DelegableKindKeys {
    /// The raw pair, for minting a capability grant
    /// (`encryption-at-rest.md` § Capability tiering). Deliberately the *only*
    /// public way out of the custody wrappers, and reachable only from the
    /// delegable branch.
    pub fn to_grant(&self) -> ([u8; 32], [u8; 32]) {
        (*self.0.entry_key, *self.0.item_blind)
    }

    /// Drop the grant-mintable marker, keeping the seal/open keys.
    pub fn into_keys(self) -> AccountStateKindKeys {
        self.0
    }
}

impl std::ops::Deref for DelegableKindKeys {
    type Target = AccountStateKindKeys;

    fn deref(&self) -> &AccountStateKindKeys {
        &self.0
    }
}

/// One kind's `{entry_key, item_blind}` pair — the **capability-grant unit** of
/// the account-data plane (`encryption-at-rest.md` § Capability tiering).
///
/// A position holding this finds and opens exactly this kind's entries and
/// cannot recognize, name, or open any other kind's. A full reading replica
/// derives the whole schedule from its bundle's [`BackupKey`], so no per-kind
/// distribution problem exists — [`AccountStateKindKeys::from_grant`] exists for
/// the *granted* position, which receives the pair and holds no root.
///
/// **Not grant-mintable.** Minting requires [`DelegableKindKeys`], which only
/// [`DelegableSchedule`] produces (R13) — so no typed path re-exports a
/// fleet-only key or a grantee's own received pair as a further grant. Grants do
/// not chain.
///
/// That is a typed-path guarantee, not an unwritable expression. A holder of a
/// raw root ([`BackupKey`], [`GenerationKey`], [`GroupMachineryRoot`] — public
/// by necessity: they wrap plaintext and fill bundle slots) can recompute any
/// kind's pair from the public context strings and hand it to
/// [`from_grant`](Self::from_grant). What guards that route is review: a root's
/// holders are the fleet's own code. Below the root the types hold — the pair's
/// secret halves have no public accessor, and neither do the non-delegable
/// branches' roots:
///
/// ```compile_fail,E0624
/// use fauna_core::crypto::AccountStateKindKeys;
/// let k = AccountStateKindKeys::from_grant("fauna.state.k", [0; 32], [0; 32]);
/// let _ = k.entry_key();
/// ```
///
/// Carries its `kind` because the open path must check the sealed payload's
/// claimed kind against the keys it opened under — see
/// [`crate::account_entry_crypto::open_entry`].
pub struct AccountStateKindKeys {
    kind: String,
    entry_key: Zeroizing<[u8; 32]>,
    item_blind: Zeroizing<[u8; 32]>,
}

impl AccountStateKindKeys {
    /// Reconstruct the pair at a **granted** position, which holds no root.
    pub fn from_grant(
        kind: &str,
        entry_key: [u8; 32],
        item_blind: [u8; 32],
    ) -> AccountStateKindKeys {
        AccountStateKindKeys {
            kind: kind.to_string(),
            entry_key: Zeroizing::new(entry_key),
            item_blind: Zeroizing::new(item_blind),
        }
    }

    /// The kind these keys are for.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The wire/journal **item key** for one logical key:
    /// `keyed_hash(item_blind(kind), logical_key)`.
    ///
    /// 32 bytes, dropping straight into the feed's existing opaque routing-key
    /// slot (the nest's `sync_changes.path_hash` column — a new value
    /// population, never a new meaning). Not secret: it is exactly what a relay
    /// is supposed to see.
    pub fn item_key(&self, logical_key: &[u8]) -> [u8; 32] {
        *blake3::keyed_hash(&self.item_blind, logical_key).as_bytes()
    }

    pub(crate) fn entry_key(&self) -> &[u8; 32] {
        &self.entry_key
    }
}

/// Which owner-audience key a sync engine seals its **owner-only** chunks under.
///
/// Both variants are the data owner's own key, and both seal through the same
/// convergent `chunk_crypto` primitive — the destination stores opaque ciphertext
/// either way and needs no per-variant arm. They differ only in *who is allowed
/// to hold the key*, which is the whole point of keeping them distinct here
/// rather than collapsing to a bare 32-byte root: a future reader (or a
/// debugger) can always tell which trust class an engine is operating in.
///
/// - [`OwnerSealKey::Client`] — the owner's [`BackupKey`], sealing
///   client-originated data. **Never nest-held.** Every engine on a user's own
///   device is on this variant.
/// - [`OwnerSealKey::SourceNest`] — the owner-granted [`NestBackupKey`], sealing
///   the nest-originated message-kind segments that the source nest backs up
///   in-process on the owner's behalf (`key-material-hierarchy.md`
///   § Path A-sibling-0).
///
/// Mixing them up is a data-at-rest break, not a mere type error: a chunk sealed
/// under one root cannot be opened under the other.
#[derive(Clone)]
pub enum OwnerSealKey {
    /// Client-originated data, sealed on the user's device. Never nest-held.
    Client(BackupKey),
    /// Nest-originated message-kind segments, sealed by the user's own source
    /// nest under the key their client granted it.
    SourceNest(NestBackupKey),
}

impl OwnerSealKey {
    /// The convergent `chunk_crypto` root every owner-only chunk seals under.
    ///
    /// This is the *only* thing the sync engine ever needs from an owner key —
    /// which is why this enum, not a whole keyring, is what the engine holds.
    pub fn convergent_chunk_root(&self) -> [u8; 32] {
        match self {
            OwnerSealKey::Client(k) => k.convergent_chunk_root(),
            OwnerSealKey::SourceNest(k) => k.convergent_chunk_root(),
        }
    }

    /// The underlying client [`BackupKey`], or `None` on the nest variant.
    ///
    /// For the handful of paths that are **client-only by construction** and need
    /// the whole key rather than a chunk root — today just the media-library
    /// thumbnail seal (`fauna_media::audience::Audience::Library`), which a
    /// segment-backup engine never reaches (it uploads opaque segment bytes and
    /// thumbnails nothing). Returning `None` for `SourceNest` is the correct,
    /// self-documenting refusal, not a gap: a source nest has no client key and
    /// must never be handed one.
    pub fn client_key(&self) -> Option<&BackupKey> {
        match self {
            OwnerSealKey::Client(k) => Some(k),
            OwnerSealKey::SourceNest(_) => None,
        }
    }
}

impl From<BackupKey> for OwnerSealKey {
    fn from(key: BackupKey) -> Self {
        OwnerSealKey::Client(key)
    }
}

impl From<NestBackupKey> for OwnerSealKey {
    fn from(key: NestBackupKey) -> Self {
        OwnerSealKey::SourceNest(key)
    }
}

/// Which owner-audience key — if any — a chunk site may seal or open under,
/// given whether the set is **bound** to a shared folder or otherwise
/// **content-keyed** (a WebDAV-served set with no group holds M2 content keys
/// at the serve pseudo-channel — `webdav-server.md` § Key model).
///
/// **This is the one place FS-5DC is decided.** Every holder of the
/// (`mls_group_id`, `content_keys`, `backup_key`) triple routes its owner-key
/// precedence through here rather than re-deciding it:
/// [`crate::file_download::FileDownloadKeys`] on the read side, and
/// `fauna_sync_engine`'s engine on the seal/open side. The rule itself is one
/// line — a **content-keyed set's chunks are content-keyed and never
/// owner-keyed** — and the reason it is a function rather than a convention is
/// that it has already failed twice by being written per-site: once per call
/// site (below), and once here, when the gate was the *group* rather than the
/// keys. Until 2026-09-09 a served-but-unshared set — content keys held, no
/// group — took the owner arm, so the app's own engine sealed its uploads
/// under the `BackupKey` with no generation stamp while the set's DAV reader,
/// holding only the M2 key, failed closed on every one of them; the label funnel had been asking
/// `content_seal_root` first all along, so names and bytes of one set were
/// even sealed for different audiences.
///
/// FS-5DC: the bearer-only
/// hydration service builds an engine with `backup_key = Some` **and** a bound
/// set's app-pushed `content_keys`/`mls_group_id`. Because the raw `backup_key`
/// was checked *first* at every seal/open site, it **strictly shadowed** the
/// content-key path — a content-key-sealed chunk was (mis)opened under the
/// owner's `BackupKey` (AEAD mismatch → hydration fails), 5d(c) was inert, and
/// the resolver's fail-closed posture never ran. Gating the precedence on the
/// bound-marker in one place makes every chunk site prefer the content key for
/// a bound set, uniformly (the same fix serves the linux in-process consumer —
/// FS-BIND FOLLOW-ON A — when it passes both).
///
/// Fails safe by construction: a bound set never seals/opens under
/// `backup_key`, and never under plaintext either — the content-key resolvers
/// (`content_seal_root`/`content_open_roots`) bail on a bound-but-keyless
/// holder rather than fall through to this one. Retired predecessor roots
/// follow this answer rather than re-deciding it, so a bound set offers none of
/// them either (`succession-aftermath.md` § the read-half bullet).
///
/// ⚠ Not every `mls_group_id` branch is this rule. The sites that **bail** on a
/// bound-but-keyless holder are a different, adjacent decision (fail closed vs.
/// degrade), and the thumbnail path stays on the raw `backup_key` deliberately
/// — a shared-set thumbnail is a separate deferred concern on an upload-only
/// path the download-only bearer service never reaches.
pub fn effective_owner_key<'a>(
    mls_group_id: Option<&[u8]>,
    content_keyed: bool,
    backup_key: Option<&'a OwnerSealKey>,
) -> Option<&'a OwnerSealKey> {
    if mls_group_id.is_some() || content_keyed {
        None
    } else {
        backup_key
    }
}

/// Encrypt `plaintext` under `key`, returning a framed ciphertext.
///
/// Output layout:
/// ```text
/// [0x01 (1 byte)] [nonce (12 bytes)] [ciphertext + tag (N + 16 bytes)]
/// ```
///
/// The version byte allows migrating to a new algorithm in the future without
/// breaking existing archives.
pub fn encrypt_backup_chunk(key: &BackupKey, plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_bytes())
        .expect("32-byte key is always valid for ChaCha20-Poly1305");

    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("backup chunk encryption failed: {e}"))?;

    // Assemble: version || nonce || ciphertext+tag
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a ciphertext previously produced by [`encrypt_backup_chunk`].
///
/// Returns an error if the version byte is unrecognised, the input is too
/// short, or the AEAD tag does not verify (indicating tampering or a wrong key).
pub fn decrypt_backup_chunk(key: &BackupKey, ciphertext: &[u8]) -> Result<Vec<u8>> {
    // Minimum: 1 (version) + 12 (nonce) + 16 (tag) = 29 bytes
    const MIN_LEN: usize = 1 + NONCE_LEN + 16;

    if ciphertext.len() < MIN_LEN {
        bail!(
            "backup ciphertext too short: {} bytes (minimum {})",
            ciphertext.len(),
            MIN_LEN
        );
    }

    let version = ciphertext[0];
    if version != VERSION {
        bail!("unsupported backup ciphertext version: 0x{:02x}", version);
    }

    let nonce_bytes: [u8; NONCE_LEN] = ciphertext[1..1 + NONCE_LEN]
        .try_into()
        .expect("slice length is guaranteed by MIN_LEN check");
    let nonce = chacha20poly1305::Nonce::from(nonce_bytes);

    let cipher = ChaCha20Poly1305::new_from_slice(key.as_bytes())
        .expect("32-byte key is always valid for ChaCha20-Poly1305");

    cipher
        .decrypt(&nonce, &ciphertext[1 + NONCE_LEN..])
        .map_err(|e| {
            anyhow::anyhow!("backup chunk decryption failed (wrong key or tampered data): {e}")
        })
}

/// The compress-then-encrypt pipeline every backup-shaped seal (MLS replicas,
/// personalization models/cue rollups, drafts) hand-copies: `compress_chunk`
/// (infallible) → [`encrypt_backup_chunk`]. `encrypt_err` builds the caller's
/// own error type from the AEAD failure message, so each site keeps its own
/// error enum untouched.
pub fn seal_backup_chunk<E>(
    plaintext: &[u8],
    key: &BackupKey,
    encrypt_err: impl FnOnce(String) -> E,
) -> std::result::Result<Vec<u8>, E> {
    let compressed = crate::compress::compress_chunk(plaintext);
    encrypt_backup_chunk(key, &compressed).map_err(|e| encrypt_err(e.to_string()))
}

/// Inverse of [`seal_backup_chunk`]: [`decrypt_backup_chunk`] → `decompress_chunk`.
/// `decrypt_err`/`decompress_err` build the caller's own error type from each
/// stage's failure message, preserving the decrypt-vs-decompress distinction
/// every hand-copy already made.
pub fn unseal_backup_chunk<E>(
    blob: &[u8],
    key: &BackupKey,
    decrypt_err: impl FnOnce(String) -> E,
    decompress_err: impl FnOnce(String) -> E,
) -> std::result::Result<Vec<u8>, E> {
    let decrypted = decrypt_backup_chunk(key, blob).map_err(|e| decrypt_err(e.to_string()))?;
    crate::compress::decompress_chunk(&decrypted).map_err(|e| decompress_err(e.to_string()))
}

/// Leading form byte of a **kind-keyed chunk** ([`encrypt_kind_chunk`]) —
/// distinct from the BackupKey chunk's `0x01` so an opener tells the two forms
/// apart from the bytes alone, with no registry and no per-row flag.
pub const KIND_CHUNK_FORM: u8 = 0x02;

/// Encrypt `plaintext` under one delegable kind's `entry_key` — the opaque-blob
/// sibling of the account plane's class-2 entry, for a value that rests outside
/// the plane (a nest table storing it verbatim) yet must open for a
/// capability-grant holder of exactly that kind and nothing wider
/// (`topic-factors.md` § At rest → *Re-keyed for the third-party plane*).
///
/// Output layout — the BackupKey chunk's, under its own form byte:
/// ```text
/// [0x02 (1 byte)] [nonce (12 bytes)] [ciphertext + tag (N + 16 bytes)]
/// ```
/// AAD = the form byte then the kind string, so a blob cannot be re-presented
/// under another form or kind. Unlike [`crate::account_entry_crypto`]'s class-2
/// entry there are no plane coordinates and no writer signature: the blob is one
/// mutable record per nest row, and the positions able to open it — the owner's
/// devices and the kind's grantees — are exactly the ones entitled to rewrite it.
pub fn encrypt_kind_chunk(keys: &AccountStateKindKeys, plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(&*keys.entry_key)
        .expect("32-byte key is always valid for ChaCha20-Poly1305");
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let aad = kind_chunk_aad(keys.kind());
    let ciphertext = cipher
        .encrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|e| anyhow::anyhow!("kind chunk encryption failed: {e}"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(KIND_CHUNK_FORM);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a blob produced by [`encrypt_kind_chunk`] under the same kind's keys.
/// Refuses any other leading form byte (a BackupKey chunk included), a short
/// input, and a tag that does not verify (wrong kind, wrong owner, tampering).
pub fn decrypt_kind_chunk(keys: &AccountStateKindKeys, ciphertext: &[u8]) -> Result<Vec<u8>> {
    const MIN_LEN: usize = 1 + NONCE_LEN + 16;
    if ciphertext.len() < MIN_LEN {
        bail!(
            "kind chunk too short: {} bytes (minimum {})",
            ciphertext.len(),
            MIN_LEN
        );
    }
    if ciphertext[0] != KIND_CHUNK_FORM {
        bail!("unsupported kind chunk form: 0x{:02x}", ciphertext[0]);
    }
    let nonce_bytes: [u8; NONCE_LEN] = ciphertext[1..1 + NONCE_LEN]
        .try_into()
        .expect("slice length is guaranteed by MIN_LEN check");
    let nonce = chacha20poly1305::Nonce::from(nonce_bytes);
    let cipher = ChaCha20Poly1305::new_from_slice(&*keys.entry_key)
        .expect("32-byte key is always valid for ChaCha20-Poly1305");
    let aad = kind_chunk_aad(keys.kind());
    cipher
        .decrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: &ciphertext[1 + NONCE_LEN..],
                aad: &aad,
            },
        )
        .map_err(|e| {
            anyhow::anyhow!("kind chunk decryption failed (wrong key or tampered data): {e}")
        })
}

fn kind_chunk_aad(kind: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + kind.len());
    aad.push(KIND_CHUNK_FORM);
    aad.extend_from_slice(kind.as_bytes());
    aad
}

/// [`seal_backup_chunk`]'s pipeline under one kind's keys: `compress_chunk` →
/// [`encrypt_kind_chunk`].
pub fn seal_kind_chunk<E>(
    plaintext: &[u8],
    keys: &AccountStateKindKeys,
    encrypt_err: impl FnOnce(String) -> E,
) -> std::result::Result<Vec<u8>, E> {
    let compressed = crate::compress::compress_chunk(plaintext);
    encrypt_kind_chunk(keys, &compressed).map_err(|e| encrypt_err(e.to_string()))
}

/// Inverse of [`seal_kind_chunk`]: [`decrypt_kind_chunk`] → `decompress_chunk`.
pub fn unseal_kind_chunk<E>(
    blob: &[u8],
    keys: &AccountStateKindKeys,
    decrypt_err: impl FnOnce(String) -> E,
    decompress_err: impl FnOnce(String) -> E,
) -> std::result::Result<Vec<u8>, E> {
    let decrypted = decrypt_kind_chunk(keys, blob).map_err(|e| decrypt_err(e.to_string()))?;
    crate::compress::decompress_chunk(&decrypted).map_err(|e| decompress_err(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── BackupKey derivation ──────────────────────────────────────────────────

    #[test]
    fn derive_is_deterministic() {
        let seed = [0x42u8; 32];
        let key_a = BackupKey::derive(&seed);
        let key_b = BackupKey::derive(&seed);
        assert_eq!(key_a.0, key_b.0, "same seed must produce identical key");
    }

    #[test]
    fn different_seeds_produce_different_keys() {
        let seed_a = [0x01u8; 32];
        let seed_b = [0x02u8; 32];
        let key_a = BackupKey::derive(&seed_a);
        let key_b = BackupKey::derive(&seed_b);
        assert_ne!(key_a.0, key_b.0, "different seeds must not collide");
    }

    #[test]
    fn from_bytes_roundtrip() {
        let raw = [0xABu8; 32];
        let key = BackupKey::from_bytes(raw);
        assert_eq!(*key.as_bytes(), raw);
    }

    // ── encrypt_backup_chunk / decrypt_backup_chunk ───────────────────────────

    #[test]
    fn roundtrip_basic() {
        let key = BackupKey::from_bytes([0x55u8; 32]);
        let plaintext = b"hello fauna backup";

        let ct = encrypt_backup_chunk(&key, plaintext).unwrap();
        let pt = decrypt_backup_chunk(&key, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn roundtrip_empty_plaintext() {
        let key = BackupKey::from_bytes([0x00u8; 32]);
        let ct = encrypt_backup_chunk(&key, b"").unwrap();
        let pt = decrypt_backup_chunk(&key, &ct).unwrap();
        assert_eq!(pt, b"");
    }

    #[test]
    fn ciphertext_has_correct_overhead() {
        let key = BackupKey::from_bytes([0x11u8; 32]);
        let plaintext = b"exactly 16 bytes";
        let ct = encrypt_backup_chunk(&key, plaintext).unwrap();
        // 1 (version) + 12 (nonce) + plaintext.len() + 16 (tag)
        let expected_len = 1 + NONCE_LEN + plaintext.len() + 16;
        assert_eq!(ct.len(), expected_len);
    }

    #[test]
    fn version_byte_is_0x01() {
        let key = BackupKey::from_bytes([0xFFu8; 32]);
        let ct = encrypt_backup_chunk(&key, b"test").unwrap();
        assert_eq!(ct[0], 0x01, "first byte must be version 0x01");
    }

    #[test]
    fn two_encryptions_produce_different_ciphertext() {
        // Random nonces mean the same plaintext encrypts differently each time.
        let key = BackupKey::from_bytes([0x77u8; 32]);
        let pt = b"same plaintext";
        let ct1 = encrypt_backup_chunk(&key, pt).unwrap();
        let ct2 = encrypt_backup_chunk(&key, pt).unwrap();
        // Nonces are 12 bytes starting at index 1 — they almost certainly differ.
        assert_ne!(
            ct1, ct2,
            "random nonces should produce distinct ciphertexts"
        );
    }

    #[test]
    fn wrong_key_returns_error() {
        let key_a = BackupKey::from_bytes([0x01u8; 32]);
        let key_b = BackupKey::from_bytes([0x02u8; 32]);
        let ct = encrypt_backup_chunk(&key_a, b"secret").unwrap();
        let result = decrypt_backup_chunk(&key_b, &ct);
        assert!(result.is_err(), "wrong key must not decrypt successfully");
    }

    #[test]
    fn tampered_ciphertext_returns_error() {
        let key = BackupKey::from_bytes([0x33u8; 32]);
        let mut ct = encrypt_backup_chunk(&key, b"sensitive data").unwrap();
        // Flip a bit in the ciphertext body (after version+nonce).
        let body_start = 1 + NONCE_LEN;
        ct[body_start] ^= 0xFF;
        let result = decrypt_backup_chunk(&key, &ct);
        assert!(result.is_err(), "tampered ciphertext must not verify");
    }

    #[test]
    fn tampered_tag_returns_error() {
        let key = BackupKey::from_bytes([0x44u8; 32]);
        let mut ct = encrypt_backup_chunk(&key, b"data").unwrap();
        // Flip the last byte (part of the Poly1305 tag).
        let last = ct.len() - 1;
        ct[last] ^= 0x01;
        let result = decrypt_backup_chunk(&key, &ct);
        assert!(result.is_err(), "tampered tag must not verify");
    }

    #[test]
    fn ciphertext_too_short_returns_error() {
        let key = BackupKey::from_bytes([0x55u8; 32]);
        let result = decrypt_backup_chunk(&key, &[0x01u8; 10]);
        assert!(result.is_err(), "too-short ciphertext must return error");
    }

    #[test]
    fn unknown_version_returns_error() {
        let key = BackupKey::from_bytes([0x55u8; 32]);
        let mut ct = encrypt_backup_chunk(&key, b"data").unwrap();
        ct[0] = 0x02; // change version byte
        let result = decrypt_backup_chunk(&key, &ct);
        assert!(result.is_err(), "unknown version must return error");
    }

    #[test]
    fn derived_key_encrypt_decrypt() {
        let seed = [0xDEu8; 32];
        let key = BackupKey::derive(&seed);
        let plaintext = b"backup payload derived from identity";
        let ct = encrypt_backup_chunk(&key, plaintext).unwrap();
        let pt = decrypt_backup_chunk(&key, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    // ── to_bytes accessor ────────────────────────────────────────────────────

    #[test]
    fn backup_key_to_bytes_round_trips_from_bytes() {
        let k = [0x42u8; 32];
        assert_eq!(BackupKey::from_bytes(k).to_bytes(), k);
    }

    #[test]
    fn backup_key_derive_pins_known_vector() {
        // Known-answer test — pins BLAKE3 "fauna backup encryption key 2026-03-12"
        // derivation from all-0x01 seed. Guards both the derivation context string
        // AND the to_bytes accessor. Cross-language conformance vector for Task 3.2.
        // hex: f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a14
        let seed = [0x01u8; 32];
        let expected: [u8; 32] = [
            0xf3, 0x95, 0x82, 0xd2, 0x47, 0xfa, 0x3b, 0xb8, 0x4a, 0x45, 0x22, 0x49, 0x43, 0xd9,
            0xf0, 0x58, 0xb8, 0x65, 0x0b, 0xed, 0x6d, 0x66, 0x40, 0xe3, 0xa6, 0x91, 0x65, 0xa1,
            0x47, 0x38, 0x3a, 0x14,
        ];
        assert_eq!(BackupKey::derive(&seed).to_bytes(), expected);
    }

    #[test]
    fn index_master_key_derive_pins_known_vector() {
        // Known-answer test — pins BLAKE3 "fauna.index.master.v1 2026-08-04"
        // derivation from the all-0x01 seed (the same conformance seed as the
        // BackupKey KAT above). Guards the generation-0 context string, which
        // is frozen: editing it orphans every master-class index segment.
        // hex: 678b2341a32abc1661a5d95a0d2ca59cc4ef8ce35f354f8e3a2738d8aed19150
        let seed = [0x01u8; 32];
        let expected: [u8; 32] = [
            0x67, 0x8b, 0x23, 0x41, 0xa3, 0x2a, 0xbc, 0x16, 0x61, 0xa5, 0xd9, 0x5a, 0x0d, 0x2c,
            0xa5, 0x9c, 0xc4, 0xef, 0x8c, 0xe3, 0x5f, 0x35, 0x4f, 0x8e, 0x3a, 0x27, 0x38, 0xd8,
            0xae, 0xd1, 0x91, 0x50,
        ];
        // `*` derefs the `Zeroizing` custody wrapper the derivation returns —
        // the KAT compares key bytes, not custody.
        assert_eq!(*derive_index_master_key(&seed), expected);
        // Domain separation from the two seed-derived siblings: same seed,
        // different context, cryptographically independent output.
        assert_ne!(
            *derive_index_master_key(&seed),
            BackupKey::derive(&seed).to_bytes()
        );
        assert_ne!(
            *derive_index_master_key(&seed),
            NestBackupKey::derive(&seed).to_bytes()
        );
    }

    // ── Account-state key schedule (T14 / Path A-sibling-2) ──────────────────

    #[test]
    fn account_state_seal_root_pins_known_vector() {
        // Known-answer test — pins BLAKE3 "fauna.account-state.seal.v1 2026-08-10"
        // over the BackupKey derived from the all-0x01 conformance seed (the same
        // seed as the BackupKey / index-master KATs above), so this vector pins the
        // WHOLE chain seed → BackupKey → seal root. The context string is frozen for
        // the life of generation 0: editing it orphans every sealed account-state
        // entry at rest and on every custodian.
        let backup_key = BackupKey::derive(&[0x01u8; 32]);
        let schedule = AccountStateKeySchedule::derive(&backup_key);
        // hex: a0227e063aafa0fcb1814c0c33f7b925b41ac7beb6fa3c530cc86aa2079afaff
        let expected: [u8; 32] = [
            0xa0, 0x22, 0x7e, 0x06, 0x3a, 0xaf, 0xa0, 0xfc, 0xb1, 0x81, 0x4c, 0x0c, 0x33, 0xf7,
            0xb9, 0x25, 0xb4, 0x1a, 0xc7, 0xbe, 0xb6, 0xfa, 0x3c, 0x53, 0x0c, 0xc8, 0x6a, 0xa2,
            0x07, 0x9a, 0xfa, 0xff,
        ];
        assert_eq!(**schedule.delegable().seal_root(), expected);
    }

    #[test]
    fn account_state_item_blind_root_pins_known_vector() {
        // Companion KAT for "fauna.account-state.item-key.v1 2026-08-10". Same
        // freeze discipline: editing it makes every stored item key unroutable.
        let backup_key = BackupKey::derive(&[0x01u8; 32]);
        let schedule = AccountStateKeySchedule::derive(&backup_key);
        // hex: 73c8b22784cbd44f9c69ccd024f62ecffd7fd5a3857a612ae1ccf2f7310aa28d
        let expected: [u8; 32] = [
            0x73, 0xc8, 0xb2, 0x27, 0x84, 0xcb, 0xd4, 0x4f, 0x9c, 0x69, 0xcc, 0xd0, 0x24, 0xf6,
            0x2e, 0xcf, 0xfd, 0x7f, 0xd5, 0xa3, 0x85, 0x7a, 0x61, 0x2a, 0xe1, 0xcc, 0xf2, 0xf7,
            0x31, 0x0a, 0xa2, 0x8d,
        ];
        assert_eq!(**schedule.delegable().item_blind_root(), expected);
    }

    #[test]
    fn account_state_fleet_only_roots_pin_known_vectors() {
        // The R13 fleet-only branch's two KATs, same conformance seed and the same
        // freeze discipline as the delegable pair above: these strings are frozen
        // for the life of generation 0, because editing either orphans every
        // fleet-only entry at rest and on every custodian holding a relayed copy.
        //
        // Filled from a first run — the point of a KAT is that it can only ever be
        // written once. If this test fails, a context string moved; do not update
        // the vector, mint a generation instead.
        let backup_key = BackupKey::derive(&[0x01u8; 32]);
        let schedule = AccountStateKeySchedule::derive(&backup_key);

        // hex: 919f2c5326029ab8e298504a350e9f65987873483d348a353ba93f96de107667
        let expected_seal: [u8; 32] = [
            0x91, 0x9f, 0x2c, 0x53, 0x26, 0x02, 0x9a, 0xb8, 0xe2, 0x98, 0x50, 0x4a, 0x35, 0x0e,
            0x9f, 0x65, 0x98, 0x78, 0x73, 0x48, 0x3d, 0x34, 0x8a, 0x35, 0x3b, 0xa9, 0x3f, 0x96,
            0xde, 0x10, 0x76, 0x67,
        ];
        // hex: 6e81f1b24158ebf8b77d67a8e5c37c3f05b5b45bfbde7cad5f6e19f91d7b465b
        let expected_blind: [u8; 32] = [
            0x6e, 0x81, 0xf1, 0xb2, 0x41, 0x58, 0xeb, 0xf8, 0xb7, 0x7d, 0x67, 0xa8, 0xe5, 0xc3,
            0x7c, 0x3f, 0x05, 0xb5, 0xb4, 0x5b, 0xfb, 0xde, 0x7c, 0xad, 0x5f, 0x6e, 0x19, 0xf9,
            0x1d, 0x7b, 0x46, 0x5b,
        ];

        assert_eq!(**schedule.fleet_only().seal_root(), expected_seal);
        assert_eq!(**schedule.fleet_only().item_blind_root(), expected_blind);
    }

    #[test]
    fn the_two_branches_are_cryptographically_independent() {
        // The load-bearing R13 property: a grant carries delegable material only,
        // so possession of every delegable key must reveal nothing about any
        // fleet-only key. Domain separation via distinct context strings is what
        // buys that — this test is what stops a refactor from collapsing the two
        // branches onto one root "since they derive the same way".
        let schedule = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x55u8; 32]));

        assert_ne!(
            **schedule.delegable().seal_root(),
            **schedule.fleet_only().seal_root()
        );
        assert_ne!(
            **schedule.delegable().item_blind_root(),
            **schedule.fleet_only().item_blind_root()
        );
        // Cross-axis too: the fleet seal root must not equal the delegable blind
        // root, or a naming position on one branch would read on the other.
        assert_ne!(
            **schedule.fleet_only().seal_root(),
            **schedule.delegable().item_blind_root()
        );
        assert_ne!(
            **schedule.delegable().seal_root(),
            **schedule.fleet_only().item_blind_root()
        );

        // And the same kind string on the two branches yields unrelated keys —
        // the property that makes a mis-registered rung fail closed (an entry
        // sealed on the wrong branch simply does not open) rather than silently
        // widening an audience.
        let kind = "fauna.state.example";
        let delegable = schedule.delegable().for_kind(kind);
        let fleet = schedule.fleet_only().for_kind(kind);
        assert_ne!(delegable.entry_key(), fleet.entry_key());
        assert_ne!(delegable.item_key(b"mail"), fleet.item_key(b"mail"));
    }

    #[test]
    fn for_rung_routes_to_the_registered_branch() {
        // `for_rung` is the fan-out every production path reaches through
        // (`fauna_protocol::merge_policy::kind_keys` does the registry lookup).
        // It must agree exactly with the branch accessors — a routing bug here
        // seals an operational secret under a grantable key.
        let schedule = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x66u8; 32]));
        let kind = "fauna.account.settings";

        assert_eq!(
            schedule.for_rung(AudienceRung::Delegable, kind).entry_key(),
            schedule.delegable().for_kind(kind).entry_key()
        );
        assert_eq!(
            schedule.for_rung(AudienceRung::FleetOnly, kind).entry_key(),
            schedule.fleet_only().for_kind(kind).entry_key()
        );
        assert_ne!(
            schedule.for_rung(AudienceRung::Delegable, kind).entry_key(),
            schedule.for_rung(AudienceRung::FleetOnly, kind).entry_key()
        );
    }

    // ── Group machinery root (T20 sealing ruling, 2026-08-17) ────────────────

    #[test]
    fn group_machinery_roots_pin_known_vectors() {
        // KATs for the two group-named context strings, from the all-0x5A
        // conformance root. Filled from a first run — a KAT can only ever be
        // written once. If this test fails, a context string moved; do not
        // update the vector: every group's sealed machinery is orphaned, and
        // there is no re-mint (the root never rotates).
        let root = GroupMachineryRoot::from_bytes([0x5A; 32]);
        let schedule = GroupMachinerySchedule::derive(&root);
        // hex: 589682ce98b4b1821a76ba4f9cafbb37536b660d836857f475700f09e28fc4a4
        let expected_seal: [u8; 32] = [
            0x58, 0x96, 0x82, 0xce, 0x98, 0xb4, 0xb1, 0x82, 0x1a, 0x76, 0xba, 0x4f, 0x9c, 0xaf,
            0xbb, 0x37, 0x53, 0x6b, 0x66, 0x0d, 0x83, 0x68, 0x57, 0xf4, 0x75, 0x70, 0x0f, 0x09,
            0xe2, 0x8f, 0xc4, 0xa4,
        ];
        // hex: 0071d9b8017c67e4d1386e18e2b48504633f7d1ee340174792ae83734b478c31
        let expected_blind: [u8; 32] = [
            0x00, 0x71, 0xd9, 0xb8, 0x01, 0x7c, 0x67, 0xe4, 0xd1, 0x38, 0x6e, 0x18, 0xe2, 0xb4,
            0x85, 0x04, 0x63, 0x3f, 0x7d, 0x1e, 0xe3, 0x40, 0x17, 0x47, 0x92, 0xae, 0x83, 0x73,
            0x4b, 0x47, 0x8c, 0x31,
        ];
        assert_eq!(**schedule.seal_root(), expected_seal);
        assert_eq!(**schedule.item_blind_root(), expected_blind);
    }

    #[test]
    fn the_group_machinery_branch_is_independent_of_every_account_branch() {
        // The plane split's load-bearing property: the same 32 bytes pushed
        // through the group contexts and the account contexts must yield
        // unrelated roots on every axis — a member who holds a group's
        // machinery root learns nothing about any account-state branch, and
        // vice versa.
        let bytes = [0x55u8; 32];
        let group = GroupMachinerySchedule::derive(&GroupMachineryRoot::from_bytes(bytes));
        let account = AccountStateKeySchedule::derive(&BackupKey::from_bytes(bytes));
        for account_root in [
            &**account.delegable().seal_root(),
            &**account.delegable().item_blind_root(),
            &**account.fleet_only().seal_root(),
            &**account.fleet_only().item_blind_root(),
        ] {
            assert_ne!(&**group.seal_root(), account_root);
            assert_ne!(&**group.item_blind_root(), account_root);
        }
        // The two group axes are split from each other too, like every
        // schedule here.
        assert_ne!(**group.seal_root(), **group.item_blind_root());
    }

    #[test]
    fn the_machinery_root_commitment_is_deterministic_and_domain_separated() {
        // The commitment is the id↔root binding's public half: deterministic
        // (two derivations of one root agree), moved by the root (a
        // substituted root is a crisp mismatch), and domain-separated from the
        // R14 generation-key commitment of the same bytes (a commitment made
        // in one plane must never verify in the other).
        let root = GroupMachineryRoot::from_bytes([0x5A; 32]);
        assert_eq!(root.commitment(), root.commitment());
        assert_ne!(
            root.commitment(),
            GroupMachineryRoot::from_bytes([0x5B; 32]).commitment()
        );
        assert_ne!(
            root.commitment(),
            blake3::derive_key(GENERATION_KEY_COMMIT_CONTEXT, &[0x5A; 32])
        );
    }

    #[test]
    fn account_state_roots_are_domain_separated() {
        // The two roots must be independent of each other AND of every other key
        // derived from the same material — otherwise a capability grant for one
        // axis (naming) would confer the other (reading).
        let seed = [0x01u8; 32];
        let backup_key = BackupKey::derive(&seed);
        let schedule = AccountStateKeySchedule::derive(&backup_key);
        let seal_root = **schedule.delegable().seal_root();
        let blind_root = **schedule.delegable().item_blind_root();

        assert_ne!(seal_root, blind_root);
        // Independent of the root BackupKey itself and its other derivations.
        assert_ne!(seal_root, backup_key.to_bytes());
        assert_ne!(blind_root, backup_key.to_bytes());
        assert_ne!(seal_root, backup_key.convergent_chunk_root());
        assert_ne!(blind_root, backup_key.convergent_chunk_root());
        // Independent of the two sibling seed-derived keys.
        assert_ne!(seal_root, NestBackupKey::derive(&seed).to_bytes());
        assert_ne!(seal_root, *derive_index_master_key(&seed));
        assert_ne!(blind_root, NestBackupKey::derive(&seed).to_bytes());
        assert_ne!(blind_root, *derive_index_master_key(&seed));
    }

    #[test]
    fn generation_key_commitment_pins_known_vector() {
        // Known-answer test — pins BLAKE3 "fauna.generation.key-commit.v1
        // 2026-08-13" over the all-0x02 key. Frozen: every unwrap recomputes
        // the commitment against its mint, so moving this string makes every
        // honest generation key verify as substituted. Filled from a first
        // run; if this fails, do not update the vector — the string moved.
        // hex: ffd83ea262c623c4ad861fe94189730a38a0c57dca7332fae420db943526cee7
        let expected: [u8; 32] = [
            0xff, 0xd8, 0x3e, 0xa2, 0x62, 0xc6, 0x23, 0xc4, 0xad, 0x86, 0x1f, 0xe9, 0x41, 0x89,
            0x73, 0x0a, 0x38, 0xa0, 0xc5, 0x7d, 0xca, 0x73, 0x32, 0xfa, 0xe4, 0x20, 0xdb, 0x94,
            0x35, 0x26, 0xce, 0xe7,
        ];
        assert_eq!(
            GenerationKey::from_bytes([0x02u8; 32]).commitment(),
            expected
        );
        // The commitment is one-way ONTO a different value (not the key), and
        // distinct keys commit distinctly.
        assert_ne!(
            GenerationKey::from_bytes([0x02u8; 32]).commitment(),
            [0x02u8; 32]
        );
        assert_ne!(
            GenerationKey::from_bytes([0x02u8; 32]).commitment(),
            GenerationKey::from_bytes([0x03u8; 32]).commitment()
        );
    }

    #[test]
    fn generation_fleet_branch_reuses_the_frozen_strings_over_new_key_material() {
        // R14's ratified rotation shape, pinned: the generation-N fleet branch
        // derives under the SAME two frozen context strings as generation 0 —
        // feeding the gen key's bytes in as if they were a BackupKey's must
        // yield byte-identical roots (independence comes from key material,
        // never from new strings). And a real random-keyed generation is
        // independent of the gen-0 branch by that key material alone.
        let bytes = [0x21u8; 32];
        let via_generation =
            FleetOnlySchedule::derive_for_generation(&GenerationKey::from_bytes(bytes));
        let via_backup_root = AccountStateKeySchedule::derive(&BackupKey::from_bytes(bytes));
        assert_eq!(
            **via_generation.seal_root(),
            **via_backup_root.fleet_only().seal_root(),
            "same root bytes + same frozen strings ⇒ same seal root"
        );
        assert_eq!(
            **via_generation.item_blind_root(),
            **via_backup_root.fleet_only().item_blind_root()
        );

        let gen0 = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x22u8; 32]));
        assert_ne!(
            **via_generation.seal_root(),
            **gen0.fleet_only().seal_root(),
            "different key material ⇒ independent generations"
        );
        // Per-kind fan-out flows through unchanged.
        let kind = "fauna.state.device-endpoints";
        assert_ne!(
            via_generation.for_kind(kind).entry_key(),
            gen0.fleet_only().for_kind(kind).entry_key()
        );
    }

    #[test]
    fn one_fleet_only_kinds_generation_0_keys_are_the_schedules_own() {
        // The succession rider's open-only pair for a predecessor's mint rows
        // is the pair that identity's own schedule seals the kind under — and
        // only that kind's.
        let key = BackupKey::from_bytes([0x24u8; 32]);
        let kind = "fauna.state.generation-mint";
        let narrow = FleetOnlySchedule::generation_0_kind_keys(&key, kind);
        let schedule = AccountStateKeySchedule::derive(&key);
        let whole = schedule.fleet_only().for_kind(kind);
        assert_eq!(narrow.kind(), kind);
        assert_eq!(narrow.entry_key(), whole.entry_key());
        assert_eq!(narrow.item_key(b"g"), whole.item_key(b"g"));
        assert_ne!(
            narrow.entry_key(),
            schedule
                .fleet_only()
                .for_kind("fauna.state.device-set")
                .entry_key(),
            "another fleet-only kind's rows do not open under it"
        );
        assert_ne!(
            narrow.entry_key(),
            schedule.delegable().for_kind(kind).into_keys().entry_key(),
            "nor does the delegable branch's fan-out for the same string"
        );
    }

    #[test]
    fn minted_generation_keys_are_random_and_commit_distinctly() {
        let a = GenerationKey::mint();
        let b = GenerationKey::mint();
        assert_ne!(a.as_bytes(), b.as_bytes());
        assert_ne!(a.commitment(), b.commitment());
    }

    #[test]
    fn account_state_schedule_is_deterministic_from_backup_key() {
        // A seedless enrolled device derives the whole schedule from the BackupKey
        // in its bundle — the forcing property of rooting one step below the seed
        // (owner-key-material.md § Path A-sibling-2 → Root).
        let a = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x33u8; 32]));
        let b = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x33u8; 32]));
        assert_eq!(**a.delegable().seal_root(), **b.delegable().seal_root());
        assert_eq!(
            **a.delegable().item_blind_root(),
            **b.delegable().item_blind_root()
        );
        // Both branches, or a device could converge on settings and diverge on
        // operational secrets.
        assert_eq!(**a.fleet_only().seal_root(), **b.fleet_only().seal_root());
        assert_eq!(
            **a.fleet_only().item_blind_root(),
            **b.fleet_only().item_blind_root()
        );

        let other = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x34u8; 32]));
        assert_ne!(**a.delegable().seal_root(), **other.delegable().seal_root());
        assert_ne!(
            **a.delegable().item_blind_root(),
            **other.delegable().item_blind_root()
        );
        assert_ne!(
            **a.fleet_only().seal_root(),
            **other.fleet_only().seal_root()
        );
    }

    #[test]
    fn a_delegable_schedule_derived_alone_is_the_full_schedules_delegable_half() {
        // The successor's walk holds a predecessor's delegable branch and no
        // fleet branch (succession-aftermath.md § Re-key scope): the lone
        // derivation must open exactly what the full schedule's delegable half
        // seals, kind by kind, and share nothing with its fleet-only half.
        let backup_key = BackupKey::from_bytes([0x35u8; 32]);
        let alone = DelegableSchedule::derive(&backup_key);
        let full = AccountStateKeySchedule::derive(&backup_key);
        for kind in ["fauna.state.moderation", "fauna.state.read-marker"] {
            assert_eq!(
                alone.for_kind(kind).to_grant(),
                full.delegable().for_kind(kind).to_grant(),
                "{kind}"
            );
        }
        assert_ne!(**alone.seal_root(), **full.fleet_only().seal_root());
        assert_ne!(
            **alone.item_blind_root(),
            **full.fleet_only().item_blind_root()
        );
    }

    #[test]
    fn per_kind_fan_out_separates_kinds() {
        // The grant unit is per kind: holding {entry_key(K), item_blind(K)} must
        // confer exactly K — neither reading nor naming any other kind's entries.
        let schedule = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x07u8; 32]));
        let delegable = schedule.delegable();
        let (a_entry, a_blind) = delegable.for_kind("fauna.account.settings").to_grant();
        let (b_entry, b_blind) = delegable.for_kind("fauna.account.seen-set").to_grant();

        assert_ne!(a_entry, b_entry);
        assert_ne!(a_blind, b_blind);
        // Within one kind the two axes stay independent.
        assert_ne!(a_entry, a_blind);
        // And neither equals the root it fanned out from.
        assert_ne!(a_entry, **delegable.seal_root());
        assert_ne!(a_blind, **delegable.item_blind_root());
    }

    #[test]
    fn item_keys_are_stable_opaque_and_kind_scoped() {
        // Stable: relays route, retain latest-per-writer and dedup on these.
        // Kind-scoped: the same logical key under two kinds must not collide, or a
        // custodian could correlate one setting across kinds.
        let schedule = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x09u8; 32]));
        let settings = schedule.delegable().for_kind("fauna.account.settings");
        let seen = schedule.delegable().for_kind("fauna.account.seen-set");

        assert_eq!(
            settings.item_key(b"notify/quiet-hours"),
            settings.item_key(b"notify/quiet-hours"),
            "item keys must be stable — routing and supersession depend on it"
        );
        assert_ne!(
            settings.item_key(b"notify/quiet-hours"),
            settings.item_key(b"notify/badge-count")
        );
        assert_ne!(
            settings.item_key(b"notify/quiet-hours"),
            seen.item_key(b"notify/quiet-hours"),
            "the same logical key under two kinds must not collide"
        );
    }

    #[test]
    fn a_grant_round_trips_and_confers_exactly_its_kind() {
        // The capability-grant path (encryption-at-rest.md § Capability tiering):
        // a granted position reconstructs the pair from bytes and derives identical
        // item keys, with no access to the roots.
        let schedule = AccountStateKeySchedule::derive(&BackupKey::from_bytes([0x21u8; 32]));
        let full = schedule.delegable().for_kind("fauna.account.settings");

        let (entry_key, item_blind) = full.to_grant();
        let granted =
            AccountStateKindKeys::from_grant("fauna.account.settings", entry_key, item_blind);

        assert_eq!(granted.kind(), full.kind());
        assert_eq!(granted.entry_key(), full.entry_key());
        assert_eq!(
            granted.item_key(b"notify/quiet-hours"),
            full.item_key(b"notify/quiet-hours")
        );
    }

    /// Compile-time pin: the account-state roots are non-`Copy`, zeroize-on-drop.
    ///
    /// Same mechanism (and the same rationale) as
    /// [`_INDEX_MASTER_KEY_IS_NOT_COPY`] above — a bare `[u8; 32]` is `Copy`, so
    /// it duplicates silently on every move and never zeroizes. Reverting either
    /// accessor to a bare array must fail the build here. Pinned on **both**
    /// branches: the fleet-only branch holds operational secrets, so it is the
    /// one that must never regress to a `Copy` array.
    const _ACCOUNT_STATE_ROOTS_ARE_NOT_COPY: fn(&DelegableSchedule) -> &Zeroizing<[u8; 32]> =
        DelegableSchedule::seal_root;
    const _FLEET_ONLY_ROOTS_ARE_NOT_COPY: fn(&FleetOnlySchedule) -> &Zeroizing<[u8; 32]> =
        FleetOnlySchedule::seal_root;

    /// Compile-time pin for the R13 grant boundary: `to_grant` exists on
    /// [`DelegableKindKeys`] and **only** there.
    ///
    /// The negative half cannot be written as a passing assertion in Rust (there
    /// is no "this method does not exist" expression), so the guard is
    /// structural: [`FleetOnlySchedule::for_kind`] returns
    /// [`AccountStateKindKeys`], which has no `to_grant`, and this pin fails to
    /// compile if someone "helpfully" moves the method down to the shared type —
    /// at which point the pin's type would no longer be the delegable one.
    const _GRANTS_MINT_ONLY_FROM_THE_DELEGABLE_BRANCH: fn(
        &DelegableKindKeys,
    ) -> ([u8; 32], [u8; 32]) = DelegableKindKeys::to_grant;

    // ── convergent_chunk_root (FS-BIND FOLLOW-ON A) ──────────────────────────

    /// The crypto-layer pin for FS-BIND FOLLOW-ON A: a chunk sealed through the
    /// convergent `chunk_crypto` primitive under `convergent_chunk_root` and
    /// keyed by its **ciphertext** hash satisfies the nest chunk route's F9
    /// anti-poisoning predicate (`blake3(body) == X-Content-Hash`), while the
    /// legacy random-nonce `encrypt_backup_chunk` frame claiming the plaintext
    /// hash does NOT (it is neither the raw hash nor a valid zstd decompression
    /// — the frame's 0x01 version byte collides with `PREFIX_ZSTD` but the AEAD
    /// bytes fail to decompress). This is the in-crate replica of the route
    /// check in `bins/fauna-nest/src/chunk_routes.rs::resolve_verified_chunk_hash`.
    #[test]
    fn convergent_backup_seal_satisfies_f9_route_predicate() {
        let f9_accepts = |body: &[u8], claimed: &[u8; 32]| -> bool {
            let raw_ok = blake3::hash(body).as_bytes() == claimed;
            let decompressed_ok = crate::compress::decompress_chunk_bounded(
                body,
                crate::compress::MAX_DECOMPRESSED_CHUNK,
            )
            .map(|p| blake3::hash(&p).as_bytes() == claimed)
            .unwrap_or(false);
            raw_ok || decompressed_ok
        };

        let key = BackupKey::from_bytes([0x61u8; 32]);
        let plaintext = b"cross-location segment backup chunk payload".to_vec();
        let plain_hash = crate::data::ContentHash::of_raw(&plaintext);

        // Convergent seal keyed by ciphertext hash → accepted.
        let root = key.convergent_chunk_root();
        let ct = crate::chunk_crypto::encrypt_chunk(&root, &plain_hash, &plaintext).unwrap();
        let store_key = crate::data::ContentHash::of_raw(&ct);
        assert!(
            f9_accepts(&ct, &store_key.digest()),
            "a convergent backup seal keyed by its ciphertext hash must pass the F9 check"
        );

        // Legacy framed seal claiming the plaintext hash → rejected (the
        // live-broken shape).
        let framed = encrypt_backup_chunk(&key, &plaintext).unwrap();
        assert!(
            !f9_accepts(&framed, &plain_hash.digest()),
            "the legacy random-nonce frame claiming the plaintext hash must fail the F9 check"
        );

        // Round-trip under the same root.
        let opened = crate::chunk_crypto::decrypt_chunk(&root, &plain_hash, &ct).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn convergent_chunk_root_is_deterministic_and_domain_separated() {
        let key = BackupKey::from_bytes([0x62u8; 32]);
        // Deterministic per key (the convergence property rests on it) …
        assert_eq!(key.convergent_chunk_root(), key.convergent_chunk_root());
        // … distinct from the raw AEAD key bytes (domain separation) …
        assert_ne!(key.convergent_chunk_root(), key.to_bytes());
        // … and distinct across keys (no cross-owner equality leak).
        let other = BackupKey::from_bytes([0x63u8; 32]);
        assert_ne!(key.convergent_chunk_root(), other.convergent_chunk_root());
    }

    #[test]
    fn convergent_backup_seal_is_stable_across_calls() {
        // Identical plaintext under one owner root seals to byte-identical
        // ciphertext — the stable content address that preserves destination-side
        // dedup + idempotent retry (unlike `encrypt_backup_chunk`'s random nonce).
        let key = BackupKey::from_bytes([0x64u8; 32]);
        let root = key.convergent_chunk_root();
        let plaintext = b"identical chunk".to_vec();
        let h = crate::data::ContentHash::of_raw(&plaintext);
        let ct1 = crate::chunk_crypto::encrypt_chunk(&root, &h, &plaintext).unwrap();
        let ct2 = crate::chunk_crypto::encrypt_chunk(&root, &h, &plaintext).unwrap();
        assert_eq!(ct1, ct2);
    }

    // ── NestBackupKey (Path A-sibling-0) ──────────────────────────────────────

    #[test]
    fn nest_backup_key_derive_is_deterministic() {
        // Determinism is what makes restore survive total device loss: the key is
        // re-derivable from the seed alone, never escrowed anywhere.
        let seed = [0x42u8; 32];
        assert_eq!(
            NestBackupKey::derive(&seed).to_bytes(),
            NestBackupKey::derive(&seed).to_bytes(),
        );
    }

    #[test]
    fn nest_backup_key_different_seeds_produce_different_keys() {
        let a = NestBackupKey::derive(&[0x01u8; 32]);
        let b = NestBackupKey::derive(&[0x02u8; 32]);
        assert_ne!(
            a.to_bytes(),
            b.to_bytes(),
            "different seeds must not collide"
        );
    }

    /// **The load-bearing separation.** `NestBackupKey` is deliberately handed to
    /// the user's own source nest; `BackupKey` must remain never-nest-held
    /// (`key-material-hierarchy.md` § Path A-sibling-0 / § Path A). Holding one
    /// must therefore reveal nothing about the other — they are independent
    /// BLAKE3 `derive_key` outputs over distinct context strings, so a nest
    /// holding `NestBackupKey` cannot reach the client-originated folder,
    /// `__drafts`, or held-for-friends backups.
    #[test]
    fn nest_backup_key_is_domain_separated_from_backup_key() {
        let seed = [0x42u8; 32];
        assert_ne!(
            NestBackupKey::derive(&seed).to_bytes(),
            BackupKey::derive(&seed).to_bytes(),
            "the nest-held key must never equal the never-nest-held BackupKey",
        );
        // Their convergent roots are independent too — else a nest could confirm
        // equality of the owner's client-originated chunks.
        assert_ne!(
            NestBackupKey::derive(&seed).convergent_chunk_root(),
            BackupKey::derive(&seed).convergent_chunk_root(),
        );
    }

    #[test]
    fn nest_backup_key_from_bytes_roundtrip() {
        let raw = [0xABu8; 32];
        assert_eq!(NestBackupKey::from_bytes(raw).to_bytes(), raw);
    }

    #[test]
    fn nest_backup_key_derive_pins_known_vector() {
        // Known-answer test — pins the ratified BLAKE3 context string
        // "fauna nest backup key 2026-07-23" (`key-material-hierarchy.md`
        // § Path A-sibling-0) from an all-0x01 seed. A context-string edit is a
        // silent at-rest break for every already-uploaded backup, so it is
        // pinned here and cross-checked against `BackupKey`'s own vector above.
        // hex: d81340db36e226c4238aa34f6afdf0a80b3e6077bc6429903b3708e03ff1ae80
        let seed = [0x01u8; 32];
        let expected: [u8; 32] = [
            0xd8, 0x13, 0x40, 0xdb, 0x36, 0xe2, 0x26, 0xc4, 0x23, 0x8a, 0xa3, 0x4f, 0x6a, 0xfd,
            0xf0, 0xa8, 0x0b, 0x3e, 0x60, 0x77, 0xbc, 0x64, 0x29, 0x90, 0x3b, 0x37, 0x08, 0xe0,
            0x3f, 0xf1, 0xae, 0x80,
        ];
        assert_eq!(NestBackupKey::derive(&seed).to_bytes(), expected);
    }

    #[test]
    fn nest_backup_key_convergent_root_is_deterministic_and_domain_separated() {
        let key = NestBackupKey::from_bytes([0x62u8; 32]);
        assert_eq!(key.convergent_chunk_root(), key.convergent_chunk_root());
        assert_ne!(key.convergent_chunk_root(), key.to_bytes());
        let other = NestBackupKey::from_bytes([0x63u8; 32]);
        assert_ne!(key.convergent_chunk_root(), other.convergent_chunk_root());
    }

    /// Destinations accept `NestBackupKey`-sealed chunks through exactly the same
    /// F9 anti-poisoning route as `BackupKey` chunks (design record § The
    /// decision: "same acceptance as `BackupKey` chunks") — so the nest-side
    /// coordinator needs no new chunk-route arm on the destination.
    #[test]
    fn nest_backup_convergent_seal_satisfies_f9_route_predicate() {
        let key = NestBackupKey::from_bytes([0x61u8; 32]);
        let plaintext = b"nest-sealed mail segment payload".to_vec();
        let plain_hash = crate::data::ContentHash::of_raw(&plaintext);

        let root = key.convergent_chunk_root();
        let ct = crate::chunk_crypto::encrypt_chunk(&root, &plain_hash, &plaintext).unwrap();
        let store_key = crate::data::ContentHash::of_raw(&ct);
        assert_eq!(
            blake3::hash(&ct).as_bytes(),
            &store_key.digest(),
            "the ciphertext must be its own stable content address (F9)",
        );

        let opened = crate::chunk_crypto::decrypt_chunk(&root, &plain_hash, &ct).unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn nest_backup_convergent_seal_is_stable_across_calls() {
        // Idempotent retry + per-owner dedup at the destination rest on this.
        let key = NestBackupKey::from_bytes([0x64u8; 32]);
        let root = key.convergent_chunk_root();
        let plaintext = b"identical segment chunk".to_vec();
        let h = crate::data::ContentHash::of_raw(&plaintext);
        assert_eq!(
            crate::chunk_crypto::encrypt_chunk(&root, &h, &plaintext).unwrap(),
            crate::chunk_crypto::encrypt_chunk(&root, &h, &plaintext).unwrap(),
        );
    }

    // ── OwnerSealKey ─────────────────────────────────────────────────────────

    /// The enum must be a pure pass-through: an engine holding
    /// `OwnerSealKey::Client(k)` must seal byte-identically to one that held the
    /// bare `BackupKey` before this indirection existed, or the refactor would
    /// silently orphan every chunk already at rest on every user's destination.
    #[test]
    fn owner_seal_key_preserves_each_variants_root() {
        let seed = [0x21u8; 32];

        let client = BackupKey::derive(&seed);
        let client_root = client.convergent_chunk_root();
        assert_eq!(
            OwnerSealKey::from(client).convergent_chunk_root(),
            client_root,
        );

        let nest = NestBackupKey::derive(&seed);
        let nest_root = nest.convergent_chunk_root();
        assert_eq!(OwnerSealKey::from(nest).convergent_chunk_root(), nest_root);
    }

    /// FS-5DC, pinned at its one owner: a **bound** set never yields an owner
    /// key, no matter how much owner material the holder carries. This is the
    /// exact shadowing the bearer-only hydration service produced live — an
    /// engine holding `backup_key = Some` *and* a bound set's `mls_group_id` —
    /// and it must resolve to the content-key path, not the owner key.
    #[test]
    fn a_bound_set_yields_no_owner_key_even_holding_one() {
        let key = OwnerSealKey::from(BackupKey::derive(&[0x31u8; 32]));
        assert!(effective_owner_key(Some(b"group-1"), false, Some(&key)).is_none());
        // An empty group id is still a group id — the marker is presence, never
        // its contents.
        assert!(effective_owner_key(Some(b""), false, Some(&key)).is_none());
        // Holding the keys as well changes nothing on this arm.
        assert!(effective_owner_key(Some(b"group-1"), true, Some(&key)).is_none());
    }

    /// The second time this rule failed by being written narrowly: a
    /// WebDAV-served set with no MLS group holds M2 content keys at the serve
    /// pseudo-channel, and its chunks are content-keyed by design — the owner
    /// key it also holds must not shadow them, or the app's engine seals what
    /// the set's DAV reader (content key only) can never open.
    #[test]
    fn a_served_group_less_set_holding_content_keys_yields_no_owner_key() {
        let key = OwnerSealKey::from(BackupKey::derive(&[0x33u8; 32]));
        assert!(effective_owner_key(None, true, Some(&key)).is_none());
    }

    /// The unbound arm is the one that must keep working: an owner-only set
    /// hands back exactly the key it holds, so the delegating call sites stay
    /// byte-for-byte what they were before this became one function.
    #[test]
    fn an_unbound_set_yields_the_owner_key_it_holds() {
        let key = OwnerSealKey::from(BackupKey::derive(&[0x32u8; 32]));
        let got = effective_owner_key(None, false, Some(&key))
            .expect("an unbound holder with a key yields it");
        assert_eq!(got.convergent_chunk_root(), key.convergent_chunk_root());

        let nest = OwnerSealKey::from(NestBackupKey::derive(&[0x32u8; 32]));
        let got = effective_owner_key(None, false, Some(&nest))
            .expect("the source-nest variant is an owner key too");
        assert_eq!(got.convergent_chunk_root(), nest.convergent_chunk_root());
    }

    /// A keyless holder yields nothing on either arm — the callers' fall-through
    /// to the content-key path (and their fail-closed posture there) is what
    /// makes this safe, so it must not start inventing a key.
    #[test]
    fn a_keyless_holder_yields_no_owner_key_on_either_arm() {
        assert!(effective_owner_key(None, false, None).is_none());
        assert!(effective_owner_key(Some(b"group-1"), false, None).is_none());
        assert!(effective_owner_key(None, true, None).is_none());
    }

    /// The two variants never collapse — an engine on the nest key must not be
    /// able to open (or be confused for) one on the client key.
    #[test]
    fn owner_seal_key_variants_are_distinct() {
        let seed = [0x22u8; 32];
        assert_ne!(
            OwnerSealKey::from(BackupKey::derive(&seed)).convergent_chunk_root(),
            OwnerSealKey::from(NestBackupKey::derive(&seed)).convergent_chunk_root(),
        );
    }

    /// A chunk sealed under `NestBackupKey` must not open under `BackupKey` from
    /// the same seed (and vice versa) — the cross-flow split of
    /// `encryption-at-rest.md` § Readable classes, enforced at the crypto layer.
    #[test]
    fn nest_backup_seal_does_not_open_under_backup_key() {
        let seed = [0x77u8; 32];
        let nest_root = NestBackupKey::derive(&seed).convergent_chunk_root();
        let client_root = BackupKey::derive(&seed).convergent_chunk_root();

        let plaintext = b"segment bytes".to_vec();
        let h = crate::data::ContentHash::of_raw(&plaintext);
        let ct = crate::chunk_crypto::encrypt_chunk(&nest_root, &h, &plaintext).unwrap();

        assert!(
            crate::chunk_crypto::decrypt_chunk(&client_root, &h, &ct).is_err(),
            "a NestBackupKey-sealed chunk must not open under BackupKey",
        );
    }
}
