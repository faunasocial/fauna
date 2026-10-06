//! The client-side file-download walk: manifest + chunks by content address →
//! open under the caller's key material → verified plaintext.
//!
//! This is the **one** implementation of the walk every app runs to read a
//! backed-up file's bytes — `docs/goal/behavior/backup-restore.md` § 3 and
//! `docs/goal/ui/backups.md` § Where logic lives → *Single-file byte download*
//! (re-ratified 2026-07-14) name it as shared Rust with per-target seams. It
//! lives in `fauna-core` because every primitive it composes already does
//! ([`chunk`](crate::chunk), [`chunk_crypto`](crate::chunk_crypto),
//! [`chunker`](crate::chunker), [`compress`](crate::compress),
//! [`crypto::BackupKey`](crate::crypto::BackupKey),
//! [`folder_keys`](crate::folder_keys)) — and, decisively, because
//! `fauna-core` compiles to wasm32 while `fauna-sync-engine` (its native home
//! until 2026-07-16) cannot: that crate takes `rusqlite` unconditionally for the
//! `SyncDb` the walk never touches.
//!
//! # Why the walk is client-side
//!
//! Owner-only chunks are sealed unconditionally (2026-07-13), and the nest holds
//! no opening key — so the nest cannot reassemble a file, and no server-side
//! reassembly route exists. The plaintext only ever materialises where the keys
//! live: here.
//!
//! # The seam
//!
//! [`BlobFetcher`] is the one platform-variant leg — how the two content-address
//! GETs (`/api/v1/manifests/{hash}`, `/api/v1/chunks/{hash}`) reach the nest.
//! Native wires it to the sync engine's pooled transfer worker; wasm wires it to
//! the browser's fetch. Chunk fetching is handed to the implementer as a **batch**
//! (`fetch_chunks`) precisely so each target keeps its own concurrency strategy —
//! a `tokio` buffered stream natively, `Promise.all` on the web — without this
//! module depending on a runtime. Mirrors the
//! [`MediaBlobFetcher`](https://docs.rs/) seam in `fauna-media-machine`, down to
//! the `MaybeSendSync` + dual-`async_trait` arm.
//!
//! # Integrity (load-bearing — do not drop the verify)
//!
//! [`download_file_bytes_by_manifest`] verifies the reassembled bytes against the
//! manifest's whole-file content address before returning them. Three anchors are
//! at work, and it matters which covers what:
//!
//! - **The manifest itself is address-checked, and it has to be first.**
//!   [`fetch_manifest`] hashes the served bytes back to the address it asked
//!   for before decoding them. Without that, the other two anchors are
//!   circular: both read their trusted values *out of the fetched document* —
//!   the whole-file compare against `file_hash`, the per-chunk compare against
//!   `chunk_hashes[i]`, and the sealed chunks' key/nonce derivation — so a nest
//!   serving a *different* file's manifest for address M satisfies all of them
//!   against its own numbers. The check also precedes the `is_sealed()` branch,
//!   because `sealed_hashes`/`stored_hashes` are fields of those same
//!   nest-supplied bytes: strip them and the reader's key material is never
//!   consulted at all, so no seal is left to fail.
//! - **Sealed chunks are self-anchoring.** [`chunk_crypto`](crate::chunk_crypto)
//!   derives each chunk's key *and* nonce from its **plaintext** content hash, so
//!   a ciphertext substituted for another chunk simply fails to decrypt — the
//!   seal is content-bound by construction, with no AAD needed. (Contrast the
//!   sibling `MediaBlobFetcher` thumbnail path, whose framed
//!   `decrypt_backup_chunk` seal is *not* content-bound: there a different blob
//!   sealed under the same key decrypts cleanly, and the content-address check is
//!   the only thing that catches the swap — security review. Do not
//!   carry that reasoning over to this walk; the mechanisms differ.)
//! - **A plaintext manifest's chunks have no other anchor.** When
//!   `stored_hashes` is `None`, the stored bytes are passed through
//!   undecrypted, so nothing authenticates the *bodies* but the whole-file
//!   address. It is also what catches a truncated, reordered or short chunk set
//!   on either path.
//!
//! So the whole-file verify is the sole *body* integrity check for plaintext
//! manifests (public-audience folders, raw nest writes) and the backstop for everything else — but it is a backstop
//! only because the document naming it was itself address-checked first. All
//! three arms are pinned by the tests below.

use anyhow::{Context, Result};

use crate::chunk::ChunkManifest;
use crate::crypto::OwnerSealKey;
use crate::data::ContentHash;
use crate::folder_keys::FolderContentKeys;

/// The two content-address GETs the walk needs, as one per-target seam.
///
/// Implementations return the stored bytes **verbatim** — still sealed, still
/// compression-prefixed. All opening, decompression and verification happens in
/// [`download_file_bytes_by_manifest`], so every app shares one policy.
///
/// `MaybeSendSync` + the dual `async_trait` arm let the one seam serve native
/// (`Send + Sync`, spawnable) and wasm (single-threaded, `!Send`) — the identical
/// pattern `fauna-media-machine`'s `MediaBlobFetcher` uses.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait BlobFetcher: crate::MaybeSendSync {
    /// `GET /api/v1/manifests/{hash}` — the canonical-encoded [`ChunkManifest`]
    /// bytes (sealed hash lists included; opening is the caller's job).
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>>;

    /// `GET /api/v1/chunks/{hash}` for each **store key**, in order, returning the
    /// stored bodies parallel to `store_keys`.
    ///
    /// Handed the whole batch so the implementer owns concurrency. `relative_path`
    /// is context for error messages only — never a lookup key.
    ///
    /// A fetcher whose counterparty can move bytes and then fail the call
    /// (a ranged peer pull refused mid-body) attaches
    /// [`TransferredBeforeFailure`] to the error, so a caller metering the
    /// transfer can charge what crossed the wire — see
    /// [`bytes_transferred_before_failure`].
    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> Result<Vec<Vec<u8>>>;
}

/// Error context a [`BlobFetcher`] attaches to a failed call that had already
/// received bytes: how many body bytes arrived before the call gave up.
///
/// A failed transfer still cost its bytes. A caller that meters transfers (the
/// share pump's `p2p-share.transfer` ledger) must charge them, or a
/// counterparty that fails every call on purpose transfers for free; the error
/// is the only thing such a call returns, so the count rides on it. Read it
/// with [`bytes_transferred_before_failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferredBeforeFailure(pub u64);

impl std::fmt::Display for TransferredBeforeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} body bytes arrived before the transfer failed",
            self.0
        )
    }
}

/// The bytes a failed fetch had already received, from its
/// [`TransferredBeforeFailure`] context — 0 when the fetcher attached none.
pub fn bytes_transferred_before_failure(err: &anyhow::Error) -> u64 {
    err.downcast_ref::<TransferredBeforeFailure>()
        .map_or(0, |t| t.0)
}

/// One retired owner root this account succeeded from, with the identity it
/// belongs to when the host knows it — an element of
/// [`FileDownloadKeys::predecessor_backup_keys`].
///
/// `mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(c): a row signed as predecessor **A** is offered only A's own root and
/// the roots of A's predecessors, so every open needs each key **paired with
/// its actor id, in chain order**. A key carried without its id (a host whose
/// source dropped the pairing — `From` an owner key) is still offered to a row
/// signed as the current identity, as before, and never to a predecessor-signed
/// one: an unnamed root cannot be placed in the chain, and the bound fails
/// closed rather than guess.
#[derive(Clone)]
pub struct PredecessorSealKey {
    /// The retired identity this root belongs to; `None` when the host was
    /// handed the key without it.
    pub actor_id: Option<crate::identity::ActorId>,
    /// The retired identity's owner key.
    pub key: OwnerSealKey,
}

impl PredecessorSealKey {
    /// A retired root paired with the identity it belongs to — the shape
    /// `AccountRegistry::predecessor_backup_keys_by_actor` yields.
    pub fn named(actor_id: crate::identity::ActorId, key: impl Into<OwnerSealKey>) -> Self {
        Self {
            actor_id: Some(actor_id),
            key: key.into(),
        }
    }

    /// Pair a chain of `(actor id, key)` entries, nearest hop first.
    pub fn chain<K: Into<OwnerSealKey>>(
        pairs: impl IntoIterator<Item = (crate::identity::ActorId, K)>,
    ) -> Vec<Self> {
        pairs
            .into_iter()
            .map(|(id, key)| Self::named(id, key))
            .collect()
    }

    /// The convergent chunk root of [`key`](Self::key).
    pub fn root(&self) -> [u8; 32] {
        self.key.convergent_chunk_root()
    }
}

impl From<OwnerSealKey> for PredecessorSealKey {
    fn from(key: OwnerSealKey) -> Self {
        Self {
            actor_id: None,
            key,
        }
    }
}

impl From<crate::crypto::BackupKey> for PredecessorSealKey {
    fn from(key: crate::crypto::BackupKey) -> Self {
        OwnerSealKey::from(key).into()
    }
}

impl From<crate::crypto::NestBackupKey> for PredecessorSealKey {
    fn from(key: crate::crypto::NestBackupKey) -> Self {
        OwnerSealKey::from(key).into()
    }
}

/// The identity the **record being opened** was signed as, as the owner-root
/// family's bound reads it ([`FileDownloadKeys::record_signer`]).
///
/// `mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(c). Set by a caller that holds the row's verdict, from its *signed as*
/// — never from the served author.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordSigner {
    /// Signed as this holder's current identity — or a read no verdict
    /// governs (a snapshot download, the sealed selective-sync lists): the
    /// current root and every predecessor root are offered, today's
    /// behaviour.
    #[default]
    Current,
    /// Signed as the retired identity `A` of this account: offered only A's
    /// own root and the roots of A's predecessors — never the current root nor
    /// any root later in the chain than A, and nothing at all when this holder
    /// cannot place A in its paired chain. A failed open is final
    /// ([`PermanentApplyFailure::SIGNER_BOUND`](crate::apply_failure::PermanentApplyFailure::SIGNER_BOUND)).
    Predecessor(crate::identity::ActorId),
    /// Signed as another writer: no root of the owner family is offered —
    /// it is not theirs to open. A failed open keeps its ordinary class.
    Other,
}

impl RecordSigner {
    /// Whether the holder's **current** owner root is offered to this record.
    pub fn offers_current(self) -> bool {
        self == Self::Current
    }

    /// Of two signers whose rows named one record, the one with the wider
    /// offer — the current identity, else the predecessor nearer in `chain`,
    /// else another writer. A reader that remembers who signed a manifest
    /// keeps this, so no later row narrows what an earlier signature vouched
    /// for, and none widens it past the widest signature seen.
    pub fn wider(self, other: Self, chain: &[PredecessorSealKey]) -> Self {
        let rank = |s: Self| match s {
            Self::Current => 0,
            Self::Predecessor(id) => chain
                .iter()
                .position(|k| k.actor_id == Some(id))
                .map_or(usize::MAX - 1, |at| at + 1),
            Self::Other => usize::MAX,
        };
        if rank(other) < rank(self) {
            other
        } else {
            self
        }
    }

    /// The retired keys of `chain` (nearest hop first) this record is
    /// offered — the one decision of ruling (8)(c) every owner-family open
    /// shares, the bare-key Library blob open included: every one for the
    /// current identity; for a predecessor, its own and every one after it in
    /// the chain (its predecessors), never an unnamed one and never one nearer
    /// than the signer — none when the signer is not in the chain; none for
    /// another writer.
    pub fn retired_keys(self, chain: &[PredecessorSealKey]) -> Vec<&OwnerSealKey> {
        match self {
            Self::Current => chain.iter().map(|k| &k.key).collect(),
            Self::Predecessor(signer) => chain
                .iter()
                .position(|k| k.actor_id == Some(signer))
                .map(|at| {
                    chain[at..]
                        .iter()
                        .filter(|k| k.actor_id.is_some())
                        .map(|k| &k.key)
                        .collect()
                })
                .unwrap_or_default(),
            Self::Other => Vec::new(),
        }
    }
}

/// The key material a walk opens under — the reader's half of the folder key
/// hierarchy (`docs/goal/architecture/nest/mls-group-key-material.md` § M2).
///
/// Deliberately not `Debug`: [`OwnerSealKey`] is secret and does not implement it.
#[derive(Clone, Default)]
pub struct FileDownloadKeys {
    /// The owner's backup key. Present for an owner-only (unbound) set; it is the
    /// convergent chunk root for every chunk that set ever sealed.
    ///
    /// Either owner-audience variant reads here: a client opening its own
    /// `BackupKey`-sealed chunks, or — once the client audit loop lands (slice 4
    /// of the nest-side segment-backup track) — a client opening the segments its
    /// source nest sealed under the granted `NestBackupKey`, to verify them
    /// byte-for-byte against data it already holds.
    pub backup_key: Option<OwnerSealKey>,
    /// Owner keys of **retired identities this account succeeded from**, most
    /// recent first — read-only candidates, never a seal root.
    ///
    /// The identity-succession aftermath (`docs/goal/behavior/succession-aftermath.md`
    /// § Re-key scope, the `BackupKey` corpus row). A succession re-points corpus
    /// *ownership* in one nest-side transaction but moves no *seal*: the successor
    /// derives [`backup_key`](Self::backup_key) from its own new seed while every
    /// chunk already at rest is sealed under the predecessor's. So the fetch
    /// succeeds and only the AEAD tag fails — the successor's entire file-sync and
    /// media corpus reads as dark, indistinguishable from corruption. Offering the
    /// retired roots here is that doc's own answer ("the successor holds the old
    /// seed to unseal what it now owns"), and the prerequisite for the re-seal pass
    /// that eventually retires them: a path this device does not hold locally has
    /// no plaintext source *but* this read.
    ///
    /// Populated from the account registry's predecessor walk
    /// (`fauna_client_accounts::AccountIndex::predecessors_of`), which reaches
    /// **every** ancestor rather than the immediate one — a corpus can still be
    /// sealed under a grandpredecessor when an intermediate re-seal never finished.
    ///
    /// Three properties, each load-bearing:
    ///
    /// 1. **Read-only by construction.** The two seal sites
    ///    ([`Self::label_seal_root`] and the engine's `content_seal_root`) read
    ///    `backup_key` alone, so no write can reach a retired root — a new seal
    ///    under a key the aftermath exists to retire would be silent (a wrong label
    ///    root degrades to `Omit`, never an error). This is a *separate field*
    ///    rather than a keyring on `backup_key` precisely to make that
    ///    unrepresentable instead of merely asserted.
    /// 2. **Travels with `backup_key` on each path, arm for arm.** On the *chunk*
    ///    path both are suppressed for a bound set (FS-5DC): those chunks are
    ///    content-keyed and never owner-keyed, and a fall-through here would
    ///    re-open the shadowing hole that gating on `mls_group_id` closed. On the
    ///    *label* `gen: None` arm both are offered unsuppressed, for the reason
    ///    [`Self::label_open_roots`] already records — an owner who later bound a
    ///    set still holds the names it sealed before binding, and here a
    ///    predecessor's names too.
    /// 3. **Cost is a failed AEAD tag.** Extra candidates are the same mechanism
    ///    [`Self::content_open_roots`] already uses for a same-version key pair;
    ///    the current key is always tried first, so the steady state pays nothing.
    /// 4. **Paired with its identity, in chain order** ([`PredecessorSealKey`]):
    ///    a row signed as one of these identities is offered only that
    ///    identity's root and the ones after it ([`Self::record_signer`]).
    pub predecessor_backup_keys: Vec<PredecessorSealKey>,
    /// Set when this reader is bound to a shared folder. Its presence alone
    /// suppresses the owner-key path — a bound set's chunks are never owner-keyed.
    pub mls_group_id: Option<Vec<u8>>,
    /// Set when the set is **WebDAV-served and group-less** — content-keyed at
    /// the serve pseudo-channel (`webdav-server.md` § Key model, the custody
    /// note) with no MLS group to carry the bound-marker. Its presence alone
    /// suppresses the owner-key chunk path exactly as `mls_group_id` does, and
    /// with `content_keys: None` it **fails closed** the same way: the
    /// read-side twin of the engine's `EngineKeyBinding::ServedKeysMissing`,
    /// which refuses to construct an engine at all. Without this marker a
    /// served-but-keyless reader would read as owner-only and mint an
    /// owner-root seal the MDA could never open. Never faked as a group id.
    pub served: bool,
    /// The M2 per-generation content keys for a bound shared set — or for a
    /// WebDAV-served group-less one (custody at the serve pseudo-channel). Their
    /// presence suppresses the owner-key path exactly as `mls_group_id` does: a
    /// content-keyed set's chunks are never owner-keyed
    /// ([`crate::crypto::effective_owner_key`]).
    pub content_keys: Option<FolderContentKeys>,
    /// M2 generations of a set this reader's **current** binding no longer
    /// claims — the WebDAV serve-toggle twin of
    /// [`predecessor_backup_keys`](Self::predecessor_backup_keys): read-only
    /// candidates, never a seal root, offered to a record stamped with their
    /// generation INSTEAD of the owner-key roots, which a stamped record is
    /// never offered (ruling (10)(c), [`Self::stamp_bound_roots`]).
    ///
    /// `webdav-server.md` § Key model's Revocation bullet: unflagging a set's
    /// serve toggle **rotates** its content key rather than forgetting it, so
    /// the owner's own custody still holds every generation a served window
    /// ever sealed under, even once the set's engine binding degrades back to
    /// owner-only (`content_keys` above is `None` for exactly that set, which
    /// is what makes it eligible for the owner-key path at all). This field is
    /// what still lets that owner path open — and re-seal — the files a
    /// since-disabled serve window sealed under the M2 content key.
    ///
    /// **A separate field, not folded into `content_keys`, on purpose:**
    /// [`Self::effective_backup_key`] gates the owner-key path on
    /// `content_keys.is_some()` alone (`crate::crypto::effective_owner_key`'s
    /// `content_keyed` triple) — a set genuinely bound or still served must
    /// never seal under the owner root. Retired candidates must never trip
    /// that gate, so they live here instead, read by
    /// [`Self::retired_content_open_roots`] only.
    pub retired_content_keys: Option<FolderContentKeys>,
    /// Whether the **record being opened** verified as this holder's own
    /// signed change record of this set, on a set it owns — consulted for
    /// exactly one question: may an **unstamped** record of a content-keyed set
    /// open under the owner root ([`Self::content_open_roots`]'s `version =
    /// None` arm)? Per record, never per holder: the caller sets it for a read
    /// that names one manifest.
    ///
    /// `mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (5) (the (D) enabler — it replaced the nest-projected "is this
    /// holder the owner" flag, which a nest could lie about and which could not
    /// tell a record copied from another of the owner's sets from this set's
    /// own): a bound reader selects the root **by the record's stamp**. A
    /// stamped record opens under that generation and never under the owner
    /// root — the content-keyed precedence of
    /// [`crate::crypto::effective_owner_key`] is untouched, because this field
    /// is never read on a stamped record. An unstamped record of a
    /// content-keyed set is pre-bind content by construction, and the owner's
    /// signature under this set's nonce proves it is this set's, so the owner
    /// root is the only root that can have sealed it.
    ///
    /// **Read-only.** No seal site reads it — [`Self::label_seal_root`] and the
    /// engine's `content_seal_root` still pick the current generation for a
    /// content-keyed set whatever this says, so a bound set never seals under
    /// the owner root. `false` (the default) is the fail-safe direction: a read
    /// whose record was not verified keeps the refusal.
    pub owner_signed_record: bool,
    /// The identity the **record being opened** was signed as — per record,
    /// like [`owner_signed_record`](Self::owner_signed_record): the caller sets
    /// it for a read that names one row.
    ///
    /// `mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(c): of the owner-root family, a row signed as predecessor
    /// **A** is offered only A's own root and the roots of A's predecessors —
    /// never the current [`backup_key`](Self::backup_key) nor a root later in
    /// the chain than A — on every arm that *opens*: the owner-only chunk and
    /// manifest read ([`Self::owner_open_roots`]), part (D)'s unstamped open
    /// ([`Self::prebind_owner_roots`]) and the unstamped label arm
    /// ([`Self::label_open_roots`]). Without it the retired seed and a lying
    /// nest together could name, in one of the account's sets, the manifest or
    /// the sealed path of a file the successor created after the ceremony in
    /// another, and this holder would open it there. The chain is
    /// [`predecessor_backup_keys`](Self::predecessor_backup_keys), nearest hop
    /// first; an unnamed key in it is never offered to a predecessor-signed
    /// row ([`PredecessorSealKey`]).
    ///
    /// **Read-only.** No seal site reads it: a re-seal after such an open
    /// still commits to the current root. [`RecordSigner::Current`] (the
    /// default) is today's behaviour, so the callers that hold a verdict are
    /// the ones that must carry it — a reader of judged change rows sets it
    /// from the verdict's *signed as*, never from the served author.
    pub record_signer: RecordSigner,
    /// Test/legacy configured root for an owner-only set (`None` in production).
    pub epoch_secret: Option<[u8; 32]>,
}

impl FileDownloadKeys {
    /// An owner-only reader holding just its owner key — the snapshot per-file
    /// download and full-restore case on every app. Accepts either
    /// owner-audience key (`BackupKey` or `NestBackupKey`) via `Into`.
    pub fn owner(backup_key: impl Into<OwnerSealKey>) -> Self {
        Self {
            backup_key: Some(backup_key.into()),
            ..Default::default()
        }
    }

    /// The owner key, or `None` when this reader is bound to a shared file
    /// set or holds a set's content keys (whose chunks are content-keyed, never
    /// owner-keyed).
    ///
    /// Delegates rather than restates: FS-5DC is decided once, in
    /// [`crate::crypto::effective_owner_key`], which carries the rule's full
    /// rationale and the failures that bought it. The sync engine's seal/open
    /// side reaches the same function with the same triple.
    fn effective_backup_key(&self) -> Option<&OwnerSealKey> {
        crate::crypto::effective_owner_key(
            self.mls_group_id.as_deref(),
            // A served set is content-keyed whether or not this holder has
            // its keys in hand: keyless must fail closed, never owner-key.
            self.content_keys.is_some() || self.served,
            self.backup_key.as_ref(),
        )
    }

    /// Whether this reader's set rests under M2 content keys at all — bound to
    /// a group ([`mls_group_id`](Self::mls_group_id)) or served group-less
    /// ([`served`](Self::served)). The one predicate every fail-closed bail
    /// below asks, so a served set cannot be missed by a site that only knew
    /// about the group marker.
    pub fn is_content_keyed(&self) -> bool {
        self.mls_group_id.is_some() || self.served
    }

    /// Every owner-audience `chunk_crypto` root this reader may **open** a chunk
    /// or manifest under — the current [`backup_key`](Self::backup_key) first,
    /// then each retired [`predecessor_backup_keys`](Self::predecessor_backup_keys)
    /// root in registry order. Empty for a bound or content-keyed set (FS-5DC, via
    /// [`Self::effective_backup_key`]) and for a reader holding no owner material,
    /// which is what makes the callers' fall-through to `content_open_roots`
    /// unchanged from the single-key shape.
    ///
    /// The AEAD tag disambiguates, exactly as it does for
    /// [`Self::content_open_roots`]'s same-version candidate pair — the current
    /// key is tried first, so a corpus already re-sealed pays nothing.
    fn owner_open_roots(&self) -> Vec<[u8; 32]> {
        // Gated on the *current* key, so FS-5DC is decided in exactly one place
        // ([`Self::effective_backup_key`]) and the predecessors follow it rather
        // than re-deciding it. A reader holding retired keys but no current one
        // is not a shape the aftermath produces — a successor always derives its
        // own — so it stays as fail-closed as it is today rather than being
        // widened speculatively.
        let Some(current) = self.effective_backup_key() else {
            return Vec::new();
        };
        self.owner_family_roots(Some(current))
    }

    /// The owner-root family this **record** may open under, bounded by
    /// [`Self::record_signer`] — the one decision of ruling (8)(c) every
    /// opening arm shares (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*).
    ///
    /// - [`RecordSigner::Current`]: `current` first, then every retired root
    ///   in chain order (an unnamed one included) — today's offer.
    /// - [`RecordSigner::Predecessor`]`(A)`: A's own root and every root after
    ///   it in the chain — A's predecessors — and nothing else: never
    ///   `current`, never a root nearer than A, never an unnamed one. Empty
    ///   when A is not in this holder's paired chain (a host that proved the
    ///   link by statements and holds no predecessor root): the caller's
    ///   "nothing opens" is then a noted skip, never a transient hold.
    /// - [`RecordSigner::Other`] — nothing.
    fn owner_family_roots(&self, current: Option<&OwnerSealKey>) -> Vec<[u8; 32]> {
        current
            .filter(|_| self.record_signer.offers_current())
            .into_iter()
            .chain(
                self.record_signer
                    .retired_keys(&self.predecessor_backup_keys),
            )
            .map(OwnerSealKey::convergent_chunk_root)
            .collect()
    }

    /// Mark a failed **open** of this record final when the per-signer bound
    /// governs it — an unstamped record signed as a predecessor
    /// ([`Self::record_signer`]): nothing a later pull delivers widens the
    /// roots that identity may open, so the failure is
    /// [`PermanentApplyFailure::SIGNER_BOUND`](crate::apply_failure::PermanentApplyFailure::SIGNER_BOUND),
    /// a noted skip, never a transient hold (ruling (8)(c)).
    ///
    /// Wrap only the crypto step, never a fetch: a socket failure stays
    /// transient whoever signed. A stamped record is left as it is: no owner
    /// root is offered to it at all (ruling (10)(c), [`Self::stamp_bound_roots`],
    /// which classes its own failure).
    fn finalize_open<T>(&self, content_key_version: Option<u64>, opened: Result<T>) -> Result<T> {
        finalize_signer_bound(self.signer_bound_is_final(content_key_version), opened)
    }

    /// Whether [`Self::finalize_open`] marks a failed open of this record final.
    fn signer_bound_is_final(&self, content_key_version: Option<u64>) -> bool {
        content_key_version.is_none() && matches!(self.record_signer, RecordSigner::Predecessor(_))
    }

    /// Candidate `chunk_crypto` roots for a bound set's stamped generation.
    ///
    /// - **Bound + content keys** — the keys for `version`; `Err` if this holder
    ///   lacks that generation (a removed member reading post-removal content, or
    ///   a generation not yet synced): never a plaintext/wrong-key fall-through
    ///   (the FS-BIND-5 posture, per generation). Normally one candidate; after a
    ///   concurrent-rotation custody merge two distinct keys can share a
    ///   version, so the caller tries each — the AEAD
    ///   tag disambiguates.
    /// - **Content-keyed, UNSTAMPED record** (`version = None`) — the root is
    ///   selected by the stamp's absence, never guessed from the generations:
    ///   the owner root (and its retired predecessors) for a record that
    ///   [verified as the holder's own](Self::owner_signed_record), `Err` for
    ///   any other. Decided before
    ///   the content keys are consulted, so it holds for a bound-keyless owner
    ///   too — pre-bind content was never content-keyed.
    /// - **Bound but no content keys** — `Err` (fail closed).
    /// - **Owner-only set** — `version` is irrelevant: the configured
    ///   `epoch_secret` (`None` in production; the `backup_key` branch applies at
    ///   the call sites).
    fn content_open_roots(&self, version: Option<u64>) -> Result<Option<Vec<[u8; 32]>>> {
        if version.is_none() && (self.content_keys.is_some() || self.is_content_keyed()) {
            return self.prebind_owner_roots().map(Some);
        }
        if let Some(content_keys) = self.content_keys.as_ref() {
            let version = version.context(
                "content_open_roots: a chunk of a bound shared folder carries no content-key \
                 version stamp — refusing to guess the generation",
            )?;
            let candidates: Vec<[u8; 32]> = content_keys.keys_for(version).copied().collect();
            if candidates.is_empty() {
                anyhow::bail!(
                    "content_open_roots: this holder lacks content-key generation {version} \
                     (removed member, or generation not yet synced) — failing closed rather than \
                     fall through to a plaintext/owner-key read"
                );
            }
            return Ok(Some(candidates));
        }
        if self.is_content_keyed() {
            anyhow::bail!(
                "content_open_roots: reader's set is content-keyed (bound to a shared folder, or \
                 WebDAV-served) but has no M2 content keys loaded (removed member / custody not \
                 yet synced / startup race?) — refusing to read"
            );
        }
        Ok(self.epoch_secret.map(|secret| vec![secret]))
    }

    /// The roots an **unstamped** record of a content-keyed set may open under
    /// — part (D)'s read (`mls-group-key-material.md` § M2 → *Pre-bind re-seal
    /// migration*): the owner's current root, then each retired predecessor
    /// root (a successor's pre-bind corpus may still rest under one), for a
    /// record that verified as the holder's own only — bounded by who signed
    /// it ([`Self::owner_family_roots`]).
    ///
    /// ⚠ **Reads the raw `backup_key`, deliberately bypassing
    /// [`Self::effective_backup_key`].** That gate answers "may the owner key
    /// seal or open this set's *content-keyed* chunks" and must keep answering
    /// no. This is a different question, reached only once the record's own
    /// stamp has said it is not content-keyed — the label `gen: None` arm's
    /// reasoning ([`Self::label_open_roots`]), narrowed to the owner.
    fn prebind_owner_roots(&self) -> Result<Vec<[u8; 32]>> {
        if !self.owner_signed_record {
            anyhow::bail!(
                "content_open_roots: an unstamped record of a content-keyed set is pre-bind \
                 content under its owner's root, and this record did not verify as this \
                 holder's own — refusing to guess the generation or offer its own key"
            );
        }
        let Some(current) = self.backup_key.as_ref() else {
            anyhow::bail!(
                "content_open_roots: an unstamped record of a content-keyed set rests under the \
                 owner root, and this owner holder carries no owner key — failing closed"
            );
        };
        Ok(self.owner_family_roots(Some(current)))
    }

    /// The roots an **owner-only** reader may open a **stamped** record under
    /// — `writer-signed-change-records.md` ruling (10)(c), *the stamp binds
    /// the root*: that generation's retired content key(s)
    /// ([`Self::retired_content_open_roots`]) and **nothing of the owner
    /// family**, whoever signed the record. `Ok(None)` for an unstamped record
    /// or a reader that is not owner-only (a content-keyed set selects by the
    /// stamp in [`Self::content_open_roots`] already), so the caller's
    /// owner-root arm is reached by unstamped records alone.
    ///
    /// The stamp is the writer's signed statement of the generation its bytes
    /// were sealed under, so an honest stamped record never rests under an
    /// owner root and loses nothing here. Without this the retired seed and a
    /// lying nest could stamp a row naming bytes the successor sealed, a
    /// restore would re-point it verbatim *because* it is stamped, and the
    /// head — then signed as the current identity — would open under the
    /// current root. Bound to the record rather than the set's mode, so a
    /// serve-off's still-stamped heads stay bound too.
    ///
    /// No generation held → [`PermanentApplyFailure::STAMP_BOUND`](crate::apply_failure::PermanentApplyFailure::STAMP_BOUND):
    /// an owner-only reader's generations come with the custody that made the
    /// set owner-only, so a pull delivers none — a noted skip, never a hold
    /// that would cap every later row behind one stamp.
    fn stamp_bound_roots(&self, version: Option<u64>) -> Result<Option<Vec<[u8; 32]>>> {
        if version.is_none() || self.effective_backup_key().is_none() {
            return Ok(None);
        }
        let roots = self.retired_content_open_roots(version);
        if roots.is_empty() {
            return Err(crate::apply_failure::permanent(
                crate::apply_failure::PermanentApplyFailure::STAMP_BOUND,
                format!(
                    "a record stamped with content-key generation {version:?} on an owner-only \
                     set opens under that generation or not at all, and this reader holds none \
                     of it — no owner root is offered to a stamped record"
                ),
            ));
        }
        Ok(Some(roots))
    }

    /// Retired M2 generation candidates for a stamped `version`, from
    /// [`retired_content_keys`](Self::retired_content_keys) — the served-set
    /// twin of [`owner_open_roots`](Self::owner_open_roots)'s predecessor
    /// widening, but offered **instead of** the owner roots, never beside them
    /// ([`Self::stamp_bound_roots`], ruling (10)(c)).
    ///
    /// Unlike [`Self::content_open_roots`] this never errors: an absent
    /// version or absent `retired_content_keys` simply answers empty; whether
    /// an empty answer is a failure is [`Self::stamp_bound_roots`]' question.
    fn retired_content_open_roots(&self, version: Option<u64>) -> Vec<[u8; 32]> {
        let (Some(content_keys), Some(version)) = (self.retired_content_keys.as_ref(), version)
        else {
            return Vec::new();
        };
        content_keys.keys_for(version).copied().collect()
    }

    /// Candidate roots for opening a **sealed label** — the read-side mirror of
    /// [`crate::path_crypto`]'s seal (`docs/goal/behavior/file-sync.md`
    /// § Sealed names & paths). Same custody object as the byte download by the
    /// ruling's own logic: whoever can open the set's bytes renders its names.
    ///
    /// **Deliberately not [`Self::content_open_roots`]**, for two reasons a
    /// future reader will otherwise try to "simplify" away:
    ///
    /// 1. **The discriminator is the envelope's own `gen`**, not the chunk's
    ///    `content_key_version`. A label carries the generation it sealed under
    ///    (that is what makes a keyless server-side row copy stay openable), so
    ///    the caller passes [`crate::path_crypto::SealedLabel::generation`] here
    ///    — never the manifest's stamp.
    /// 2. **The `gen: None` arm consults `backup_key` UNSUPPRESSED by
    ///    `mls_group_id`.** A set's owner who later *bound* the set still holds
    ///    the labels it sealed under the owner root before binding; suppressing
    ///    the owner key (as the chunk path must, FS-5DC) would make the owner
    ///    unable to render their own pre-binding names. This is safe where the
    ///    chunk suppression is not: trying an extra root of the reader's *own*
    ///    key material only ever fails an AEAD tag — it cannot downgrade a
    ///    bound set's seal, because a bound-set label is stamped `gen: Some(v)`
    ///    and never reaches this arm.
    ///
    /// Arm-for-arm the mirror of `SyncEngine::seal_recorded_path`'s root
    /// selection: `Some(v)` fails closed (FS-BIND-5) when this holder lacks that
    /// generation; `gen: None` offers the configured `epoch_secret` (test/legacy)
    /// and the owner's `convergent_chunk_root()`, in that order — the AEAD tag
    /// disambiguates, exactly as it does for a same-version candidate pair.
    ///
    /// An empty `Ok(vec![])` means "this reader holds nothing that could open
    /// it" — the caller degrades per the ratified contract (omit from the
    /// listing, re-enter on re-record), it is not an error.
    pub fn label_open_roots(&self, generation: Option<u64>) -> Result<Vec<[u8; 32]>> {
        if let Some(version) = generation {
            // The live generations, then the RETIRED ones a since-unserved set's
            // served-era names still rest under — the label twin of
            // `retired_content_open_roots` on the byte path. Without them an
            // owner reader opened a served-era file's bytes and never its name,
            // so the file silently never landed (`webdav-server.md` § Key model,
            // Revocation). An extra root of the reader's own material only ever
            // fails an AEAD tag.
            let retired = self.retired_content_open_roots(Some(version));
            if self.content_keys.is_none() && retired.is_empty() {
                anyhow::bail!(
                    "label_open_roots: label is stamped content-key generation {version} but this \
                     reader holds no M2 content keys (removed member / generation not yet \
                     synced?) — failing closed rather than guessing a root"
                );
            }
            let mut candidates: Vec<[u8; 32]> = self
                .content_keys
                .as_ref()
                .map(|keys| keys.keys_for(version).copied().collect())
                .unwrap_or_default();
            candidates.extend(retired);
            if candidates.is_empty() {
                anyhow::bail!(
                    "label_open_roots: this holder lacks content-key generation {version} \
                     (removed member, or generation not yet synced)"
                );
            }
            return Ok(candidates);
        }
        let mut roots = Vec::new();
        if let Some(secret) = self.epoch_secret {
            roots.push(secret);
        }
        // The current owner root, then the retired roots a not-yet-re-sealed
        // corpus's *names* still rest under — bounded by who signed the row
        // (ruling (8)(c), [`Self::owner_family_roots`]): the sealed path of a
        // file the successor created elsewhere, or a later predecessor sealed,
        // must not render under an earlier identity's signature. Unsuppressed
        // by `mls_group_id` for the reason in this method's point 2: a
        // `gen: Some(v)` label never reaches this arm, so an extra root of the
        // reader's own material can only ever fail an AEAD tag. Without the
        // retired roots a successor opens its bytes and renders an empty file
        // list over them — silently, since a wrong root degrades to `Omit`.
        roots.extend(self.owner_family_roots(self.backup_key.as_ref()));
        Ok(roots)
    }

    /// The **one** root a client seals a new label under — the write-side mirror
    /// of [`Self::label_open_roots`], and arm-for-arm the mirror of
    /// `SyncEngine::label_seal_root` (`libs/fauna-sync-engine/src/engine.rs`).
    ///
    /// Sealing differs from opening in kind, not degree: opening offers
    /// *candidates* and lets the AEAD tag disambiguate, while sealing must
    /// commit to exactly one root **and** stamp the generation that names it —
    /// which is why [`crate::path_crypto::LabelRoot`] pairs the two rather than
    /// letting a caller seal under generation 3's key while stamping `gen:
    /// None`.
    ///
    /// This exists because the sync engine is not the only writer. The
    /// engine's selection serves the paths the *engine* records; a label
    /// minted by an app gesture — a snapshot's tags (path-sealing S6-d) — is
    /// authored in `fauna-client-*`, which cannot reach a `SyncEngine`. Both
    /// selections are pinned equal by test: a set whose tags sealed under a
    /// different root than its paths renders for a different audience than its
    /// own file list, and that failure is **silent** (a wrong root degrades to
    /// [`crate::path_crypto::SealedLabelRender::Omit`], never to an error).
    ///
    /// - **Bound + content keys** → the *current* generation, stamped. New
    ///   labels seal under what new uploads seal under.
    /// - **Bound, no content keys** → `Err`. Fail closed exactly as
    ///   `content_seal_root` does: a removed member or a startup race must never
    ///   degrade a shared set's label to the owner root (which no other roster
    ///   member could open) or to plaintext.
    /// - **Unbound** → the configured `epoch_secret` (test/legacy) else the
    ///   owner's `convergent_chunk_root()`, with no generation to stamp.
    /// - **Neither** → `Ok(None)`: this holder cannot seal at all. The caller
    ///   records plaintext-only and leaves an S8 backfill row — the ratified
    ///   degrade, not an error (`docs/goal/behavior/file-sync.md` § Sealed names
    ///   & paths).
    pub fn label_seal_root(&self) -> Result<Option<crate::path_crypto::LabelRoot>> {
        use crate::path_crypto::LabelRoot;

        if let Some(content_keys) = self.content_keys.as_ref() {
            return Ok(Some(LabelRoot::content_key(
                *content_keys.current_key(),
                content_keys.current_version(),
            )));
        }
        if self.is_content_keyed() {
            anyhow::bail!(
                "label_seal_root: reader's set is content-keyed (bound to a shared folder, or \
                 WebDAV-served) but has no M2 content keys loaded (removed member / custody not \
                 yet synced / startup race?) — refusing to seal its label under a root no other \
                 holder of the set could open"
            );
        }
        if let Some(secret) = self.epoch_secret {
            return Ok(Some(LabelRoot::owner(secret)));
        }
        Ok(self
            .backup_key
            .as_ref()
            .map(|key| LabelRoot::owner(key.convergent_chunk_root())))
    }
}

/// Fetch a manifest by content address, **verify it against that address**, and
/// open its hash list if sealed.
///
/// The address check is this function's load-bearing job, not a formality: it is
/// the one place the walk's root document is authenticated, and every other
/// integrity check in the module reads its trusted values out of what this
/// returns (see the module docs' `# Integrity` section).
///
/// A sealed manifest (`sealed_hashes` present) hides `file_hash`/`chunk_hashes`
/// until opened under a chunk root — distinct from `stored_hashes`, which seals
/// the chunk *bodies* (see [`fetch_decoded_chunks`]). Both axes are independent.
///
/// Public because the sync engine's *other* download paths (the streaming
/// disk-write and the self-echo apply) need the opened manifest in hand — for a
/// progress event's size, or to decide whether to rewrite a merge base — before
/// they fetch chunks. They share this opening policy rather than restate it.
pub async fn fetch_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: &ContentHash,
    content_key_version: Option<u64>,
) -> Result<ChunkManifest> {
    let manifest_bytes = fetcher
        .fetch_manifest(manifest_hash)
        .await
        .context("downloading manifest")?;
    // Address-vs-bytes at the transfer boundary — the root of the chain, so it
    // comes before the decode and before the `is_sealed()` branch. Everything
    // downstream takes its trusted values *from this document*, so a served
    // manifest that is not the one asked for is internally consistent with
    // itself and nothing further down can catch it. The writer addresses a
    // manifest as `of_raw(canonical_encode(&manifest))` over the exact bytes it
    // uploads (`fauna-sync-engine::engine`), so this compare is exact, not
    // approximate.
    let actual = ContentHash::of_raw(&manifest_bytes);
    if actual != *manifest_hash {
        anyhow::bail!(
            "manifest hash mismatch: asked for {}, got bytes hashing to {}",
            hex::encode(manifest_hash.digest()),
            hex::encode(actual.digest())
        );
    }
    let manifest: ChunkManifest =
        crate::encoding::canonical_decode(&manifest_bytes).context("deserializing manifest")?;
    // PERMANENT for this change (`apply_failure`): a format bump this binary
    // does not implement recurs identically on every later pull, so the
    // catch-up anchor must skip the change rather than freeze below it —
    // exactly the freeze measured live 2026-07-31.
    manifest.check_min_reader().map_err(|e| {
        crate::apply_failure::permanent(
            crate::apply_failure::PermanentApplyFailure::MANIFEST_TOO_NEW,
            format!("{e:#}"),
        )
    })?;
    // A sealed-chunk manifest carrying its plaintext hashes (beside or instead
    // of `sealed_hashes`) is refused, never tolerated: it is the destination's
    // confirmation oracle (`mls-group-key-material.md` § M2 *Sealed manifest
    // hashes*). PERMANENT — the same bytes refetch the same shape.
    manifest.check_hash_shape().map_err(|e| {
        crate::apply_failure::permanent(
            crate::apply_failure::PermanentApplyFailure::MANIFEST_SHAPE_REFUSED,
            format!("{e:#}"),
        )
    })?;
    if !manifest.is_sealed() {
        return Ok(manifest);
    }
    keys.finalize_open(
        content_key_version,
        open_sealed_manifest(keys, manifest, content_key_version),
    )
}

/// [`FileDownloadKeys::finalize_open`]'s classification, for a walk that
/// resolved the bound once and opens window by window ([`ManifestWalk`]).
fn finalize_signer_bound<T>(is_final: bool, opened: Result<T>) -> Result<T> {
    match opened {
        Err(e) if is_final && crate::apply_failure::permanent_reason(&e).is_none() => {
            Err(crate::apply_failure::permanent(
                crate::apply_failure::PermanentApplyFailure::SIGNER_BOUND,
                format!("{e:#}"),
            ))
        }
        other => other,
    }
}

/// [`fetch_manifest`]'s crypto step: select the roots and unseal.
fn open_sealed_manifest(
    keys: &FileDownloadKeys,
    manifest: ChunkManifest,
    content_key_version: Option<u64>,
) -> Result<ChunkManifest> {
    let owner_roots = keys.owner_open_roots();
    let roots = if let Some(roots) = keys.stamp_bound_roots(content_key_version)? {
        roots
    } else if !owner_roots.is_empty() {
        owner_roots
    } else {
        keys.content_open_roots(content_key_version)?
            .filter(|roots| !roots.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "sealed manifest but no open root for generation \
                     {content_key_version:?} (fail closed)"
                )
            })?
    };
    // Try every same-version candidate root (normally one; two after a
    // concurrent-rotation custody merge — the AEAD tag disambiguates).
    let mut last_err = None;
    for root in &roots {
        match manifest.clone().unseal_hashes(root) {
            Ok(m) => return Ok(m),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("roots is non-empty"))
}

/// Download a manifest's chunks, then decrypt and decompress them, yielding the
/// plaintext chunk payloads in manifest order (ready for reassembly).
///
/// **The manifest is self-describing: `stored_hashes` presence is the seal
/// discriminator** (`file-sync.md` § Apple apps — *"the manifest's
/// `stored_hashes` presence selects the open path"*; `key-material-hierarchy.md`
/// § M2 At-rest blob keying):
///
/// - `stored_hashes = None` ⇒ the stored bytes ARE the (possibly compressed)
///   plaintext — **always, regardless of this reader's key state**
///   (`ChunkManifest.stored_hashes`'s own contract). A keyed reader reading a
///   plaintext manifest passes it through: that is what keeps plaintext
///   manifests (public-audience folders, a public-to-private flip, raw nest
///   writes) readable to a keyed reader, and what fixes the windows on-demand hydration host (keyed,
///   download-only) hard-failing every owner-only file. The branch this replaced — "no
///   `stored_hashes` + a key ⇒ legacy random-nonce framed seal" — guarded a
///   corpus that provably does not exist: every pre-convergence framed chunk
///   upload was rejected 400 by the F9 chunk route and never stored (FS-BIND
///   FOLLOW-ON A), and no other production writer ever held a chunk
///   key before the convergent reconcile.
/// - `stored_hashes = Some` ⇒ convergent `chunk_crypto`, root selected by key
///   precedence: the owner `BackupKey` root for an unbound set, else the M2
///   content keys for the stamped `content_key_version` (fails closed if this
///   holder lacks that generation; every same-version candidate is tried). **A sealed manifest with no key material
///   at all fails closed, loudly** — never a ciphertext passthrough (which would
///   land raw ciphertext in the reassembler and surface as a baffling whole-file
///   hash mismatch, or worse).
///
/// Public for the same reason as [`fetch_manifest`]: the sync engine's streaming
/// disk-write and self-echo paths reassemble the plaintext chunks themselves
/// (to a file, or to compare against a merge base) instead of taking the single
/// buffer [`download_file_bytes_by_manifest`] returns.
pub async fn fetch_decoded_chunks(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest: &ChunkManifest,
    relative_path: &str,
    content_key_version: Option<u64>,
) -> Result<Vec<Vec<u8>>> {
    // Address the blob store by the content-addressed STORE key (the ciphertext
    // hash for a sealed set, else the plaintext hash), then decrypt under the
    // PLAINTEXT hash — `chunk_crypto` derives its key+nonce from the plaintext
    // content hash (FS-BIND, PIECE 6).
    let chunk_data = fetcher
        .fetch_chunks(&manifest.store_keys(), relative_path)
        .await?;

    keys.finalize_open(
        content_key_version,
        resolve_chunk_open_policy(keys, manifest, relative_path, content_key_version)
            .and_then(|policy| open_chunk_window(&policy, &manifest.chunk_hashes, chunk_data)),
    )
}

/// How each stored chunk body of one manifest becomes plaintext — the seal
/// discriminator + key precedence of [`fetch_decoded_chunks`], resolved **once
/// per manifest** so the whole-buffer and bounded-memory ([`download_file_to_path_by_manifest`])
/// walks share exactly one policy.
enum ChunkOpenPolicy {
    /// Plaintext manifest (`stored_hashes = None`) — pass bodies through; the
    /// caller's whole-file hash verify anchors integrity.
    Plaintext,
    /// Convergent `chunk_crypto` seal; try each candidate root in order (normally
    /// one — the owner `BackupKey` root, or the stamped generation's content key;
    /// two only after a concurrent-rotation custody merge, where the AEAD tag
    /// disambiguates).
    Roots(Vec<[u8; 32]>),
}

/// Resolve the [`ChunkOpenPolicy`] for one manifest (see [`fetch_decoded_chunks`]'s
/// doc for the full discriminator rationale). A sealed manifest with no key
/// material fails closed, loudly — never a ciphertext passthrough.
fn resolve_chunk_open_policy(
    keys: &FileDownloadKeys,
    manifest: &ChunkManifest,
    relative_path: &str,
    content_key_version: Option<u64>,
) -> Result<ChunkOpenPolicy> {
    if manifest.stored_hashes.is_none() {
        return Ok(ChunkOpenPolicy::Plaintext);
    }
    if let Some(roots) = keys.stamp_bound_roots(content_key_version)? {
        return Ok(ChunkOpenPolicy::Roots(roots));
    }
    let owner_roots = keys.owner_open_roots();
    if !owner_roots.is_empty() {
        // Convergent owner-backup seal (FS-BIND FOLLOW-ON A, 2026-07-07+) of
        // an unstamped record — the current key, then any retired predecessor
        // root a not-yet-re-sealed corpus still rests under
        // (`FileDownloadKeys::predecessor_backup_keys`).
        return Ok(ChunkOpenPolicy::Roots(owner_roots));
    }
    if let Some(roots) = keys.content_open_roots(content_key_version)? {
        if roots.is_empty() {
            // Part (D)'s arm under the per-signer bound, for a signer this
            // holder cannot place: `finalize_open` makes it final.
            anyhow::bail!("sealed manifest for {relative_path}: no root its signer may open");
        }
        return Ok(ChunkOpenPolicy::Roots(roots));
    }
    if keys.backup_key.is_some() && keys.record_signer != RecordSigner::Current {
        // An owner reader whose bound left it nothing for this record — not
        // a reader without key material.
        anyhow::bail!("sealed manifest for {relative_path}: no root its signer may open");
    }
    // PERMANENT for this change (`apply_failure`), and deliberately NOT the
    // same class as `content_open_roots`' "this holder lacks generation v"
    // above: a generation arrives over the wire, so that one stays transient
    // and the anchor waits for it (`path-sealing.md` § Apply-path degrade
    // ruling). A reader holding NO key material of any kind never acquires an
    // owner key by syncing, so waiting for it is waiting for nothing —
    // freezing here would strand every later change behind this one.
    Err(crate::apply_failure::permanent(
        crate::apply_failure::PermanentApplyFailure::NO_KEY_MATERIAL,
        format!(
            "sealed manifest for {relative_path} (stored_hashes present) but this reader holds \
             no BackupKey and no content keys — refusing to pass ciphertext through (fail \
             closed; an owner-only reader needs its BackupKey)"
        ),
    ))
}

/// Open one contiguous window of stored chunk bodies under `policy`:
/// decrypt (trying each candidate root — a manifest is sealed by one device
/// under one key, so whichever root opens it opens all of it) and strip the
/// self-describing compression prefix every stored chunk carries
/// (`compress::compress_chunk` frames even an incompressible chunk `0x00`).
/// Bounded: a chunk body is untrusted input until the whole-file address
/// verifies, so a small malicious blob must not expand to exhaust memory.
///
/// The frame is **not decidable by inspection** — an unframed chunk can begin
/// with a frame byte — so the manifest's plaintext hash is the oracle
/// (`compress::unframe_verified_chunk`: framed first, the raw body as the
/// fallback, fail closed when neither addresses the recorded content). This
/// is the same walk the nest web reader takes. The raw arm
/// is live: the nest's first-writer-wins store also takes raw bodies under
/// their plaintext hash, the key a public chunk rests under. Verifying per
/// chunk also names the first bad chunk instead of a whole-file mismatch.
///
/// `chunk_hashes` must be the plaintext hashes **parallel to** `bodies` (the
/// same window of the manifest) — `chunk_crypto` keys+nonces off them.
fn open_chunk_window(
    policy: &ChunkOpenPolicy,
    chunk_hashes: &[ContentHash],
    bodies: Vec<Vec<u8>>,
) -> Result<Vec<Vec<u8>>> {
    if chunk_hashes.len() != bodies.len() {
        anyhow::bail!(
            "open_chunk_window: {} chunk hashes but {} bodies",
            chunk_hashes.len(),
            bodies.len()
        );
    }
    let opened = match policy {
        ChunkOpenPolicy::Plaintext => bodies,
        ChunkOpenPolicy::Roots(roots) => {
            let mut decoded = None;
            let mut last_err = None;
            for root in roots {
                match crate::chunk_crypto::decrypt_chunks(root, chunk_hashes, &bodies) {
                    Ok(d) => {
                        decoded = Some(d);
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            match decoded {
                Some(d) => d,
                None => return Err(last_err.expect("roots is non-empty")),
            }
        }
    };
    opened
        .into_iter()
        .zip(chunk_hashes)
        .enumerate()
        .map(|(i, (body, want))| {
            crate::compress::unframe_verified_chunk(body, want).ok_or_else(|| {
                anyhow::anyhow!(
                    "chunk {i} does not address its recorded content (expected {}) either \
                     framed or raw — the stored bytes are not this chunk",
                    hex::encode(want.digest())
                )
            })
        })
        .collect()
}

/// A manifest opened for a **bounded, windowed walk** — the seal
/// discriminator and key precedence resolved once ([`resolve_chunk_open_policy`]),
/// then the chunks fetched and opened a [`Self::WINDOW`] at a time, so peak
/// memory is O(window × chunk) rather than O(file).
///
/// The one windowed walk every bounded consumer shares: the File Provider's
/// download-to-path ([`download_file_to_path_by_manifest`]), the hash-only
/// verify ([`verify_file_by_manifest`]), and the sync engine's nest-sourced
/// re-seal, which re-seals each window as it opens it and never holds the whole
/// file (`mls-group-key-material.md` § M2 → *Pre-bind re-seal migration*, part
/// (D): an iOS extension's memory cap rules out a whole-file buffer).
///
/// Each window's chunks are verified against the manifest's plaintext hashes as
/// they open ([`open_chunk_window`]); the whole-file content address is the
/// consumer's to check once the last window lands ([`Self::file_hash`] is what
/// it must equal) — a per-chunk check alone does not prove the chunk *list* is
/// the file's, which is why every consumer below closes on it.
pub struct ManifestWalk {
    manifest: ChunkManifest,
    policy: ChunkOpenPolicy,
    store_keys: Vec<ContentHash>,
    /// A failed window open is final under the per-signer bound
    /// ([`FileDownloadKeys::finalize_open`]).
    signer_bound_is_final: bool,
}

impl ManifestWalk {
    /// Chunks fetched + held per window: keeps the batch seam's concurrency
    /// while bounding peak memory to ~window × MAX_CHUNK (≤ 32 MB plaintext).
    pub const WINDOW: usize = 4;

    /// Fetch and address-verify the manifest, open its hashes, and resolve the
    /// open policy — every refusal a whole-buffer walk makes happens here, before
    /// the first chunk is fetched.
    pub async fn open(
        fetcher: &dyn BlobFetcher,
        keys: &FileDownloadKeys,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        relative_path: &str,
    ) -> Result<Self> {
        if !crate::path_guard::is_safe_relative_path(relative_path) {
            anyhow::bail!("unsafe path rejected: {relative_path}");
        }
        let manifest = fetch_manifest(fetcher, keys, &manifest_hash, content_key_version).await?;
        let signer_bound_is_final = keys.signer_bound_is_final(content_key_version);
        let policy = finalize_signer_bound(
            signer_bound_is_final,
            resolve_chunk_open_policy(keys, &manifest, relative_path, content_key_version),
        )?;
        let store_keys = manifest.store_keys();
        if store_keys.len() != manifest.chunk_hashes.len()
            || manifest.chunk_sizes.len() != manifest.chunk_hashes.len()
        {
            anyhow::bail!(
                "manifest for {relative_path} lists {} chunk hashes, {} store keys and {} sizes \
                 — refusing a malformed manifest",
                manifest.chunk_hashes.len(),
                store_keys.len(),
                manifest.chunk_sizes.len()
            );
        }
        Ok(Self {
            manifest,
            policy,
            store_keys,
            signer_bound_is_final,
        })
    }

    /// The opened (plaintext-view) manifest.
    pub fn manifest(&self) -> &ChunkManifest {
        &self.manifest
    }

    /// The whole-file content address the reassembled windows must hash to.
    pub fn file_hash(&self) -> ContentHash {
        self.manifest.file_hash
    }

    /// How many windows [`Self::window`] serves.
    pub fn window_count(&self) -> usize {
        self.store_keys.len().div_ceil(Self::WINDOW)
    }

    /// Window `index`: `(plaintext hash, plaintext)` per chunk, in manifest
    /// order, each verified to address its recorded content.
    pub async fn window(
        &self,
        fetcher: &dyn BlobFetcher,
        index: usize,
        relative_path: &str,
    ) -> Result<Vec<(ContentHash, Vec<u8>)>> {
        let start = index * Self::WINDOW;
        let end = (start + Self::WINDOW).min(self.store_keys.len());
        if start >= end {
            anyhow::bail!("manifest walk: window {index} is past the last chunk");
        }
        let bodies = fetcher
            .fetch_chunks(&self.store_keys[start..end], relative_path)
            .await?;
        let hashes = &self.manifest.chunk_hashes[start..end];
        let opened = finalize_signer_bound(
            self.signer_bound_is_final,
            open_chunk_window(&self.policy, hashes, bodies),
        )?;
        // Each chunk's length against the manifest's size table: the hash check
        // proves the bytes, not the table, and a consumer that carries the table
        // forward (the re-seal) or seeks by it (a range read of the result) must
        // not inherit a permuted one that merely sums to the right total.
        for (i, plain) in opened.iter().enumerate() {
            let want = self.manifest.chunk_sizes[start + i];
            if plain.len() as u64 != want {
                anyhow::bail!(
                    "chunk {} of {relative_path} is {} bytes but the manifest records {want} — \
                     refusing a manifest whose size table does not match its chunks",
                    start + i,
                    plain.len()
                );
            }
        }
        Ok(hashes.iter().copied().zip(opened).collect())
    }
}

/// Walk one file's chunks with bounded memory and verify the whole-file content
/// address **without keeping the bytes** — the proof a re-seal's new copy is
/// retrievable, decryptable and exactly the file, at O(window) memory rather
/// than the O(file) of [`download_file_bytes_by_manifest`]. Returns the
/// verified whole-file hash.
pub async fn verify_file_by_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
    relative_path: &str,
) -> Result<ContentHash> {
    let walk = ManifestWalk::open(
        fetcher,
        keys,
        manifest_hash,
        content_key_version,
        relative_path,
    )
    .await?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    for i in 0..walk.window_count() {
        for (_, plain) in walk.window(fetcher, i, relative_path).await? {
            total += plain.len() as u64;
            hasher.update(&plain);
        }
    }
    let actual = ContentHash::from_digest_raw(*hasher.finalize().as_bytes());
    if actual != walk.file_hash() || total != walk.manifest().total_size {
        anyhow::bail!(
            "file hash mismatch: expected {} ({} bytes), got {} ({total} bytes)",
            hex::encode(walk.file_hash().digest()),
            walk.manifest().total_size,
            hex::encode(actual.digest())
        );
    }
    Ok(actual)
}

/// Download one file's plaintext **to a path** with bounded memory — the walk
/// of [`download_file_bytes_by_manifest`], but windowed ([`ManifestWalk`]):
/// chunks are fetched, opened and appended to `dest` a few at a time, so peak
/// memory is O(window × chunk) instead of O(file). Built for the iOS File
/// Provider extension's `fetchContents` (an appex runs under a hard memory cap a
/// large file's single `Vec<u8>` — crossed over UniFFI, doubled — would blow
/// through); correct for any native caller that wants bytes on disk rather than
/// in hand.
///
/// Integrity is anchored exactly like the in-memory walk: after the last chunk
/// lands, the **written file** is re-hashed (streaming, bounded) and verified
/// against the manifest's whole-file content address — on mismatch (or any
/// error) `dest` is removed and the error returned, so a partial or corrupt
/// file never survives at `dest`. `dest` is treated as a scratch target owned
/// by this call (truncated on entry, deleted on failure): the File Provider
/// caller hands the OS-provided temp URL, which the OS itself promotes — a
/// caller needing atomic placement at a live path does its own temp + rename.
///
/// Returns the verified whole-file content hash (the FP `contentVersion`).
#[cfg(not(target_arch = "wasm32"))]
pub async fn download_file_to_path_by_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
    relative_path: &str,
    dest: &std::path::Path,
) -> Result<ContentHash> {
    let walk = ManifestWalk::open(
        fetcher,
        keys,
        manifest_hash,
        content_key_version,
        relative_path,
    )
    .await?;

    let write_walk = async {
        use std::io::Write;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dirs for {}", dest.display()))?;
        }
        let mut file =
            std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
        for i in 0..walk.window_count() {
            for (_, plain) in walk.window(fetcher, i, relative_path).await? {
                file.write_all(&plain)
                    .with_context(|| format!("writing chunk to {}", dest.display()))?;
            }
        }
        file.flush()
            .with_context(|| format!("flushing {}", dest.display()))?;
        drop(file);

        // The whole-file verify — security, not hygiene, exactly as in
        // `download_file_bytes_by_manifest` (see the module docs); streaming so
        // the verify is as bounded as the download.
        let actual = crate::chunker_stream::content_hash_streaming(dest)
            .with_context(|| format!("hashing downloaded {}", dest.display()))?;
        if actual != walk.file_hash() {
            anyhow::bail!(
                "file hash mismatch: expected {}, got {}",
                hex::encode(walk.file_hash().digest()),
                hex::encode(actual.digest())
            );
        }
        Ok(walk.file_hash())
    };

    match write_walk.await {
        Ok(hash) => Ok(hash),
        Err(e) => {
            // Never leave a partial/corrupt file at dest.
            let _ = std::fs::remove_file(dest);
            Err(e)
        }
    }
}

/// Download one file's plaintext bytes by its manifest hash — **the walk**.
///
/// Fetches the manifest + chunks by content address, opens them under `keys`
/// (fail-closed on a sealed manifest with no key), reassembles, and verifies the
/// result against the manifest's whole-file content address before returning it.
///
/// `content_key_version` is the M2 generation stamp the file entry carries; it is
/// consulted only for a bound shared set (an owner-only read ignores it).
///
/// The whole-file verify is **security, not hygiene** — the chunk AEAD frame
/// carries no AAD, so a different blob sealed under the same key decrypts
/// cleanly; only the content address catches the swap (see the module docs).
pub async fn download_file_bytes_by_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
    relative_path: &str,
) -> Result<Vec<u8>> {
    if !crate::path_guard::is_safe_relative_path(relative_path) {
        anyhow::bail!("unsafe path rejected: {relative_path}");
    }
    let manifest = fetch_manifest(fetcher, keys, &manifest_hash, content_key_version).await?;
    let chunk_data =
        fetch_decoded_chunks(fetcher, keys, &manifest, relative_path, content_key_version).await?;

    // Reassemble in memory and verify the file hash. Callers (the cfapi FETCH_DATA
    // callback, the web blob download) chunk the transfer themselves, so we hand
    // back a single buffer rather than streaming to disk.
    let file_data = crate::chunker::reassemble_chunks(&chunk_data);
    let actual = ContentHash::of_raw(&file_data);
    if actual != manifest.file_hash {
        anyhow::bail!(
            "file hash mismatch: expected {}, got {}",
            hex::encode(manifest.file_hash.digest()),
            hex::encode(actual.digest())
        );
    }
    Ok(file_data)
}

/// The file's total plaintext size by manifest — what a seeking reader
/// (`ArchiveSource::len`) needs before it reads anything.
pub async fn file_len_by_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
) -> Result<u64> {
    let manifest = fetch_manifest(fetcher, keys, &manifest_hash, content_key_version).await?;
    Ok(manifest.total_size)
}

/// Bytes `[offset, offset + len)` of one file by its manifest — **the
/// byte-range walk** (`archive-import.md` § Storage): the chunks overlapping
/// the range are fetched and opened, nothing else, so a reader positioned
/// inside a multi-gigabyte file (one zip member of an export archive) costs
/// O(range), not O(file). Clamped at the end of the file; `len == 0` is an
/// empty read.
///
/// Integrity: a range has no whole-file address to verify, so every opened
/// chunk is checked against the manifest's **plaintext** hash for that index —
/// [`fetch_manifest`] verified the manifest's own bytes against the content
/// address they were fetched by, so the hash list is trusted and a swapped body
/// (the AEAD frame carries no AAD) cannot pass — plus a length check that the
/// opened window has exactly as many chunks as hashes, so a short body list
/// (fetcher or nest returning fewer chunks than asked) cannot silently truncate
/// the read. Together the hash check (content) and the length check
/// (completeness) stand in for what `download_file_bytes_by_manifest`'s
/// whole-file verify covers in one shot. This is the path with the least margin
/// — there is no second, whole-file compare behind the per-chunk one — which is
/// why the manifest's own address check is not optional here.
pub async fn download_file_range_by_manifest(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
    relative_path: &str,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    if !crate::path_guard::is_safe_relative_path(relative_path) {
        anyhow::bail!("unsafe path rejected: {relative_path}");
    }
    let manifest = fetch_manifest(fetcher, keys, &manifest_hash, content_key_version).await?;
    let end = offset.saturating_add(len).min(manifest.total_size);
    if offset >= end {
        return Ok(Vec::new());
    }
    // Locate the overlapping window of chunk indices from the size table.
    let mut first = None;
    let mut last = 0usize;
    let mut cursor = 0u64;
    let mut first_start = 0u64;
    for (i, size) in manifest.chunk_sizes.iter().enumerate() {
        let chunk_end = cursor + size;
        if first.is_none() && offset < chunk_end {
            first = Some(i);
            first_start = cursor;
        }
        if first.is_some() {
            last = i;
            if end <= chunk_end {
                break;
            }
        }
        cursor = chunk_end;
    }
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    let policy = keys.finalize_open(
        content_key_version,
        resolve_chunk_open_policy(keys, &manifest, relative_path, content_key_version),
    )?;
    let store_keys = manifest.store_keys();
    let window = &store_keys[first..=last];
    let bodies = fetcher.fetch_chunks(window, relative_path).await?;
    let hashes = &manifest.chunk_hashes[first..=last];
    // `open_chunk_window` is the whole integrity story for a range read: it
    // refuses a body count that does not match the hash window (a fetcher or
    // nest silently returning a short body list) and hands back one plaintext
    // per hash, each verified to address its recorded content — so, unlike
    // the whole-file walks, there is no file-level hash to fall back on and
    // nothing left to re-check here.
    let opened = keys.finalize_open(
        content_key_version,
        open_chunk_window(&policy, hashes, bodies),
    )?;
    let mut out = Vec::with_capacity((end - offset) as usize);
    let mut pos = first_start;
    for plain in &opened {
        let chunk_end = pos + plain.len() as u64;
        let take_from = offset.max(pos);
        let take_to = end.min(chunk_end);
        if take_from < take_to {
            out.extend_from_slice(&plain[(take_from - pos) as usize..(take_to - pos) as usize]);
        }
        pos = chunk_end;
    }
    Ok(out)
}

/// Download one file of a followed public folder — **the follower's byte read**,
/// in one shared place so no app re-derives which key material a follower holds.
///
/// The answer is **none**, and that is the whole point of this function
/// existing. `fetcher` is bound to the folder's *home* nest (the record's
/// `home_nest_url`; the byte routes are open and CORS-open, integrity is by
/// content address, and the record's `home_nest_actor_id` is the SPKI pin the
/// dial is made under — `folders.md` § Publicly-synced follow, *Reads ride
/// existing planes end-to-end*), and the walk opens under
/// [`FileDownloadKeys::default`] — no owner key, no content keys.
///
/// ⚠ **Do not "helpfully" pass the reader's own `BackupKey` here.** A plaintext
/// manifest passes through under *any* key state, so a wrongly-keyed follower
/// read would succeed in every happy path and only diverge where it matters: a
/// manifest that is somehow sealed would then be attempted against the
/// follower's own root instead of failing closed. Keyless makes the refusal
/// structural — a public folder's content is plaintext by the audience
/// contract, so a sealed manifest on this plane is a bug to surface loudly, not
/// a key to hunt for.
///
/// `relative_path` is the change row's path (public rows carry it in the clear —
/// `path_sealed` is one of the four fields the stripped projection omits).
///
/// Lives here (rather than its original home,
/// `fauna_client_folders::public_follow`, which re-exports it) so the Media
/// machine's followed browse scope can reach it without a dependency cycle —
/// `fauna-client-folders` depends on `fauna-media-machine` through its `mls`
/// feature, so the machine can never depend back.
pub async fn download_followed_file(
    fetcher: &dyn BlobFetcher,
    manifest_hash: ContentHash,
    relative_path: &str,
) -> Result<Vec<u8>> {
    download_file_bytes_by_manifest(
        fetcher,
        &FileDownloadKeys::default(),
        manifest_hash,
        None,
        relative_path,
    )
    .await
}

/// The browser leg of [`BlobFetcher`]: GET the nest's public content-addressed
/// blob routes (`/api/v1/manifests/{hash}`, `/api/v1/chunks/{hash}`) over
/// `gloo-net`. The one wasm binding shared by every wasm consumer of this
/// walk — `fauna-wasm` (Backups per-file download; the mail client-feed
/// reference leg, `fauna_mail::body_ref`) and `fauna-media-machine`'s wasm
/// `download_fetcher` (the Media page, for clients with no sync engine) — so
/// there is exactly one wasm impl of this seam rather than three duplicates.
/// Mirrors native's [`NestPublicChunkFetcher`](https://docs.rs/) in
/// `fauna-client` (the crate this lives in, `fauna-core`, is the neutral home
/// on wasm: both consumers already depend on it, and it is the one crate in
/// this walk's dependency graph that compiles to wasm32 unconditionally — see
/// the module docs).
///
/// Carries **no bearer**: both routes are open
/// (`bins/fauna-nest/src/chunk_routes.rs` has no auth extractor) —
/// confidentiality is cryptographic (the bytes are already sealed), and
/// integrity rests on the content address each blob is fetched *by*, not on
/// who asked.
#[cfg(all(target_arch = "wasm32", feature = "blob-fetch-wasm"))]
pub struct WasmPublicChunkFetcher {
    nest_url: String,
}

#[cfg(all(target_arch = "wasm32", feature = "blob-fetch-wasm"))]
impl WasmPublicChunkFetcher {
    /// Build over `nest_url` (the SPA's current nest, trailing slash already
    /// trimmed by the caller — every existing constructor site did this).
    pub fn new(nest_url: impl Into<String>) -> Self {
        Self {
            nest_url: nest_url.into(),
        }
    }

    async fn get(&self, path: &str) -> Result<Vec<u8>> {
        let url = format!("{}{path}", self.nest_url);
        let resp = gloo_net::http::Request::get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GET {path}: {e}"))?;
        let status = resp.status();
        if !(200..300).contains(&status) {
            let message = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {path} failed ({status}): {message}");
        }
        resp.binary()
            .await
            .map_err(|e| anyhow::anyhow!("GET {path} read body: {e}"))
    }
}

#[cfg(all(target_arch = "wasm32", feature = "blob-fetch-wasm"))]
#[async_trait::async_trait(?Send)]
impl BlobFetcher for WasmPublicChunkFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.get(&format!("/api/v1/manifests/{}", hex::encode(hash.digest())))
            .await
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        // Sequential: the browser already pipelines over one connection, and
        // the seam hands the whole batch to this method precisely so a target
        // can choose — if this ever needs parallelism,
        // `futures::stream::buffer_unordered` here is the change, and no
        // shared code moves.
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let bytes = self
                .get(&format!("/api/v1/chunks/{}", hex::encode(key.digest())))
                .await
                .map_err(|e| anyhow::anyhow!("downloading chunk for {relative_path}: {e}"))?;
            out.push(bytes);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::chunk::ChunkManifest;

    /// A blob store in a HashMap. The whole point of the seam: the walk is
    /// exercised end to end with no nest, no HTTP and no `SyncDb` — which is
    /// exactly the shape it runs in on wasm.
    #[derive(Default)]
    struct FakeFetcher {
        blobs: HashMap<Vec<u8>, Vec<u8>>,
        /// Every `fetch_chunks` call's store-key batch, in call order — lets a
        /// test assert exactly which chunks a walk fetched (the O(range), not
        /// O(file), property `download_file_range_by_manifest` exists for).
        /// `fetch_chunks` takes `&self` (the trait's own signature, unchanged
        /// here) and `BlobFetcher: MaybeSendSync` requires `Sync`, so
        /// recording needs a `Mutex`, not a `RefCell`.
        fetched: Mutex<Vec<Vec<ContentHash>>>,
        /// When set, `fetch_chunks` silently returns one body fewer than the
        /// store keys it was asked for — the "fetcher/nest returns a short
        /// body list" shape `open_chunk_window`'s hash-vs-body count check
        /// exists to catch (a range read has no whole-file hash behind it).
        drop_last: AtomicBool,
    }

    impl FakeFetcher {
        fn put(&mut self, hash: &ContentHash, bytes: Vec<u8>) {
            self.blobs.insert(hash.digest().to_vec(), bytes);
        }

        fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.blobs
                .get(hash.digest().as_slice())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404 {}", hex::encode(hash.digest())))
        }

        /// Every `fetch_chunks` batch recorded so far, in call order.
        fn fetched(&self) -> Vec<Vec<ContentHash>> {
            self.fetched.lock().unwrap().clone()
        }

        /// Reset the fetch log so a test can assert one read's window at a
        /// time instead of the cumulative history.
        fn clear_fetched(&self) {
            self.fetched.lock().unwrap().clear();
        }

        /// Arm the short-body-list fault: the next (and every subsequent)
        /// `fetch_chunks` call returns one body fewer than requested.
        fn drop_last_chunk(&self) {
            self.drop_last.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl BlobFetcher for FakeFetcher {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.get(hash)
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _path: &str,
        ) -> Result<Vec<Vec<u8>>> {
            self.fetched.lock().unwrap().push(store_keys.to_vec());
            let mut bodies: Vec<Vec<u8>> = store_keys
                .iter()
                .map(|k| self.get(k))
                .collect::<Result<_>>()?;
            if self.drop_last.load(Ordering::SeqCst) {
                bodies.pop();
            }
            Ok(bodies)
        }
    }

    /// Seal `chunks` under `root` the way the upload path does — hash the
    /// plaintext, compress, encrypt, address by ciphertext — and stock a fetcher
    /// with the manifest + chunk blobs. Returns the fetcher, the manifest hash,
    /// and the expected plaintext file.
    fn seal_file(root: &[u8; 32], chunks: &[&[u8]]) -> (FakeFetcher, ContentHash, Vec<u8>) {
        let plaintext: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        let hashes: Vec<ContentHash> = plaintext.iter().map(|c| ContentHash::of_raw(c)).collect();
        // Encode order: hash(plaintext) -> compress -> encrypt -> store.
        let pairs: Vec<(ContentHash, Vec<u8>)> = hashes
            .iter()
            .cloned()
            .zip(plaintext.iter().map(|c| crate::compress::compress_chunk(c)))
            .collect();
        let ciphertexts: Vec<Vec<u8>> = pairs
            .iter()
            .map(|(h, d)| crate::chunk_crypto::encrypt_chunk(root, h, d).unwrap())
            .collect();
        let store_keys: Vec<ContentHash> =
            ciphertexts.iter().map(|c| ContentHash::of_raw(c)).collect();

        let file_data = crate::chunker::reassemble_chunks(&plaintext);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&file_data),
            total_size: file_data.len() as u64,
            chunk_hashes: hashes,
            chunk_sizes: plaintext.iter().map(|c| c.len() as u64).collect(),
            stored_hashes: Some(store_keys.clone()),
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes =
            crate::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);

        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        for (key, body) in store_keys.iter().zip(ciphertexts) {
            fetcher.put(key, body);
        }
        (fetcher, manifest_hash, file_data)
    }

    fn owner_key() -> crate::crypto::BackupKey {
        crate::crypto::BackupKey::from_bytes([0x51u8; 32])
    }

    /// The web Backups case end to end: a sealed owner-only file walks back to
    /// its plaintext over nothing but the blob seam.
    #[tokio::test]
    async fn owner_sealed_file_round_trips_over_the_seam() {
        let key = owner_key();
        // >4 KiB and highly compressible, so the zstd arm of the prefix actually
        // runs — the arm a reader that skipped decompression would fail on.
        let big = vec![b'a'; 8192];
        let (fetcher, manifest_hash, expected) = seal_file(
            &key.convergent_chunk_root(),
            &[b"hello ", big.as_slice(), b" world"],
        );

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            manifest_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect("sealed owner file downloads");

        assert_eq!(got, expected);
    }

    /// A chunk sealed **unframed** — no `compress` stage — whose plaintext
    /// begins with a frame byte. By inspection it is an uncompressed frame; the
    /// manifest's plaintext hash says it is not. The walk lets the hash decide
    /// (framed first, the raw body as the fallback), exactly as the nest web
    /// reader does through `compress::unframe_verified_chunk`.
    #[tokio::test]
    async fn an_unframed_chunk_beginning_with_a_frame_byte_opens_through_the_walk() {
        let key = owner_key();
        let root = key.convergent_chunk_root();
        // Leading 0x00: indistinguishable from an uncompressed frame by inspection.
        let plain = [
            b"\x00".as_slice(),
            b"unframed chunk sealed raw by a raw-body writer",
        ]
        .concat();
        let hash = ContentHash::of_raw(&plain);
        // Sealed raw through the primitive — no `compress` stage — which is what
        // `chunk_seal` now makes unreachable from outside this crate.
        let raw_sealed = crate::chunk_crypto::encrypt_chunk(&root, &hash, &plain).unwrap();
        let store_key = ContentHash::of_raw(&raw_sealed);

        let manifest = ChunkManifest {
            file_hash: hash,
            total_size: plain.len() as u64,
            chunk_hashes: vec![hash],
            chunk_sizes: vec![plain.len() as u64],
            stored_hashes: Some(vec![store_key]),
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes =
            crate::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        fetcher.put(&store_key, raw_sealed);

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            manifest_hash,
            None,
            "docs/raw.bin",
        )
        .await
        .expect("an unframed chunk opens — the hash, not the first byte, decides");
        assert_eq!(got, plain);
    }

    /// The identity-succession aftermath, read half
    /// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row:
    /// *"the successor holds the old seed to unseal what it now owns"*).
    ///
    /// A successor's client derives its owner key from the **successor's** seed,
    /// while every chunk already at rest is sealed under the **predecessor's**.
    /// Nest-side ownership re-points in the succession transaction, so the fetch
    /// succeeds and only the AEAD tag fails — i.e. without this the successor's
    /// whole corpus is dark and indistinguishable from corruption.
    #[tokio::test]
    async fn a_successor_opens_a_predecessor_sealed_file() {
        let predecessor = crate::crypto::BackupKey::from_bytes([0x11u8; 32]);
        let successor = crate::crypto::BackupKey::from_bytes([0x22u8; 32]);
        let (fetcher, manifest_hash, expected) = seal_file(
            &predecessor.convergent_chunk_root(),
            &[b"pre-succession bytes"],
        );

        let keys = FileDownloadKeys {
            backup_key: Some(successor.into()),
            predecessor_backup_keys: vec![predecessor.into()],
            ..Default::default()
        };

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            None,
            "photos/before.bin",
        )
        .await
        .expect("the successor opens what it now owns");

        assert_eq!(got, expected);
    }

    /// The WebDAV serve-toggle twin of the succession case above — `webdav-server.md` § Key model, Revocation: serve-OFF rotates
    /// a set's content key and drops it from the engine's *live* binding
    /// (`content_keys = None`), but the owner's own custody still retains the
    /// retired generation a served window sealed chunks under. An owner-only
    /// reader offered that generation as a **retired** candidate — never as
    /// `content_keys` itself, which would wrongly re-trip the content-keyed
    /// gate — must still open what serving-while-it-was-on sealed.
    #[tokio::test]
    async fn an_owner_reader_opens_a_served_era_content_key_sealed_file() {
        let owner = crate::crypto::BackupKey::from_bytes([0x33u8; 32]);
        let served_generation = crate::folder_keys::FolderContentKeys::genesis([0x44u8; 32], 1_000);
        let (fetcher, manifest_hash, expected) = seal_file(
            served_generation.current_key(),
            &[b"sealed while this set was WebDAV-served"],
        );

        let keys = FileDownloadKeys {
            backup_key: Some(owner.into()),
            retired_content_keys: Some(served_generation.clone()),
            ..Default::default()
        };
        // The gate this field must never trip: an owner-only reader carrying
        // only RETIRED content-key candidates is still owner-keyed, not
        // content-keyed.
        assert!(keys.content_keys.is_none());

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            Some(served_generation.current_version()),
            "docs/served-while-on.bin",
        )
        .await
        .expect("the owner path opens a served-era content-key-sealed file");

        assert_eq!(got, expected);
    }

    /// The byte-range walk (`archive-import.md` § Storage): only the chunks
    /// overlapping `[offset, offset+len)` are fetched and opened — asserted
    /// on the fetcher's own call log, not merely inferred from the returned
    /// bytes (a whole-file fetch sliced afterwards would satisfy every byte
    /// assertion here) — each verified against the manifest's plaintext hash,
    /// and the slice is exact at every chunk boundary.
    #[tokio::test]
    async fn a_range_read_fetches_only_the_overlapping_chunks_and_slices_exactly() {
        let key = owner_key();
        let a = vec![b'a'; 5000];
        let b = vec![b'b'; 5000];
        let c = vec![b'c'; 5000];
        let (fetcher, manifest_hash, expected) =
            seal_file(&key.convergent_chunk_root(), &[&a, &b, &c]);
        let keys = FileDownloadKeys::owner(key);
        let manifest: ChunkManifest =
            crate::encoding::canonical_decode(&fetcher.get(&manifest_hash).unwrap()).unwrap();
        let store_keys = manifest.store_keys();
        let read = |offset: u64, len: u64| {
            let fetcher = &fetcher;
            let keys = &keys;
            async move {
                download_file_range_by_manifest(
                    fetcher,
                    keys,
                    manifest_hash,
                    None,
                    "raw/export.zip",
                    offset,
                    len,
                )
                .await
                .expect("range")
            }
        };

        // Inside one chunk: only chunk 0 is ever fetched.
        assert_eq!(read(10, 20).await, &expected[10..30]);
        assert_eq!(fetcher.fetched(), vec![store_keys[0..=0].to_vec()]);
        fetcher.clear_fetched();

        // Straddling a boundary: both straddled chunks, nothing past them.
        assert_eq!(read(4990, 20).await, &expected[4990..5010]);
        assert_eq!(fetcher.fetched(), vec![store_keys[0..=1].to_vec()]);
        fetcher.clear_fetched();

        // Exactly one chunk: chunk 1 alone, not chunk 0 or chunk 2.
        assert_eq!(read(5000, 5000).await, &expected[5000..10000]);
        assert_eq!(fetcher.fetched(), vec![store_keys[1..=1].to_vec()]);
        fetcher.clear_fetched();

        // Past the end is clamped, never an error — and only the last chunk
        // is fetched for it, not the whole file.
        assert_eq!(read(14990, 100).await, &expected[14990..]);
        assert_eq!(fetcher.fetched(), vec![store_keys[2..=2].to_vec()]);
        fetcher.clear_fetched();

        // Wholly past EOF or zero-length: no fetch at all.
        assert!(read(15000, 10).await.is_empty());
        assert!(
            fetcher.fetched().is_empty(),
            "past-EOF read fetches nothing"
        );
        assert_eq!(read(0, 0).await, Vec::<u8>::new());
        assert!(
            fetcher.fetched().is_empty(),
            "zero-length read fetches nothing"
        );

        assert_eq!(
            file_len_by_manifest(&fetcher, &keys, manifest_hash, None)
                .await
                .unwrap(),
            15000
        );
    }

    /// A sealed manifest's chunk is content-bound (`chunk_crypto` derives
    /// key+nonce from the plaintext hash), so a body swapped for another
    /// chunk's fails to **decrypt** under the hash the manifest names —
    /// `open_chunk_window` errs before its per-chunk unframe-verify ever runs
    /// for this manifest shape. Distinct from
    /// `a_range_read_on_a_plaintext_manifest_refuses_a_swapped_chunk_body`
    /// below, which is what actually exercises that verify.
    #[tokio::test]
    async fn a_range_read_on_a_sealed_manifest_refuses_a_swapped_chunk_body() {
        let key = owner_key();
        let (mut fetcher, manifest_hash, _) = seal_file(
            &key.convergent_chunk_root(),
            &[&[b'x'; 5000], &[b'y'; 5000]],
        );
        let manifest: ChunkManifest =
            crate::encoding::canonical_decode(&fetcher.get(&manifest_hash).unwrap()).unwrap();
        let keys_of = manifest.store_keys();
        let second = fetcher.get(&keys_of[1]).unwrap();
        fetcher.put(&keys_of[0], second);
        let err = download_file_range_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            manifest_hash,
            None,
            "raw/export.zip",
            0,
            10,
        )
        .await
        .expect_err("a swapped sealed body is refused");
        assert!(
            format!("{err:#}").contains("chunk decryption failed"),
            "{err:#}"
        );
    }

    /// The plaintext-manifest arm is where `open_chunk_window`'s per-chunk
    /// unframe-verify is the **only** thing that can catch a swap (bodies
    /// pass through undecrypted, and a range read has no whole-file address
    /// to fall back on — contrast
    /// `substituted_plaintext_chunk_is_caught_by_the_whole_file_address`):
    /// serving chunk 1's compressed body at chunk 0's stored address must be
    /// refused as not addressing chunk 0's recorded content, never silently
    /// returned as chunk 0's bytes.
    #[tokio::test]
    async fn a_range_read_on_a_plaintext_manifest_refuses_a_swapped_chunk_body() {
        let x = vec![b'x'; 5000];
        let y = vec![b'y'; 5000];
        let hash_x = ContentHash::of_raw(&x);
        let hash_y = ContentHash::of_raw(&y);
        let file_data = crate::chunker::reassemble_chunks(&[x.clone(), y.clone()]);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&file_data),
            total_size: file_data.len() as u64,
            chunk_hashes: vec![hash_x, hash_y],
            chunk_sizes: vec![x.len() as u64, y.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = crate::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);

        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        fetcher.put(&hash_x, crate::compress::compress_chunk(&x));
        fetcher.put(&hash_y, crate::compress::compress_chunk(&y));
        // Serve chunk 1's body at chunk 0's stored (== plaintext-hash) address.
        fetcher.put(&hash_x, crate::compress::compress_chunk(&y));

        let err = download_file_range_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(owner_key()),
            manifest_hash,
            None,
            "raw/export.zip",
            0,
            10,
        )
        .await
        .expect_err("a swapped plaintext body is refused");
        assert!(
            err.to_string()
                .contains("chunk 0 does not address its recorded content"),
            "expected open_chunk_window's per-chunk verify to catch it, got: {err}"
        );
    }

    /// The `ChunkOpenPolicy::Plaintext` arm passes fetched bodies straight
    /// through undecrypted, so nothing downstream would notice a fetcher or
    /// nest that silently returned fewer bodies than asked — and a range read
    /// has no whole-file hash to catch a short result. `open_chunk_window`'s
    /// hash-vs-body count check, shared by every walk, is what closes that
    /// gap; this pins it on the one walk where it is the only guard.
    #[tokio::test]
    async fn a_range_read_on_a_plaintext_manifest_errs_on_a_short_chunk_window() {
        let x = vec![b'x'; 5000];
        let y = vec![b'y'; 5000];
        let hash_x = ContentHash::of_raw(&x);
        let hash_y = ContentHash::of_raw(&y);
        let file_data = crate::chunker::reassemble_chunks(&[x.clone(), y.clone()]);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&file_data),
            total_size: file_data.len() as u64,
            chunk_hashes: vec![hash_x, hash_y],
            chunk_sizes: vec![x.len() as u64, y.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = crate::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);

        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        fetcher.put(&hash_x, crate::compress::compress_chunk(&x));
        fetcher.put(&hash_y, crate::compress::compress_chunk(&y));
        fetcher.drop_last_chunk();

        // Spans both chunks, so the window asks for 2 store keys but the
        // fake only returns 1.
        let err = download_file_range_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(owner_key()),
            manifest_hash,
            None,
            "raw/export.zip",
            0,
            10000,
        )
        .await
        .expect_err("a short chunk window must be refused, not silently truncated");
        assert!(
            err.to_string().contains("2 chunk hashes but 1 bodies"),
            "expected open_chunk_window's count check to catch it, got: {err}"
        );
    }

    /// `predecessors_of` walks to **every** ancestor, not just the immediate one
    /// (`fauna-client-accounts/src/lib.rs:1167`), precisely because a corpus can
    /// still be sealed under a grandpredecessor when an intermediate re-seal
    /// never finished. The reader must therefore try every offered root, not
    /// just the first.
    #[tokio::test]
    async fn a_successor_opens_a_grandpredecessor_sealed_file() {
        let grandpredecessor = crate::crypto::BackupKey::from_bytes([0x31u8; 32]);
        let predecessor = crate::crypto::BackupKey::from_bytes([0x32u8; 32]);
        let successor = crate::crypto::BackupKey::from_bytes([0x33u8; 32]);
        let (fetcher, manifest_hash, expected) = seal_file(
            &grandpredecessor.convergent_chunk_root(),
            &[b"two successions ago"],
        );

        let keys = FileDownloadKeys {
            backup_key: Some(successor.into()),
            predecessor_backup_keys: vec![predecessor.into(), grandpredecessor.into()],
            ..Default::default()
        };

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            None,
            "photos/older.bin",
        )
        .await
        .expect("the successor opens a grandpredecessor-sealed file");

        assert_eq!(got, expected);
    }

    /// FS-5DC, applied to the new field: a **bound** set's chunks are content-keyed
    /// and never owner-keyed, so a bound reader must suppress predecessor roots
    /// exactly as it already suppresses `backup_key` — otherwise the new field
    /// re-opens the shadowing hole that gating on `mls_group_id` closed.
    ///
    /// The shape is the one FS-5DC was written for, and the only one that can
    /// witness the suppression: the reader holds a current owner key **and** the
    /// bound set's content keys at once (the bearer-only hydration service builds
    /// exactly this). A reader with `backup_key: None` proves nothing here — it
    /// takes the empty branch whether or not the gate exists, which is how the
    /// first draft of this test survived a mutation deleting the very gate it
    /// names.
    #[tokio::test]
    async fn a_bound_reader_suppresses_predecessor_roots_too() {
        let predecessor = crate::crypto::BackupKey::from_bytes([0x41u8; 32]);
        let successor = crate::crypto::BackupKey::from_bytes([0x42u8; 32]);
        // Sealed under the PREDECESSOR's owner root — the bytes a fall-through
        // would happily open.
        let (fetcher, manifest_hash, _) = seal_file(
            &predecessor.convergent_chunk_root(),
            &[b"owner-keyed bytes"],
        );

        let keys = FileDownloadKeys {
            backup_key: Some(successor.into()),
            predecessor_backup_keys: vec![predecessor.into()],
            mls_group_id: Some(vec![9u8; 32]),
            content_keys: Some(crate::folder_keys::FolderContentKeys::genesis(
                [0x43u8; 32],
                1_000,
            )),
            ..Default::default()
        };

        download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, Some(1), "shared/f.bin")
            .await
            .expect_err("a bound reader must not fall through to a predecessor owner root");
    }

    /// Opening offers candidates; **sealing must commit to exactly one root**, and
    /// that root is the current key — never a retired one. A write under a
    /// predecessor root would be the aftermath re-creating the very state it
    /// exists to retire, and would do so silently (a wrong label root degrades to
    /// `Omit`, never to an error).
    #[test]
    fn sealing_never_selects_a_predecessor_root() {
        use crate::path_crypto::{LabelField, seal_convergent};

        let predecessor = crate::crypto::BackupKey::from_bytes([0x51u8; 32]);
        let successor = crate::crypto::BackupKey::from_bytes([0x52u8; 32]);
        let (current, retired) = (
            successor.convergent_chunk_root(),
            predecessor.convergent_chunk_root(),
        );

        let keys = FileDownloadKeys {
            backup_key: Some(successor.into()),
            predecessor_backup_keys: vec![predecessor.into()],
            ..Default::default()
        };

        let root = keys
            .label_seal_root()
            .expect("an owner-only reader can seal")
            .expect("a keyed reader has a root");

        // Behavioural, not structural: `LabelRoot` keeps its secret private on
        // purpose, so prove which root it committed to by opening the envelope.
        let salt = [0x7au8; 32];
        let sealed = seal_convergent(
            &root,
            &salt,
            LabelField::SyncChangePath,
            b"photos/before.bin",
        )
        .unwrap();
        crate::path_crypto::open([&current], &salt, LabelField::SyncChangePath, &sealed)
            .expect("a new label seals under the CURRENT key");
        crate::path_crypto::open([&retired], &salt, LabelField::SyncChangePath, &sealed)
            .expect_err("a new label must never seal under a retired key");
    }

    /// The label plane travels with the chunk plane: a successor that could open
    /// its bytes but not its *names* renders an empty file list over a corpus it
    /// can read (`file-sync.md` § Sealed names & paths — a wrong root degrades to
    /// `Omit`, so the failure is silent).
    #[test]
    fn predecessor_roots_are_offered_for_sealed_labels() {
        let predecessor = crate::crypto::BackupKey::from_bytes([0x61u8; 32]);
        let successor = crate::crypto::BackupKey::from_bytes([0x62u8; 32]);
        let (current, retired) = (
            successor.convergent_chunk_root(),
            predecessor.convergent_chunk_root(),
        );

        let keys = FileDownloadKeys {
            backup_key: Some(successor.into()),
            predecessor_backup_keys: vec![predecessor.into()],
            ..Default::default()
        };

        let roots = keys.label_open_roots(None).expect("unbound label roots");
        assert_eq!(
            roots,
            vec![current, retired],
            "current first, then predecessors in registry order"
        );
    }

    /// The chain the per-signer bound pins run over: successor S ← predecessor
    /// P ← grandpredecessor G, nearest hop first, each key named.
    struct Chain {
        successor: crate::crypto::BackupKey,
        predecessor: crate::crypto::BackupKey,
        grandpredecessor: crate::crypto::BackupKey,
        p_id: crate::identity::ActorId,
        g_id: crate::identity::ActorId,
    }

    impl Chain {
        fn new() -> Self {
            Self {
                successor: crate::crypto::BackupKey::from_bytes([0x22u8; 32]),
                predecessor: crate::crypto::BackupKey::from_bytes([0x11u8; 32]),
                grandpredecessor: crate::crypto::BackupKey::from_bytes([0x05u8; 32]),
                p_id: crate::identity::ActorId([0xB1; 32]),
                g_id: crate::identity::ActorId([0xA0; 32]),
            }
        }

        /// An owner-only reader holding the whole named chain, reading a
        /// record signed as `signer`.
        fn keys(&self, signer: RecordSigner) -> FileDownloadKeys {
            FileDownloadKeys {
                backup_key: Some(self.successor.clone().into()),
                predecessor_backup_keys: PredecessorSealKey::chain([
                    (self.p_id, self.predecessor.clone()),
                    (self.g_id, self.grandpredecessor.clone()),
                ]),
                record_signer: signer,
                ..Default::default()
            }
        }

        /// The same reader on a content-keyed set, the record unstamped and
        /// signed as this account (part (D)'s arm).
        fn content_keyed(&self, signer: RecordSigner) -> FileDownloadKeys {
            FileDownloadKeys {
                mls_group_id: Some(vec![9u8; 32]),
                content_keys: Some(crate::folder_keys::FolderContentKeys::genesis(
                    [0x43u8; 32],
                    1_000,
                )),
                owner_signed_record: true,
                ..self.keys(signer)
            }
        }

        fn roots(&self) -> [[u8; 32]; 3] {
            [
                self.successor.convergent_chunk_root(),
                self.predecessor.convergent_chunk_root(),
                self.grandpredecessor.convergent_chunk_root(),
            ]
        }
    }

    /// Ruling (8)(c), the per-signer bound, at the one place roots are chosen:
    /// of the owner-root family a record signed as predecessor A is offered
    /// only A's root and its predecessors' — on the owner-only chunk/manifest
    /// arm, part (D)'s unstamped arm and the unstamped label arm alike; a
    /// record signed as the current identity is offered all of them.
    #[test]
    fn each_arm_offers_a_signer_only_its_own_root_and_earlier_ones() {
        let chain = Chain::new();
        let [s, p, g] = chain.roots();
        let cases = [
            (RecordSigner::Current, vec![s, p, g]),
            (RecordSigner::Predecessor(chain.p_id), vec![p, g]),
            (RecordSigner::Predecessor(chain.g_id), vec![g]),
            // Not in this holder's chain: nothing of the owner family.
            (
                RecordSigner::Predecessor(crate::identity::ActorId([0x77; 32])),
                vec![],
            ),
            // Another writer: the owner-root family is not theirs to open.
            (RecordSigner::Other, vec![]),
        ];
        for (signer, want) in cases {
            assert_eq!(chain.keys(signer).owner_open_roots(), want, "{signer:?}");
            assert_eq!(
                chain.keys(signer).label_open_roots(None).unwrap(),
                want,
                "label, {signer:?}"
            );
            assert_eq!(
                chain.content_keyed(signer).prebind_owner_roots().unwrap(),
                want,
                "unstamped, {signer:?}"
            );
        }
    }

    /// A key handed over without its identity cannot be placed in the chain,
    /// so a predecessor-signed record is never offered it; a record signed as
    /// the current identity still is.
    #[test]
    fn an_unnamed_retired_root_is_offered_only_to_the_current_identity() {
        let chain = Chain::new();
        let [s, p, _] = chain.roots();
        let unnamed = |signer| FileDownloadKeys {
            predecessor_backup_keys: vec![chain.predecessor.clone().into()],
            ..chain.keys(signer)
        };
        assert_eq!(
            unnamed(RecordSigner::Current).owner_open_roots(),
            vec![s, p]
        );
        assert!(
            unnamed(RecordSigner::Predecessor(chain.p_id))
                .owner_open_roots()
                .is_empty()
        );
    }

    /// A host that admitted a predecessor's row by the statement walk but
    /// holds no root of that identity opens nothing — and says so as a final
    /// failure, so the cursor passes the row instead of holding below it.
    #[tokio::test]
    async fn a_signer_this_host_cannot_place_is_a_final_failure() {
        let chain = Chain::new();
        let [_, p, _] = chain.roots();
        let (fetcher, manifest_hash, _) = seal_file(&p, &[b"inherited"]);
        let keys = FileDownloadKeys {
            predecessor_backup_keys: Vec::new(),
            ..chain.keys(RecordSigner::Predecessor(chain.p_id))
        };
        for (arm, keys) in [
            ("owner-only", keys.clone()),
            (
                "content-keyed, unstamped",
                FileDownloadKeys {
                    predecessor_backup_keys: Vec::new(),
                    ..chain.content_keyed(RecordSigner::Predecessor(chain.p_id))
                },
            ),
        ] {
            let err = download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "f")
                .await
                .expect_err(arm);
            assert_eq!(
                crate::apply_failure::permanent_reason(&err),
                Some(crate::apply_failure::PermanentApplyFailure::SIGNER_BOUND.reason),
                "{arm}: {err:#}"
            );
        }
    }

    /// The attack, end to end on the byte walk: the retired seed plus a lying
    /// nest name, in one of the account's sets, the manifest of a file sealed
    /// under the CURRENT root — or under a root later in the chain than the
    /// signer — and it does not open, on an owner-only set and on a
    /// content-keyed one. The signer's own and earlier roots still open.
    #[tokio::test]
    async fn a_predecessor_signature_never_opens_a_later_root() {
        let chain = Chain::new();
        let [s, p, g] = chain.roots();
        let as_p = RecordSigner::Predecessor(chain.p_id);
        let as_g = RecordSigner::Predecessor(chain.g_id);
        // (sealing root, signer, opens?)
        let cases = [
            (s, as_p, false),
            (s, as_g, false),
            (p, as_g, false),
            (p, as_p, true),
            (g, as_p, true),
            (g, as_g, true),
            (s, RecordSigner::Current, true),
        ];
        for (root, signer, opens) in cases {
            let (fetcher, manifest_hash, expected) = seal_file(&root, &[b"bytes at rest"]);
            for (arm, keys) in [
                ("owner-only", chain.keys(signer)),
                ("content-keyed, unstamped", chain.content_keyed(signer)),
            ] {
                let got =
                    download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "f.bin")
                        .await;
                if opens {
                    assert_eq!(got.expect(arm), expected, "{arm}, {signer:?}");
                } else {
                    let err = got.expect_err(arm);
                    // A noted skip, never a transient hold (ruling (8)(c)).
                    assert_eq!(
                        crate::apply_failure::permanent_reason(&err),
                        Some(crate::apply_failure::PermanentApplyFailure::SIGNER_BOUND.reason),
                        "{arm}: {signer:?} opened a later root, or held: {err:#}"
                    );
                }
            }
        }
    }

    /// [`seal_file`], named for the arm it exercises: its manifest's hashes
    /// are sealed under `root` too (`sealed_hashes`, the shape every writer
    /// emits), so a read crosses the manifest arm's root choice before the
    /// chunk arm's.
    fn seal_file_and_manifest(
        root: &[u8; 32],
        chunks: &[&[u8]],
    ) -> (FakeFetcher, ContentHash, Vec<u8>) {
        seal_file(root, chunks)
    }

    /// Ruling (10)(c), the stamp binds the root: on an owner-only set a
    /// record carrying a `content_key_version` is never offered an
    /// owner-family root — not the current one, not a predecessor's, whoever
    /// signed it — on the manifest arm, the chunk arm or the bounded walk.
    /// The attack it closes: the retired seed and a lying nest stamp a row
    /// naming bytes the successor sealed, a restore re-points it verbatim
    /// because it is stamped, and every owner-only reader opened it. The
    /// failure is final for the row (nothing a pull delivers gives an
    /// owner-only reader a generation its custody does not carry), so it
    /// never caps the batch.
    #[tokio::test]
    async fn a_stamped_record_never_opens_under_an_owner_root_on_an_owner_only_set() {
        let chain = Chain::new();
        let [s, p, _] = chain.roots();
        let stamp_bound = Some(crate::apply_failure::PermanentApplyFailure::STAMP_BOUND.reason);
        for (root, sealer, signer) in [
            (s, "current root", RecordSigner::Current),
            (s, "current root", RecordSigner::Predecessor(chain.p_id)),
            (p, "predecessor root", RecordSigner::Predecessor(chain.p_id)),
            (p, "predecessor root", RecordSigner::Current),
        ] {
            let keys = chain.keys(signer);
            let case = format!("sealed under the {sealer}, signed {signer:?}");

            // The manifest arm.
            let (fetcher, manifest_hash, _) = seal_file_and_manifest(&root, &[b"successor's"]);
            let err = fetch_manifest(&fetcher, &keys, &manifest_hash, Some(7))
                .await
                .expect_err(&case);
            assert_eq!(
                crate::apply_failure::permanent_reason(&err),
                stamp_bound,
                "manifest arm, {case}: {err:#}"
            );

            // The chunk arm (a manifest with no sealed hashes reaches it
            // directly), whole-buffer and bounded walk alike.
            let (fetcher, manifest_hash, _) = seal_file(&root, &[b"successor's"]);
            let err =
                download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, Some(7), "f.bin")
                    .await
                    .expect_err(&case);
            assert_eq!(
                crate::apply_failure::permanent_reason(&err),
                stamp_bound,
                "chunk arm, {case}: {err:#}"
            );
            let dir = tempfile::tempdir().unwrap();
            let err = download_file_to_path_by_manifest(
                &fetcher,
                &keys,
                manifest_hash,
                Some(7),
                "f.bin",
                &dir.path().join("f.bin"),
            )
            .await
            .expect_err(&case);
            assert_eq!(
                crate::apply_failure::permanent_reason(&err),
                stamp_bound,
                "bounded walk, {case}: {err:#}"
            );
        }
    }

    /// The other half of ruling (10)(c): a stamped record still opens under
    /// the generation it names — here a retired served-window generation an
    /// owner-only reader holds — on both arms and whoever signed it; and an
    /// unstamped record opens under the owner family exactly as before.
    #[tokio::test]
    async fn the_stamp_selects_its_generation_and_its_absence_the_owner_root() {
        let chain = Chain::new();
        let [s, ..] = chain.roots();
        let served = crate::folder_keys::FolderContentKeys::genesis([0x44u8; 32], 1_000);
        let stamp = Some(served.current_version());
        for signer in [RecordSigner::Current, RecordSigner::Predecessor(chain.p_id)] {
            let keys = FileDownloadKeys {
                retired_content_keys: Some(served.clone()),
                ..chain.keys(signer)
            };
            let (fetcher, manifest_hash, expected) =
                seal_file_and_manifest(served.current_key(), &[b"sealed while served"]);
            let got =
                download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, stamp, "f.bin")
                    .await
                    .unwrap_or_else(|e| panic!("{signer:?}: {e:#}"));
            assert_eq!(got, expected, "{signer:?}");
        }
        let (fetcher, manifest_hash, expected) =
            seal_file_and_manifest(&s, &[b"owner-sealed, unstamped"]);
        let keys = FileDownloadKeys {
            retired_content_keys: Some(served),
            ..chain.keys(RecordSigner::Current)
        };
        let got = download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "f.bin")
            .await
            .expect("an unstamped owner-only record opens under the owner root");
        assert_eq!(got, expected);
    }

    /// A sealed manifest with no key material must fail closed — never hand back
    /// ciphertext as if it were the file.
    #[tokio::test]
    async fn sealed_file_without_key_fails_closed() {
        let (fetcher, manifest_hash, expected) =
            seal_file(&owner_key().convergent_chunk_root(), &[b"secret bytes"]);

        let err = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::default(), // keyless reader
            manifest_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect_err("a keyless reader must not open a sealed file");

        let msg = err.to_string();
        assert!(msg.contains("fail closed"), "unexpected error: {msg}");
        // And emphatically not a ciphertext passthrough dressed up as success.
        assert!(!msg.contains(&String::from_utf8_lossy(&expected).to_string()));
    }

    /// A chunk substituted for another — even one *validly sealed under the same
    /// owner key* — must never reach the caller. It fails at decrypt, because
    /// `chunk_crypto` derives key+nonce from the chunk's plaintext hash: the seal
    /// is content-bound by construction, so the decoy's ciphertext cannot open
    /// under the hash the manifest names.
    #[tokio::test]
    async fn substituted_owner_sealed_chunk_cannot_open_under_the_manifest_hash() {
        let key = owner_key();
        let root = key.convergent_chunk_root();
        let (mut fetcher, manifest_hash, _) = seal_file(&root, &[b"the real chunk"]);

        let decoy = b"an entirely different chunk".to_vec();
        let decoy_sealed = crate::chunk_crypto::encrypt_chunk(
            &root,
            &ContentHash::of_raw(&decoy),
            &crate::compress::compress_chunk(&decoy),
        )
        .unwrap();
        let manifest: ChunkManifest =
            crate::encoding::canonical_decode(&fetcher.get(&manifest_hash).unwrap()).unwrap();
        let victim = manifest.store_keys()[0];
        fetcher.put(&victim, decoy_sealed);

        download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            manifest_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect_err("a substituted sealed chunk must be rejected");
    }

    /// The plaintext-manifest arm, where the content addresses are the
    /// **only** integrity check there is: nothing is decrypted, so a swapped
    /// chunk is authenticated by nothing else. Since 2026-09-03 the per-chunk
    /// verify in `open_chunk_window` (the same hash-decided unframe the nest
    /// web reader runs) catches it first and names the chunk; the whole-file address
    /// stays the backstop behind it. Drop both and a plaintext manifest silently
    /// serves attacker-chosen bytes.
    #[tokio::test]
    async fn substituted_plaintext_chunk_is_caught_by_its_content_address() {
        // A plaintext manifest: stored bytes ARE the (compressed) plaintext, so
        // `store_keys()` falls back to `chunk_hashes` and no decryption runs.
        let real = b"the real chunk".to_vec();
        let hash = ContentHash::of_raw(&real);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&real),
            total_size: real.len() as u64,
            chunk_hashes: vec![hash],
            chunk_sizes: vec![real.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = crate::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);

        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        // Serve *different* bytes at the address the manifest names.
        fetcher.put(&hash, crate::compress::compress_chunk(b"tampered bytes!"));

        let err = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(owner_key()),
            manifest_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect_err("a tampered plaintext chunk must be rejected");
        assert!(
            err.to_string()
                .contains("chunk 0 does not address its recorded content"),
            "expected the per-chunk content address to catch it and name the chunk, got: {err}"
        );
    }

    /// **The manifest is the root of the whole chain, so it gets the same
    /// address check every other fetched blob gets.** Each anchor below reads
    /// its trusted values *out of the manifest*: the whole-file walk compares
    /// against `manifest.file_hash`, the range walk against
    /// `manifest.chunk_hashes[i]`, and a sealed chunk derives its key and nonce
    /// from that same list. A nest serving a *different* file's manifest for
    /// address M therefore satisfies all three against its own numbers — the
    /// substitution is internally consistent, so nothing downstream can catch
    /// it. Only hashing the served bytes back to M does.
    ///
    /// Both walks are pinned because they fail differently without the check:
    /// the whole-file walk would return the decoy's bytes (its `file_hash`
    /// matches its own chunks), and the range walk has no whole-file address to
    /// compare at all.
    #[tokio::test]
    async fn a_manifest_for_a_different_file_is_refused_at_the_door() {
        let key = owner_key();
        let root = key.convergent_chunk_root();
        let (_, wanted_hash, wanted) = seal_file(&root, &[b"the file the caller asked for"]);
        let (mut fetcher, decoy_hash, decoy) = seal_file(&root, &[b"an entirely different file"]);
        assert_ne!(wanted, decoy, "the fixture must serve different content");

        // The nest answers the caller's address with the decoy's manifest — a
        // well-formed, internally consistent document whose chunks it also
        // holds. Everything but the address it was asked for checks out.
        let decoy_bytes = fetcher.get(&decoy_hash).unwrap();
        fetcher.put(&wanted_hash, decoy_bytes);

        let err = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key.clone()),
            wanted_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect_err("a manifest that is not the one asked for must be refused");
        assert!(
            err.to_string().contains("manifest hash mismatch"),
            "expected the manifest address check to catch it, got: {err}"
        );

        let err = download_file_range_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            wanted_hash,
            None,
            "docs/report.bin",
            0,
            8,
        )
        .await
        .expect_err("the range walk must refuse it too — it has no whole-file backstop");
        assert!(
            err.to_string().contains("manifest hash mismatch"),
            "expected the manifest address check to catch it, got: {err}"
        );
    }

    /// **The seal flag lives on the nest-supplied bytes, so it can simply be
    /// stripped.** `ChunkManifest::is_sealed()` is `sealed_hashes.is_some()`,
    /// and `stored_hashes = None` means "these bodies ARE the plaintext,
    /// regardless of this reader's key state" — a deliberate, load-bearing arm
    /// (`keyed_reader_passes_a_plaintext_manifest_through`). Together they let a
    /// nest re-serve a *sealed* file's address as a plaintext manifest over
    /// plaintext bodies: the reader's key material is never consulted, so no
    /// seal can fail, and the decoy's own `file_hash` covers the decoy's own
    /// chunks.
    ///
    /// The manifest address check is the only thing standing between that arm
    /// and a downgrade, which is why the check must come *before* the
    /// `is_sealed()` branch rather than inside it.
    #[tokio::test]
    async fn a_manifest_with_its_seal_stripped_is_refused_at_the_door() {
        let key = owner_key();
        let (mut fetcher, sealed_hash, sealed_plaintext) =
            seal_file(&key.convergent_chunk_root(), &[b"the sealed file"]);

        // The decoy: a plaintext manifest over plaintext bodies, no seal on
        // either axis, describing content the caller never asked for.
        let body = b"downgraded plaintext the nest chose".to_vec();
        let body_hash = ContentHash::of_raw(&body);
        let decoy = ChunkManifest {
            file_hash: ContentHash::of_raw(&body),
            total_size: body.len() as u64,
            chunk_hashes: vec![body_hash],
            chunk_sizes: vec![body.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let decoy_bytes = crate::encoding::canonical_encode(&decoy).unwrap();
        assert_ne!(
            body, sealed_plaintext,
            "the fixture must serve different content"
        );
        fetcher.put(&sealed_hash, decoy_bytes);
        fetcher.put(&body_hash, crate::compress::compress_chunk(&body));

        let err = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key.clone()),
            sealed_hash,
            None,
            "docs/report.bin",
        )
        .await
        .expect_err("a stripped-seal manifest must be refused before the seal branch");
        assert!(
            err.to_string().contains("manifest hash mismatch"),
            "expected the manifest address check to catch it, got: {err}"
        );

        let err = download_file_range_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            sealed_hash,
            None,
            "docs/report.bin",
            0,
            8,
        )
        .await
        .expect_err("the range walk must refuse the downgrade too");
        assert!(
            err.to_string().contains("manifest hash mismatch"),
            "expected the manifest address check to catch it, got: {err}"
        );
    }

    /// A keyed reader must still read a plaintext manifest — the
    /// arm: `stored_hashes = None` means the
    /// bytes are plaintext *regardless* of what keys this reader holds.
    #[tokio::test]
    async fn keyed_reader_passes_a_plaintext_manifest_through() {
        let body = vec![b'z'; 8192];
        let hash = ContentHash::of_raw(&body);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&body),
            total_size: body.len() as u64,
            chunk_hashes: vec![hash],
            chunk_sizes: vec![body.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = crate::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        fetcher.put(&hash, crate::compress::compress_chunk(&body));

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(owner_key()), // keyed, reading plaintext
            manifest_hash,
            None,
            "docs/plain.bin",
        )
        .await
        .expect("a keyed reader must still read a plaintext manifest");
        assert_eq!(got, body);
    }

    /// **The public follower's exact read shape** — a reader holding *no key
    /// material at all* opens a plaintext manifest
    /// (`docs/goal/behavior/folders.md` § Publicly-synced follow: manifests +
    /// chunks "ride the existing open by-hash bulk GETs … the self-describing
    /// `stored_hashes = None` shape every reader already passes through").
    ///
    /// The sibling above pins the *keyed* reader; the keyless case was only
    /// pinned for a **sealed** manifest (`sealed_file_without_key_fails_closed`
    /// — it must fail closed). Between them sat the one shape a follower
    /// actually takes, unpinned: nothing stopped a refactor from making key
    /// material a precondition of the walk and turning every public-folder read
    /// into a refusal — a break no owner-side test would have caught, because
    /// every other reader in the product holds *some* root.
    #[tokio::test]
    async fn keyless_follower_reads_a_plaintext_manifest() {
        let body = vec![b'p'; 8192];
        let hash = ContentHash::of_raw(&body);
        let manifest = ChunkManifest {
            file_hash: ContentHash::of_raw(&body),
            total_size: body.len() as u64,
            chunk_hashes: vec![hash],
            chunk_sizes: vec![body.len() as u64],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = crate::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        let mut fetcher = FakeFetcher::default();
        fetcher.put(&manifest_hash, manifest_bytes);
        fetcher.put(&hash, crate::compress::compress_chunk(&body));

        let got = download_file_bytes_by_manifest(
            &fetcher,
            &FileDownloadKeys::default(), // a follower holds no key material
            manifest_hash,
            None,
            "site/index.html",
        )
        .await
        .expect("a keyless follower must read a public folder's plaintext file");
        assert_eq!(got, body);
    }

    #[tokio::test]
    async fn traversal_path_is_rejected_before_any_fetch() {
        let err = download_file_bytes_by_manifest(
            &FakeFetcher::default(),
            &FileDownloadKeys::owner(owner_key()),
            ContentHash::of_raw(b"whatever"),
            None,
            "../../etc/passwd",
        )
        .await
        .expect_err("traversal path must be rejected");
        assert!(err.to_string().contains("unsafe path"));
    }

    // ── label_seal_root — the client-side seal selection (S6-d) ─────────────

    fn bound_keys(version: u64, key: [u8; 32]) -> FileDownloadKeys {
        FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: Some(vec![9u8; 32]),
            content_keys: Some(crate::folder_keys::FolderContentKeys {
                current: crate::folder_keys::ContentKeyGeneration {
                    version,
                    key: key.into(),
                    rotated_at: 100,
                },
                prior: Vec::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_bound_set_seals_under_the_current_generation_and_stamps_it() {
        let root = bound_keys(4, [5u8; 32])
            .label_seal_root()
            .unwrap()
            .expect("a bound set with content keys can seal");
        assert_eq!(
            root.generation(),
            Some(4),
            "the stamp must name the generation the key came from, or no reader \
             can pick the right candidate back out"
        );
    }

    /// The bounded verify walks every window (six chunks span two) and closes on
    /// the whole-file address: a manifest whose chunks all verify but whose
    /// `file_hash` names other content is refused — the per-chunk check alone
    /// cannot prove the chunk list is the file's.
    #[tokio::test]
    async fn the_bounded_verify_walks_every_window_and_closes_on_the_file_address() {
        let key = owner_key();
        let root = key.convergent_chunk_root();
        let parts: Vec<Vec<u8>> = (0u8..6).map(|i| vec![i; 100 + i as usize]).collect();
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let (fetcher, manifest_hash, expected) = seal_file(&root, &refs);
        let got = verify_file_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key.clone()),
            manifest_hash,
            None,
            "v/six.bin",
        )
        .await
        .expect("a six-chunk file verifies across two windows");
        assert_eq!(got, ContentHash::of_raw(&expected));

        // Same chunks, a manifest naming the wrong whole-file content.
        let manifest: ChunkManifest =
            crate::encoding::canonical_decode(&fetcher.get(&manifest_hash).unwrap()).unwrap();
        let mut manifest = manifest.unseal_hashes(&root).unwrap();
        manifest.file_hash = ContentHash::of_raw(b"some other file");
        let lying =
            crate::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
        let lying_hash = ContentHash::of_raw(&lying);
        let mut fetcher = fetcher;
        fetcher.put(&lying_hash, lying);
        verify_file_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            lying_hash,
            None,
            "v/six.bin",
        )
        .await
        .expect_err("the whole-file address must match, not only each chunk");
    }

    /// A manifest whose chunks all verify but whose size table is permuted
    /// (same total) is refused by the windowed walk — the table is what a
    /// re-seal carries forward and what a range read of the result seeks by.
    #[tokio::test]
    async fn a_permuted_size_table_is_refused_by_the_windowed_walk() {
        let key = owner_key();
        let (fetcher, manifest_hash, _) =
            seal_file(&key.convergent_chunk_root(), &[&[1u8; 100], &[2u8; 300]]);
        let mut manifest: ChunkManifest =
            crate::encoding::canonical_decode(&fetcher.get(&manifest_hash).unwrap()).unwrap();
        manifest.chunk_sizes.swap(0, 1);
        let permuted = crate::encoding::canonical_encode(&manifest).unwrap();
        let permuted_hash = ContentHash::of_raw(&permuted);
        let mut fetcher = fetcher;
        fetcher.put(&permuted_hash, permuted);
        let err = verify_file_by_manifest(
            &fetcher,
            &FileDownloadKeys::owner(key),
            permuted_hash,
            None,
            "v/permuted.bin",
        )
        .await
        .expect_err("a size table that does not match the chunks is refused");
        assert!(format!("{err:#}").contains("size table"), "{err:#}");
    }

    // ── Part (D): an unstamped record of a bound set, selected by its stamp ──
    //
    // `mls-group-key-material.md` § M2 → *Pre-bind re-seal migration* (D): a
    // bound reader selects the root by the record's stamp — stamped → that
    // generation and never the owner root; unstamped → the owner root, for the
    // set's owner only. The seal side does not move.

    fn bound_owner_keys(version: u64, key: [u8; 32]) -> FileDownloadKeys {
        FileDownloadKeys {
            owner_signed_record: true,
            ..bound_keys(version, key)
        }
    }

    /// The positive: the set's owner opens a record it sealed under its own
    /// root before the bind (no stamp) on every byte walk — whole buffer,
    /// bounded-to-path and range — while holding the bound set's content keys.
    #[tokio::test]
    async fn the_owner_opens_an_unstamped_prebind_record_under_its_own_root() {
        let big = vec![b'p'; 8192];
        let (fetcher, manifest_hash, expected) = seal_file(
            &owner_key().convergent_chunk_root(),
            &[b"sealed before the bind ", big.as_slice()],
        );
        let keys = bound_owner_keys(1, [0x61u8; 32]);

        let got =
            download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "pre/bind.bin")
                .await
                .expect("the owner opens its own pre-bind record");
        assert_eq!(got, expected);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bind.bin");
        download_file_to_path_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            None,
            "pre/bind.bin",
            &dest,
        )
        .await
        .expect("the bounded walk opens it too");
        assert_eq!(std::fs::read(&dest).unwrap(), expected);

        let range = download_file_range_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            None,
            "pre/bind.bin",
            7,
            6,
        )
        .await
        .expect("and the range walk");
        assert_eq!(range, expected[7..13]);
    }

    /// A bound-keyless owner (custody not yet loaded — a capability host
    /// before its keys arrive) still opens pre-bind content: the stamp's
    /// absence, not the content keys, selects the owner root.
    #[tokio::test]
    async fn a_keyless_bound_owner_opens_an_unstamped_record_too() {
        let (fetcher, manifest_hash, expected) =
            seal_file(&owner_key().convergent_chunk_root(), &[b"pre-bind"]);
        let keys = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: Some(vec![9u8; 32]),
            owner_signed_record: true,
            ..Default::default()
        };
        let got =
            download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "pre/k.bin")
                .await
                .expect("the stamp selects the owner root whatever the custody state");
        assert_eq!(got, expected);
    }

    /// A successor owner's pre-bind corpus may still rest under a retired
    /// predecessor root; the unstamped arm offers those too, owner-only.
    #[tokio::test]
    async fn the_owner_opens_an_unstamped_record_under_a_predecessor_root() {
        let predecessor = crate::crypto::BackupKey::from_bytes([0x44u8; 32]);
        let (fetcher, manifest_hash, expected) =
            seal_file(&predecessor.convergent_chunk_root(), &[b"older identity"]);
        let keys = FileDownloadKeys {
            predecessor_backup_keys: vec![predecessor.into()],
            ..bound_owner_keys(1, [0x61u8; 32])
        };
        let got =
            download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "pre/p.bin")
                .await
                .expect("a predecessor-sealed pre-bind record opens for the owner");
        assert_eq!(got, expected);
    }

    /// Negative 1: a STAMPED record never opens under the owner root — not even
    /// for the owner, and not even when the bytes really are owner-sealed. The
    /// content-keyed precedence is unchanged; only the stamp's absence reaches
    /// the owner root.
    #[tokio::test]
    async fn a_stamped_record_never_opens_under_the_owner_root() {
        let (fetcher, manifest_hash, _) = seal_file(
            &owner_key().convergent_chunk_root(),
            &[b"owner-keyed bytes"],
        );
        let keys = bound_owner_keys(1, [0x61u8; 32]);
        download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, Some(1), "s/f.bin")
            .await
            .expect_err("a stamped record opens under its generation alone");
        let dir = tempfile::tempdir().unwrap();
        download_file_to_path_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            Some(1),
            "s/f.bin",
            &dir.path().join("f.bin"),
        )
        .await
        .expect_err("on the bounded walk too");
    }

    /// Negative 2: a member's own `BackupKey` is never offered for an unstamped
    /// record — even when that key is exactly what sealed the bytes. A member
    /// sealed nothing in this set under its own root, so a record that opens
    /// that way is not one the set's history can hold.
    #[tokio::test]
    async fn a_members_own_backup_key_is_never_offered_for_an_unstamped_record() {
        let member = crate::crypto::BackupKey::from_bytes([0x71u8; 32]);
        let (fetcher, manifest_hash, _) =
            seal_file(&member.convergent_chunk_root(), &[b"member-keyed bytes"]);
        let keys = FileDownloadKeys {
            backup_key: Some(member.into()),
            owner_signed_record: false,
            ..bound_keys(1, [0x61u8; 32])
        };
        let err = download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "m/f.bin")
            .await
            .expect_err("a member holder never offers its own key for an unstamped record");
        assert!(
            format!("{err:#}").contains("did not verify as this holder's own"),
            "refused by the owner-signed-record gate, not by an AEAD miss: {err:#}"
        );
    }

    /// Negative 3: nothing seals under the owner root in a bound set — being the
    /// owner widens the read of unstamped records and nothing else.
    #[test]
    fn a_bound_owner_still_seals_under_the_current_generation() {
        let root = bound_owner_keys(4, [5u8; 32])
            .label_seal_root()
            .unwrap()
            .expect("a bound owner seals");
        assert_eq!(root.generation(), Some(4));
        let keyless_owner = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: Some(vec![9u8; 32]),
            owner_signed_record: true,
            ..Default::default()
        };
        assert!(
            keyless_owner.label_seal_root().is_err(),
            "a bound-keyless owner refuses to seal rather than fall back to its own root"
        );
    }

    #[test]
    fn a_bound_set_with_no_content_keys_fails_closed() {
        // The arm that matters: a removed member or a startup race must NOT
        // silently fall through to the owner root (which no other roster member
        // could open) or to plaintext. `content_seal_root` fails closed here and
        // so must this mirror — note `backup_key` IS present, so a naive
        // implementation would happily seal under it.
        let keys = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: Some(vec![9u8; 32]),
            content_keys: None,
            ..Default::default()
        };
        assert!(
            keys.label_seal_root().is_err(),
            "a bound set with no content keys must refuse to seal, not downgrade"
        );
    }

    /// A served-but-unshared set's reader holds BOTH its owner key and the set's
    /// M2 content keys (custody at the serve pseudo-channel) with no MLS group.
    /// Its chunks are content-keyed by design (`webdav-server.md` § Key model),
    /// so the owner key must not shadow them: the read opens under the stamped
    /// generation, and a new label seals under the content root — the same
    /// answer the engine's chunk seal gives (FS-5DC generalised, 2026-09-09;
    /// before it this reader's owner roots came first and the content-sealed
    /// bytes a DAV client wrote were unreadable on every app).
    #[tokio::test]
    async fn a_served_reader_holding_its_owner_key_reads_and_seals_under_the_content_key() {
        let content = crate::folder_keys::FolderContentKeys::genesis([0x5e; 32], 1_000);
        let (fetcher, manifest_hash, file_data) =
            seal_file(content.current_key(), &[b"bytes a DAV client wrote"]);
        let keys = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: None,
            content_keys: Some(content.clone()),
            ..Default::default()
        };
        let got = download_file_bytes_by_manifest(
            &fetcher,
            &keys,
            manifest_hash,
            Some(content.current_version()),
            "docs/from-dav.txt",
        )
        .await
        .expect("a served reader opens the set's content-keyed chunks");
        assert_eq!(got, file_data);
        let root = keys
            .label_seal_root()
            .unwrap()
            .expect("a served reader seals its labels");
        assert_eq!(
            root.generation(),
            Some(content.current_version()),
            "labels seal under the content root — the one that seals the set's chunks"
        );
    }

    #[test]
    fn an_unbound_set_seals_under_the_owner_root_with_no_generation() {
        let root = FileDownloadKeys::owner(owner_key())
            .label_seal_root()
            .unwrap()
            .expect("an owner-only reader can seal");
        assert_eq!(
            root.generation(),
            None,
            "the owner root does not rotate, so there is no generation to stamp"
        );
    }

    #[test]
    fn a_keyless_reader_seals_nothing_and_that_is_not_an_error() {
        // The ratified degrade: the caller records plaintext-only and leaves an S8
        // backfill row. Distinct from the fail-closed `Err` above — that one is a
        // reader who *should* have keys, this one never had any.
        assert!(
            FileDownloadKeys::default()
                .label_seal_root()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn what_a_reader_seals_it_can_open_again_on_the_same_custody() {
        // The property both selections exist to guarantee, asserted on ONE keys
        // object rather than by eye across two functions: a label sealed under
        // `label_seal_root` must open under the candidates `label_open_roots`
        // offers for the stamp that seal carries. A drift between the two is
        // **silent** — a wrong root degrades to `SealedLabelRender::Omit`, never
        // to an error — which is exactly why this is pinned by round-trip rather
        // than by comparing two roots for equality.
        use crate::path_crypto::{LabelField, SealedLabelRender, render_sealed_label, seal_random};

        for (keys, label) in [
            (FileDownloadKeys::owner(owner_key()), "unbound owner"),
            (bound_keys(4, [5u8; 32]), "bound set, generation 4"),
        ] {
            let root = keys.label_seal_root().unwrap().unwrap();
            let salt = [7u8; 32];
            let sealed = seal_random(&root, &salt, LabelField::SnapshotTags, b"[\"manual\"]")
                .unwrap()
                .to_bytes()
                .unwrap();
            assert!(
                matches!(
                    render_sealed_label(&keys, Some(&sealed), None, &salt, LabelField::SnapshotTags),
                    SealedLabelRender::Sealed(ref s) if s == "[\"manual\"]"
                ),
                "{label}: the seal selection and the open selection must agree"
            );
        }
    }

    #[test]
    fn an_owner_reader_opens_a_served_era_name_under_its_retired_generation() {
        // The name half of `an_owner_reader_opens_a_served_era_content_key_sealed_file`:
        // after serve-OFF the owner's engine holds the served generation only as a
        // RETIRED candidate. A label the served window sealed carries `gen: Some(v)`;
        // offering only the live `content_keys` (now `None`) failed closed, so the
        // file's bytes opened and its name never did.
        use crate::path_crypto::{LabelField, SealedLabelRender, render_sealed_label, seal_random};

        let served = bound_keys(4, [5u8; 32]);
        let root = served.label_seal_root().unwrap().unwrap();
        let salt = [9u8; 32];
        let sealed = seal_random(
            &root,
            &salt,
            LabelField::SyncChangePath,
            b"docs/from-the-drive.txt",
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        let after_unserve = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            retired_content_keys: served.content_keys.clone(),
            ..Default::default()
        };
        assert!(after_unserve.content_keys.is_none());
        assert!(
            matches!(
                render_sealed_label(&after_unserve, Some(&sealed), None, &salt, LabelField::SyncChangePath),
                SealedLabelRender::Sealed(ref s) if s == "docs/from-the-drive.txt"
            ),
            "a served-era name must open under the retired generation"
        );

        // Still fail-closed for a reader holding neither.
        assert!(
            FileDownloadKeys::owner(owner_key())
                .label_open_roots(Some(4))
                .is_err()
        );
    }
}
