//! Sealing user-chosen file/folder/set **labels** — paths, names, device
//! labels, tags — so a hosting nest cannot read them at rest.
//!
//! Implements the *paths-are-content* ruling: a file name reveals as much as
//! its bytes (`eviction_notice.pdf`), and a hosting nest's admin is not
//! necessarily the data owner (the family-nest case). Mechanism owner:
//! `docs/goal/behavior/file-sync.md` § Sealed names & paths; the ruling +
//! tightening set + the two exemptions (`path_hash` stays floor, web-type
//! paths are public by design) live in
//! `docs/goal/architecture/encryption-at-rest.md` § Carve-outs; the key entry
//! is `docs/goal/architecture/mls-group-key-material.md` § M2 → *Sealed names
//! & paths*.
//!
//! **No new key category** (key-hierarchy rule #1). A label seals under the
//! *same root that already seals the set's chunks* — a bound/served set's M2
//! content-key generation, or `BackupKey::convergent_chunk_root()` on the
//! owner path — so whoever can open the set's bytes renders its names, and
//! nobody weaker can. The derivation is the same two-step BLAKE3 shape as
//! [`crate::chunk_crypto`] / [`crate::manifest_crypto`], domain-separated by
//! the context string `"fauna.path.v1"`.
//!
//! **Two nonce modes**, and picking the wrong one is a real break:
//!
//! - [`seal_convergent`] derives the nonce from the salt. Legal **only** where
//!   the salt determines the plaintext — `path` salted by its `path_hash`, a
//!   set `name` by its [`set_name_hash`], an import `source_descriptor` by its
//!   `source_hash`, a per-tag seal by that tag's hash. There, re-sealing the
//!   same label reproduces a byte-identical blob (idempotent retry), and two
//!   different plaintexts can never share a salt.
//! - [`seal_random`] takes a fresh random nonce, and is mandatory for every
//!   field that is *mutable under a fixed salt*: a device label, a set's
//!   include/exclude lists, a conflict's free-text `details`, the tag-list
//!   display copy. A derived nonce there would reuse a (key, nonce) pair
//!   across differing plaintexts — the classic AEAD catastrophe.
//!
//! The envelope is self-describing ([`SealedLabel`]), so a **server-side row
//! copy moves a sealed label verbatim with no key** — snapshot creation copies
//! the membership projection, carrying `gen` along, which is why there is no
//! per-table generation column.

use crate::crypto::BackupKey;
use crate::encoding::{canonical_decode, canonical_encode};
use anyhow::{Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// The only envelope version this module writes.
pub const SEALED_LABEL_V1: u8 = 1;

/// The BLAKE3 `derive_key` context — domain-separates label seals from
/// `"fauna.chunk.v1"` (chunk bodies) and `"fauna.manifest.v1"` (manifest
/// hashes) under the *same* root secret.
const LABEL_CONTEXT: &str = "fauna.path.v1";

/// The field a sealed label belongs to — bound into both the key derivation
/// and the AEAD associated data, so a blob sealed for one column can never
/// open as another (splicing a `share_tokens.filename` into a
/// `sync_devices.label` fails the tag check, fail-closed).
///
/// The variant set is the ratified tightening inventory
/// (`docs/goal/architecture/encryption-at-rest.md` § Carve-outs → *Seal
/// file-sync name/path metadata*). The wire tags are frozen: changing one
/// orphans every blob already sealed under it, so it is a data migration, not
/// a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelField {
    /// **The tag for the ENTIRE path plane — every folder-relative path in
    /// every table seals under this one, not under a per-table variant.**
    ///
    /// Named for `sync_changes.path` only because that is where paths are
    /// first recorded; it is equally the tag for `snapshot_files.path`,
    /// `backup_custody.path`, `sync_conflicts.path` and every other path
    /// carrier. Reach for this one whenever you seal or open a path.
    ///
    /// The reason is structural, not stylistic: the nest **copies `path_sealed`
    /// verbatim between tables** and holds no key to re-seal with
    /// (`sync_changes` → `snapshot_files`, `db/sync_storage.rs`'s
    /// `INSERT INTO snapshot_files … f.path_sealed`), which is exactly what the
    /// self-describing `gen` field exists to make safe
    /// (`file-sync.md` § Sealed names & paths). Because the tag is mixed into
    /// both the key derivation ([`composed_salt`]) and the AAD
    /// ([`label_aad`]), a per-table tag would make every copied blob fail to
    /// open in its destination table.
    SyncChangePath,
    /// `sync_conflicts.details` — free-text conflict description (mutable).
    ConflictDetails,
    /// `folders.name` — the user-chosen set name. Reserved `__` names are
    /// routing constants and never seal.
    FolderName,
    /// `folders.include_paths` — the owner's absolute local layout (mutable).
    FolderIncludePaths,
    /// `folders.exclude_paths` — likewise (mutable).
    FolderExcludePaths,
    /// `sync_devices.label` — the user-chosen device name (mutable).
    DeviceLabel,
    /// `share_tokens.filename` — the shared file's name, for the author's list.
    ShareFilename,
    /// `import_sessions.source_descriptor` — the import source identity.
    ImportSource,
    /// `snapshots.tags` — the whole tag list, as the display copy (mutable).
    /// Sealed through [`crate::label_custody::seal_snapshot_tags`] (S6-d).
    SnapshotTags,
    /// `folders.retention_policy` — the per-set retention JSON (mutable).
    /// Sealed through [`crate::label_custody::seal_retention_policy`] (S6-e).
    ///
    /// Its own tag rather than a shared one: the single-tag rule S6-a
    /// established binds the **path** plane only (the nest copies `path_sealed`
    /// verbatim between tables and holds no key to re-seal), and no nest code
    /// ever copies this column anywhere.
    FolderRetentionPolicy,
}

impl LabelField {
    /// The frozen wire tag mixed into the key derivation and the AAD.
    pub fn tag(self) -> &'static str {
        match self {
            Self::SyncChangePath => "sync_changes.path",
            Self::ConflictDetails => "sync_conflicts.details",
            Self::FolderName => "folders.name",
            Self::FolderIncludePaths => "folders.include_paths",
            Self::FolderExcludePaths => "folders.exclude_paths",
            Self::DeviceLabel => "sync_devices.label",
            Self::ShareFilename => "share_tokens.filename",
            Self::ImportSource => "import_sessions.source_descriptor",
            Self::SnapshotTags => "snapshots.tags",
            Self::FolderRetentionPolicy => "folders.retention_policy",
        }
    }
}

/// A root secret paired with the M2 content-key generation it *is*, so a
/// caller cannot seal under generation 3's key while stamping `gen: None`.
///
/// `gen = None` means the owner root (`BackupKey::convergent_chunk_root()`),
/// which does not rotate; `Some(v)` names the `FolderContentKeys` version, so
/// a reader knows to trial `keys_for(v)` — plural, because a concurrent
/// rotation merge can leave several candidate keys at one version.
///
/// **Not `Copy`** (`key-material-hierarchy.md` § Plaintext key lifetime on bridges
/// → *Carrier shape*, second bullet). It was, until the 2026-08-12 sweep — the
/// only `Copy` holder of user-content key material that survey found. A `Copy`
/// carrier duplicates itself on every assignment and pass-by-value and no
/// duplicate is ever zeroized, which is the shape the rule exists to forbid;
/// dropping the derive cost **nothing** (zero call sites across `fauna-core`,
/// `fauna-sync-engine`, `fauna-client-folders`, `fauna-ffi` and
/// `fauna-sync-agent` — every one already passed this by reference), which is
/// precisely why it was never noticed.
///
/// The `ZeroizeOnDrop` half is what makes that permanent rather than a
/// convention: its destructor makes `Copy` **uncompilable** (`E0184`), so this
/// type needs no separate compile-time pin — the same reasoning the rule gives
/// for `MailKeys`, and the reason both halves of the bullet are taken together
/// rather than only the one that showed up in a grep.
#[derive(Debug, Clone, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct LabelRoot {
    secret: [u8; 32],
    generation: Option<u64>,
}

impl LabelRoot {
    /// The owner path: `BackupKey::convergent_chunk_root()`, no generation.
    pub fn owner(secret: [u8; 32]) -> Self {
        Self {
            secret,
            generation: None,
        }
    }

    /// [`Self::owner`] from an already-derived [`BackupKey`] — the "registering
    /// owner's own root, never a folder's M2 generation" construction every
    /// device-label-seal call site reaches for (five of them, independently,
    /// before this: `fauna-ffi`'s two exports, `fauna-client-sync`'s renewal
    /// grant, `fauna-sync-engine`'s startup register, and the since-removed
    /// headless daemon's register).
    pub fn owner_of(key: &BackupKey) -> Self {
        Self::owner(key.convergent_chunk_root())
    }

    /// A bound/served set's M2 content-key generation.
    pub fn content_key(secret: [u8; 32], version: u64) -> Self {
        Self {
            secret,
            generation: Some(version),
        }
    }

    /// The generation this root *is* — what a seal stamps into its envelope's
    /// `gen`, and therefore what a reader must pass to
    /// [`crate::file_download::FileDownloadKeys::label_open_roots`] to get the
    /// matching candidates back.
    ///
    /// Read-only, and deliberately the *only* accessor: the secret stays private
    /// so a caller cannot lift it out and seal under it while stamping a
    /// different generation — the pairing this type exists to enforce.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }
}

/// The sealed-label envelope, canonical dag-cbor, as stored in every
/// `*_sealed` column (`docs/goal/behavior/file-sync.md` § Sealed names &
/// paths → *The carrier*).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedLabel {
    /// Envelope version — [`SEALED_LABEL_V1`].
    pub v: u8,
    /// The M2 content-key generation this sealed under; `None` = the owner
    /// root. Carried so a keyless server-side row copy stays openable.
    ///
    /// Encodes as `gen` — the name the goal doc pins for the envelope field —
    /// but `gen` is a reserved keyword in Rust 2024, so the Rust field spells
    /// it out.
    #[serde(rename = "gen")]
    pub generation: Option<u64>,
    /// `None` = convergent mode (nonce derived from the salt); `Some` = an
    /// explicit random nonce, for fields mutable under a fixed salt.
    #[serde(with = "serde_bytes")]
    pub nonce: Option<[u8; 12]>,
    /// The AEAD ciphertext.
    pub ct: ByteBuf,
}

impl SealedLabel {
    /// Canonical dag-cbor bytes, for the `BLOB` column.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        canonical_encode(self).map_err(Into::into)
    }

    /// Parse a `BLOB` column back into an envelope.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        canonical_decode(bytes).map_err(Into::into)
    }
}

/// The composed salt: the caller's 32-byte salt, then a length-prefixed field
/// tag. Both the key and the convergent nonce derive from it, so a fixed root
/// gives every (salt, field) pair its own key *and* nonce.
fn composed_salt(salt: &[u8; 32], field: LabelField) -> blake3::Hash {
    let tag = field.tag().as_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(salt);
    hasher.update(&(tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.finalize()
}

/// Two-step derivation, same shape as `chunk_crypto::derive_chunk_key` and
/// `manifest_crypto::derive_manifest_key`: domain-separate the root by
/// context, then salt it.
fn derive_label_key(root_secret: &[u8; 32], salt: &blake3::Hash) -> [u8; 32] {
    crate::domain_key::derive_domain_key(LABEL_CONTEXT, root_secret, salt.as_bytes())
}

fn derive_label_nonce(salt: &blake3::Hash) -> Nonce {
    crate::nonce_truncate::nonce_from_digest(salt.as_bytes())
}

/// The associated data: the envelope version, the caller's salt, and the field
/// tag. Binding the salt is what stops a blob sealed for path A from opening
/// as path B even when both derivations are reachable.
fn label_aad(salt: &[u8; 32], field: LabelField) -> Vec<u8> {
    let tag = field.tag().as_bytes();
    let mut aad = Vec::with_capacity(1 + 32 + 8 + tag.len());
    aad.push(SEALED_LABEL_V1);
    aad.extend_from_slice(salt);
    aad.extend_from_slice(&(tag.len() as u64).to_le_bytes());
    aad.extend_from_slice(tag);
    aad
}

/// Seal a label whose **salt determines its plaintext** — `path` under its
/// `path_hash`, a set `name` under its [`set_name_hash`], and the rest of the
/// convergent set named in the module docs.
///
/// Re-sealing identical input yields a byte-identical envelope, so a retry is
/// idempotent and a re-record does not churn the column.
///
/// # Errors
/// Propagates an AEAD failure (unreachable for the shapes we seal, but never
/// swallowed).
pub fn seal_convergent(
    root: &LabelRoot,
    salt: &[u8; 32],
    field: LabelField,
    plaintext: &[u8],
) -> Result<SealedLabel> {
    let composed = composed_salt(salt, field);
    let key = derive_label_key(&root.secret, &composed);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    let ct = cipher
        .encrypt(
            &derive_label_nonce(&composed),
            Payload {
                msg: plaintext,
                aad: &label_aad(salt, field),
            },
        )
        .map_err(|e| anyhow::anyhow!("label sealing failed: {e}"))?;
    Ok(SealedLabel {
        v: SEALED_LABEL_V1,
        generation: root.generation,
        nonce: None,
        ct: ByteBuf::from(ct),
    })
}

/// Seal a label that is **mutable under a fixed salt** — a device label, a
/// set's include/exclude lists, a conflict's `details`, the tag-list display
/// copy — with a fresh random nonce. Using [`seal_convergent`] for one of
/// these would reuse a (key, nonce) pair across differing plaintexts.
///
/// # Errors
/// Propagates an AEAD failure.
pub fn seal_random(
    root: &LabelRoot,
    salt: &[u8; 32],
    field: LabelField,
    plaintext: &[u8],
) -> Result<SealedLabel> {
    let composed = composed_salt(salt, field);
    let key = derive_label_key(&root.secret, &composed);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &label_aad(salt, field),
            },
        )
        .map_err(|e| anyhow::anyhow!("label sealing failed: {e}"))?;
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(nonce.as_slice());
    Ok(SealedLabel {
        v: SEALED_LABEL_V1,
        generation: root.generation,
        nonce: Some(nonce_bytes),
        ct: ByteBuf::from(ct),
    })
}

/// Open a sealed label, trialling every candidate root secret — the
/// `FolderContentKeys::keys_for(version)` posture, since a concurrent
/// rotation merge can leave several keys at one generation. The AEAD tag
/// disambiguates; a label that opens under none of them fails closed.
///
/// The caller picks the candidates from [`SealedLabel::generation`]: `None` → the
/// owner root, `Some(v)` → `keys_for(v)`.
///
/// # Errors
/// Unknown envelope version, a malformed nonce, or no candidate opening it.
pub fn open<'a>(
    root_candidates: impl IntoIterator<Item = &'a [u8; 32]>,
    salt: &[u8; 32],
    field: LabelField,
    sealed: &SealedLabel,
) -> Result<Vec<u8>> {
    if sealed.v != SEALED_LABEL_V1 {
        bail!(
            "unknown sealed-label envelope version {} (this build writes and reads v{})",
            sealed.v,
            SEALED_LABEL_V1
        );
    }
    let composed = composed_salt(salt, field);
    let nonce = match sealed.nonce {
        Some(bytes) => Nonce::from(bytes),
        None => derive_label_nonce(&composed),
    };
    let aad = label_aad(salt, field);
    let mut tried = 0usize;
    for root_secret in root_candidates {
        tried += 1;
        let key = derive_label_key(root_secret, &composed);
        let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
        if let Ok(plaintext) = cipher.decrypt(
            &nonce,
            Payload {
                msg: sealed.ct.as_ref(),
                aad: &aad,
            },
        ) {
            return Ok(plaintext);
        }
    }
    bail!(
        "sealed label for `{}` opened under none of the {tried} candidate root key(s)",
        field.tag()
    )
}

/// The equality-only addressing hash for a user-chosen **folder name** — the
/// `path_hash` class, joining the plaintext floor
/// (`docs/goal/architecture/mls-group-key-material.md` § M2 → *Sealed names &
/// paths*). Addresses a set once its `name` rests sealed, and backs
/// `UNIQUE(name_hash, actor_id)`.
///
/// Pinned by a known-answer test below: changing this digest orphans every
/// stored `name_hash`, so it is a data migration, not a refactor. The
/// counterpart for paths is [`crate::sync::path_hash`].
pub fn set_name_hash(name: &str) -> [u8; 32] {
    blake3::derive_key("fauna.set-name.v1", name.as_bytes())
}

/// The equality-only hash of an import **source descriptor** — the companion
/// that carries `import_sessions`' multi-device per-source mutual-exclusion
/// lock once `source_descriptor` rests sealed.
///
/// Same class and same pinning rules as [`set_name_hash`], with its own
/// derivation context so the two hash spaces never overlap.
pub fn import_source_hash(source_descriptor: &str) -> [u8; 32] {
    blake3::derive_key("fauna.import-source.v1", source_descriptor.as_bytes())
}

/// The equality-only hash of one **snapshot tag** — what the background
/// retention pruner matches a policy's `keep_tags` against once `snapshots.tags`
/// rests sealed. The policy's own tags hash through here too, so the compare
/// stays a hash-to-hash equality on both sides.
///
/// Same class and same pinning rules as [`set_name_hash`], with its own
/// derivation context.
pub fn snapshot_tag_hash(tag: &str) -> [u8; 32] {
    blake3::derive_key("fauna.snapshot-tag.v1", tag.as_bytes())
}

// ── The sealed-first render policy ─────────────────────────────────────────────
//
// [`seal_convergent`] / [`seal_random`] / [`open`] above are the *primitive*:
// bytes in, bytes out, caller-chosen roots. Everything below is the **policy**
// every read surface must share — which root to try, in what order to prefer
// the seal over the resting plaintext, and what a reader that can open neither
// is required to do. It lives here rather than in a module of its own because
// `label` and `render` are both already taken in this crate with unrelated
// meanings (moderation labels; the semantic render model).

/// What a read surface should show for one user-chosen label
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths → *Migration*).
///
/// The three variants are the whole contract; there is deliberately no
/// "empty string" and no "error" case, because both were the observed failure
/// modes the ratified degrade rule exists to forbid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealedLabelRender {
    /// Rendered from the sealed sibling — the preferred path, and the only one
    /// that still works after the plaintext column is scrubbed at the flip.
    Sealed(String),
    /// Rendered from the plaintext column: this row carries no seal this
    /// reader can open — a ratified plaintext class (a `public`-audience
    /// folder's paths, a machine-authored label, a plane whose S9 gate still
    /// rests plaintext), a row no keyed writer has stamped yet (what the S8
    /// backfill pass converts), or a seal whose salt the wire withheld.
    Plaintext(String),
    /// This reader cannot render the label at all — a sealed-only row met an
    /// old or keyless reader. The ratified degrade is **omit the row from the
    /// listing, and let it re-enter on re-record**: never an empty name, never
    /// a hard error that takes the whole page down (the `backup_custody`
    /// path-less precedent).
    Omit,
}

impl SealedLabelRender {
    /// The rendered text, or `None` for [`Self::Omit`] — for the common caller
    /// that treats "sealed" and "plaintext" identically and only branches on
    /// whether it got a label at all.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Sealed(s) | Self::Plaintext(s) => Some(s),
            Self::Omit => None,
        }
    }

    /// The plaintext half of the degrade, on its own: [`Self::Plaintext`] when
    /// the column rests a label, [`Self::Omit`] when it is absent or scrubbed.
    ///
    /// An empty plaintext is the scrubbed/absent column, never a real label —
    /// a user-chosen path or name is non-empty by construction. This is the
    /// tail of [`render_sealed_label`], and the whole render for a row whose
    /// seal cannot even be attempted (no salt on the wire —
    /// `label_custody::render_path`).
    pub fn from_plaintext(plaintext: Option<&str>) -> Self {
        match plaintext {
            Some(p) if !p.is_empty() => Self::Plaintext(p.to_string()),
            _ => Self::Omit,
        }
    }
}

/// Render one user-chosen label **sealed-first**, falling back to the resting
/// plaintext sibling, and degrading to [`SealedLabelRender::Omit`] when the
/// reader can open neither.
///
/// This is the one seam every read surface shares (media list, sync files,
/// snapshot browse/diff, conflicts), so the preference order and the degrade
/// cannot drift per surface — the same reason
/// `SyncEngine::seal_recorded_path` is the single write funnel.
///
/// `keys` is the reader's ordinary byte-download custody: by the ruling's own
/// logic the roots that open a set's chunks open its names, so there is no
/// second resolver to build or keep in sync
/// ([`crate::file_download::FileDownloadKeys::label_open_roots`]).
///
/// `salt` must be the same salt the writer sealed under — for a path, its
/// `path_hash` ([`crate::sync::path_hash`]); for a set name,
/// [`set_name_hash`]. Passing the wrong salt fails the AEAD tag and degrades,
/// exactly as a wrong key does; it cannot silently render the wrong label.
///
/// **A seal that will not open is not an error to the caller.** It is a
/// legitimate, expected state for any reader outside the set's audience, and
/// treating it as an error would turn "one row I can't read" into "the page
/// failed to load". Failures are traced at `debug` **without any label
/// content** — the log scrub (S7) must not have new work created for it here.
pub fn render_sealed_label(
    keys: &crate::file_download::FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&str>,
    salt: &[u8; 32],
    field: LabelField,
) -> SealedLabelRender {
    if let Some(bytes) = sealed
        && let Some(text) = try_open_label(keys, bytes, salt, field)
    {
        return SealedLabelRender::Sealed(text);
    }
    SealedLabelRender::from_plaintext(plaintext)
}

/// The seal half of [`render_sealed_label`]: decode the envelope, pick the
/// reader's candidate roots from the envelope's own `gen`, open, and require
/// the plaintext to be UTF-8. Any failure is `None` — the caller degrades.
fn try_open_label(
    keys: &crate::file_download::FileDownloadKeys,
    sealed: &[u8],
    salt: &[u8; 32],
    field: LabelField,
) -> Option<String> {
    let envelope = match SealedLabel::from_bytes(sealed) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                target: "fauna_core::path_crypto",
                field = field.tag(),
                "sealed label envelope did not decode: {e:#}"
            );
            return None;
        }
    };
    let roots = match keys.label_open_roots(envelope.generation) {
        Ok(roots) if !roots.is_empty() => roots,
        // Both arms are the same outcome for a renderer: this reader holds no
        // root that could open this envelope. `Err` additionally means the
        // fail-closed posture fired (bound set, generation not held).
        Ok(_) => {
            tracing::debug!(
                target: "fauna_core::path_crypto",
                field = field.tag(),
                "reader holds no candidate root for this sealed label"
            );
            return None;
        }
        Err(e) => {
            tracing::debug!(
                target: "fauna_core::path_crypto",
                field = field.tag(),
                "no open root for this sealed label: {e:#}"
            );
            return None;
        }
    };
    let plaintext = match open(roots.iter(), salt, field, &envelope) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(
                target: "fauna_core::path_crypto",
                field = field.tag(),
                "sealed label did not open: {e:#}"
            );
            return None;
        }
    };
    match String::from_utf8(plaintext) {
        Ok(s) => Some(s),
        Err(_) => {
            tracing::debug!(
                target: "fauna_core::path_crypto",
                field = field.tag(),
                "sealed label opened to non-UTF-8 bytes"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "docs/eviction_notice.pdf";

    fn path_salt() -> [u8; 32] {
        crate::sync::path_hash(PATH)
    }

    /// Pins the name-hash derivation the same way
    /// `sync::path_hash_is_blake3_of_the_normalized_path_bytes` pins its
    /// sibling — a change here breaks every stored `name_hash`.
    #[test]
    fn set_name_hash_is_the_pinned_derivation() {
        assert_eq!(
            hex::encode(set_name_hash("Family photos")),
            hex::encode(blake3::derive_key("fauna.set-name.v1", b"Family photos"))
        );
        assert_eq!(
            hex::encode(set_name_hash("Family photos")),
            "5245f3343a81010fb7a914a290461030408fd8e90b45eb6799353860718a8331"
        );
    }

    #[test]
    fn set_name_hash_separates_distinct_names() {
        assert_ne!(set_name_hash("Photos"), set_name_hash("photos"));
        assert_ne!(set_name_hash("Photos"), set_name_hash(""));
    }

    /// Pins the other two floor companions, and proves the three hash spaces
    /// are disjoint: the same string never collides across them, so a
    /// `name_hash` can never be mistaken for a tag or source hash.
    #[test]
    fn the_three_hash_companions_are_pinned_and_disjoint() {
        assert_eq!(
            hex::encode(import_source_hash("imap://mail.example.com/INBOX")),
            hex::encode(blake3::derive_key(
                "fauna.import-source.v1",
                b"imap://mail.example.com/INBOX"
            ))
        );
        assert_eq!(
            hex::encode(snapshot_tag_hash("preserve")),
            hex::encode(blake3::derive_key("fauna.snapshot-tag.v1", b"preserve"))
        );
        let same = "preserve";
        let hashes = [
            set_name_hash(same),
            import_source_hash(same),
            snapshot_tag_hash(same),
            crate::sync::path_hash(same),
        ];
        let unique: std::collections::HashSet<[u8; 32]> = hashes.iter().copied().collect();
        assert_eq!(unique.len(), hashes.len());
    }

    #[test]
    fn convergent_round_trip() {
        let root = LabelRoot::owner([7u8; 32]);
        let salt = path_salt();
        let sealed =
            seal_convergent(&root, &salt, LabelField::SyncChangePath, PATH.as_bytes()).unwrap();
        assert_eq!(sealed.v, SEALED_LABEL_V1);
        assert_eq!(sealed.generation, None);
        assert_eq!(sealed.nonce, None);
        let opened = open([&[7u8; 32]], &salt, LabelField::SyncChangePath, &sealed).unwrap();
        assert_eq!(opened, PATH.as_bytes());
    }

    #[test]
    fn random_round_trip_and_distinct_blobs() {
        let root = LabelRoot::content_key([9u8; 32], 4);
        let salt = [3u8; 32];
        let a = seal_random(&root, &salt, LabelField::DeviceLabel, b"Ada's laptop").unwrap();
        let b = seal_random(&root, &salt, LabelField::DeviceLabel, b"Ada's laptop").unwrap();
        assert_eq!(a.generation, Some(4));
        assert!(a.nonce.is_some());
        // A fresh nonce every seal — identical plaintext, different blobs.
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ct, b.ct);
        for sealed in [&a, &b] {
            let opened = open([&[9u8; 32]], &salt, LabelField::DeviceLabel, sealed).unwrap();
            assert_eq!(opened, b"Ada's laptop");
        }
    }

    #[test]
    fn convergent_is_idempotent() {
        // Re-recording the same path must not churn the column.
        let root = LabelRoot::owner([7u8; 32]);
        let salt = path_salt();
        let a = seal_convergent(&root, &salt, LabelField::SyncChangePath, PATH.as_bytes()).unwrap();
        let b = seal_convergent(&root, &salt, LabelField::SyncChangePath, PATH.as_bytes()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.to_bytes().unwrap(), b.to_bytes().unwrap());
    }

    #[test]
    fn envelope_round_trips_through_canonical_dag_cbor() {
        // The at-rest carrier: envelope → BLOB column → envelope, both modes.
        let root = LabelRoot::content_key([1u8; 32], 12);
        let salt = path_salt();
        let convergent =
            seal_convergent(&root, &salt, LabelField::ConflictDetails, PATH.as_bytes()).unwrap();
        let random =
            seal_random(&root, &salt, LabelField::ConflictDetails, b"both edited").unwrap();
        for sealed in [&convergent, &random] {
            let bytes = sealed.to_bytes().unwrap();
            assert_eq!(&SealedLabel::from_bytes(&bytes).unwrap(), sealed);
        }
    }

    #[test]
    fn wrong_root_fails_closed() {
        let salt = path_salt();
        let sealed = seal_convergent(
            &LabelRoot::owner([1u8; 32]),
            &salt,
            LabelField::SyncChangePath,
            PATH.as_bytes(),
        )
        .unwrap();
        assert!(open([&[2u8; 32]], &salt, LabelField::SyncChangePath, &sealed).is_err());
    }

    #[test]
    fn all_candidates_are_trialled() {
        // The `keys_for(version)` posture: a concurrent rotation merge leaves
        // several candidates at one generation and the tag disambiguates.
        let salt = path_salt();
        let sealed = seal_convergent(
            &LabelRoot::content_key([5u8; 32], 2),
            &salt,
            LabelField::SyncChangePath,
            PATH.as_bytes(),
        )
        .unwrap();
        let candidates = [[8u8; 32], [5u8; 32], [6u8; 32]];
        let opened = open(
            candidates.iter(),
            &salt,
            LabelField::SyncChangePath,
            &sealed,
        )
        .unwrap();
        assert_eq!(opened, PATH.as_bytes());
    }

    #[test]
    fn aad_binds_the_salt() {
        // A blob sealed for path A must not open as path B under the same root.
        let root = LabelRoot::owner([7u8; 32]);
        let salt_a = crate::sync::path_hash("a/secret.pdf");
        let salt_b = crate::sync::path_hash("b/boring.txt");
        let sealed =
            seal_convergent(&root, &salt_a, LabelField::SyncChangePath, b"a/secret.pdf").unwrap();
        assert!(open([&[7u8; 32]], &salt_b, LabelField::SyncChangePath, &sealed).is_err());
    }

    #[test]
    fn aad_binds_the_field_tag() {
        // Splicing a share filename into a device-label column fails closed.
        let root = LabelRoot::owner([7u8; 32]);
        let salt = [4u8; 32];
        let sealed =
            seal_convergent(&root, &salt, LabelField::ShareFilename, b"payslip.pdf").unwrap();
        assert!(open([&[7u8; 32]], &salt, LabelField::DeviceLabel, &sealed).is_err());
    }

    #[test]
    fn every_field_tag_is_distinct() {
        // The tags are frozen wire values and feed both the key derivation and
        // the AAD — a duplicate would silently merge two columns' domains.
        let fields = [
            LabelField::SyncChangePath,
            LabelField::ConflictDetails,
            LabelField::FolderName,
            LabelField::FolderIncludePaths,
            LabelField::FolderExcludePaths,
            LabelField::DeviceLabel,
            LabelField::ShareFilename,
            LabelField::ImportSource,
            LabelField::SnapshotTags,
        ];
        let unique: std::collections::HashSet<&str> = fields.iter().map(|f| f.tag()).collect();
        assert_eq!(unique.len(), fields.len());
    }

    // ── The sealed-first render policy ─────────────────────────────────────
    //
    // These pin the *contract* every read surface inherits: prefer the seal,
    // fall back to plaintext, and omit rather than show an empty name or blow
    // up the page (`file-sync.md` § Sealed names & paths → *Migration*).

    use crate::file_download::FileDownloadKeys;
    use crate::folder_keys::FolderContentKeys;

    fn owner_key() -> BackupKey {
        BackupKey::derive(&[3u8; 32])
    }

    /// The engine's own root selection for an unbound owner-only set
    /// (`SyncEngine::seal_recorded_path`'s `effective_backup_key` arm).
    fn owner_sealed_path() -> Vec<u8> {
        let root = LabelRoot::owner_of(&owner_key());
        seal_convergent(
            &root,
            &path_salt(),
            LabelField::SyncChangePath,
            PATH.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap()
    }

    fn render_path(
        keys: &FileDownloadKeys,
        sealed: Option<&[u8]>,
        plaintext: Option<&str>,
    ) -> SealedLabelRender {
        render_sealed_label(
            keys,
            sealed,
            plaintext,
            &path_salt(),
            LabelField::SyncChangePath,
        )
    }

    /// The flip's whole point: with the plaintext column gone, the owner still
    /// renders the true path. Blanking the plaintext is what makes this test
    /// non-vacuous — a fallback bug would otherwise pass silently.
    #[test]
    fn owner_renders_from_the_seal_with_the_plaintext_blanked() {
        let keys = FileDownloadKeys::owner(owner_key());
        assert_eq!(
            render_path(&keys, Some(&owner_sealed_path()), Some("")),
            SealedLabelRender::Sealed(PATH.to_string())
        );
        // …and identically when the column is absent rather than empty.
        assert_eq!(
            render_path(&keys, Some(&owner_sealed_path()), None),
            SealedLabelRender::Sealed(PATH.to_string())
        );
    }

    /// A row a keyless writer seam recorded (or a public-audience folder's row)
    /// has no seal, so the plaintext column is the answer — not an omission.
    #[test]
    fn a_row_with_no_seal_renders_from_the_plaintext() {
        let keys = FileDownloadKeys::owner(owner_key());
        assert_eq!(
            render_path(&keys, None, Some(PATH)),
            SealedLabelRender::Plaintext(PATH.to_string())
        );
    }

    /// The seal wins even when both are present — otherwise the plaintext
    /// column could never be scrubbed, because nothing would exercise the seal.
    #[test]
    fn the_seal_is_preferred_over_a_present_plaintext() {
        let keys = FileDownloadKeys::owner(owner_key());
        assert_eq!(
            render_path(
                &keys,
                Some(&owner_sealed_path()),
                Some("stale/plaintext.pdf")
            ),
            SealedLabelRender::Sealed(PATH.to_string())
        );
    }

    /// The ratified degrade contract, stated three ways a caller could get it
    /// wrong: a keyless reader, a wrong-key reader, and a corrupt envelope all
    /// yield `Omit` — never `Sealed("")`, never a propagated error.
    #[test]
    fn a_sealed_only_row_omits_for_a_reader_that_cannot_open_it() {
        let sealed = owner_sealed_path();

        // Keyless (the no-custody reader).
        assert_eq!(
            render_path(&FileDownloadKeys::default(), Some(&sealed), Some("")),
            SealedLabelRender::Omit
        );
        // Keyed, but with somebody else's key.
        let stranger = FileDownloadKeys::owner(BackupKey::derive(&[9u8; 32]));
        assert_eq!(
            render_path(&stranger, Some(&sealed), None),
            SealedLabelRender::Omit
        );
        // A truncated/garbage envelope is a degrade, not a panic or an error.
        assert_eq!(
            render_path(
                &FileDownloadKeys::owner(owner_key()),
                Some(b"not cbor"),
                None
            ),
            SealedLabelRender::Omit
        );
        // And `text()` reports the omission rather than an empty string.
        assert_eq!(render_path(&stranger, Some(&sealed), None).text(), None);
    }

    /// A reader who cannot open the seal but *does* still have the plaintext
    /// column shows the plaintext — the correct, and only, way an
    /// out-of-audience reader of a public folder keeps working.
    #[test]
    fn an_unopenable_seal_falls_back_to_the_plaintext_during_expand() {
        let stranger = FileDownloadKeys::owner(BackupKey::derive(&[9u8; 32]));
        assert_eq!(
            render_path(&stranger, Some(&owner_sealed_path()), Some(PATH)),
            SealedLabelRender::Plaintext(PATH.to_string())
        );
    }

    /// A bound shared set's label is stamped with its M2 generation; a member
    /// holding that generation renders it, and the *same* member fails closed
    /// on a generation they do not hold (FS-BIND-5, per generation).
    #[test]
    fn a_bound_set_renders_under_its_stamped_generation_and_fails_closed_without_it() {
        let content_key = [11u8; 32];
        let sealed = seal_convergent(
            &LabelRoot::content_key(content_key, 1),
            &path_salt(),
            LabelField::SyncChangePath,
            PATH.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        let member = FileDownloadKeys {
            backup_key: None,
            mls_group_id: Some(vec![1, 2, 3]),
            content_keys: Some(FolderContentKeys::genesis(content_key, 1_000)),
            ..Default::default()
        };
        assert_eq!(
            render_path(&member, Some(&sealed), Some("")),
            SealedLabelRender::Sealed(PATH.to_string())
        );

        // A removed member whose custody rotated past generation 1 without
        // retaining it holds no candidate for the stamp → omit.
        let removed = FileDownloadKeys {
            backup_key: None,
            mls_group_id: Some(vec![1, 2, 3]),
            content_keys: Some(FolderContentKeys::genesis([12u8; 32], 1_000)),
            ..Default::default()
        };
        assert_eq!(
            render_path(&removed, Some(&sealed), Some("")),
            SealedLabelRender::Omit
        );
    }

    /// The owner of a set that was later BOUND keeps rendering the names it
    /// sealed under the owner root before binding — the reason
    /// `label_open_roots` does not reuse the chunk path's `mls_group_id`
    /// suppression (FS-5DC applies to chunks, not to a `gen: None` label).
    #[test]
    fn a_bound_owner_still_renders_its_pre_binding_owner_sealed_labels() {
        let keys = FileDownloadKeys {
            backup_key: Some(owner_key().into()),
            mls_group_id: Some(vec![1, 2, 3]),
            content_keys: Some(FolderContentKeys::genesis([11u8; 32], 1_000)),
            ..Default::default()
        };
        assert_eq!(
            render_path(&keys, Some(&owner_sealed_path()), Some("")),
            SealedLabelRender::Sealed(PATH.to_string())
        );
    }

    /// The salt and the field tag are both load-bearing at render time: the
    /// wrong one degrades rather than rendering some other row's label.
    #[test]
    fn a_wrong_salt_or_field_degrades_instead_of_rendering_another_label() {
        let keys = FileDownloadKeys::owner(owner_key());
        let sealed = owner_sealed_path();
        assert_eq!(
            render_sealed_label(
                &keys,
                Some(&sealed),
                None,
                &crate::sync::path_hash("docs/other.pdf"),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Omit
        );
        assert_eq!(
            render_sealed_label(
                &keys,
                Some(&sealed),
                None,
                &path_salt(),
                LabelField::ConflictDetails
            ),
            SealedLabelRender::Omit
        );
    }

    #[test]
    fn domain_separated_from_chunk_and_manifest_seals() {
        // Same root, same salt material: a label key is never a chunk key or a
        // manifest key. All three two-step derivations differ only by context.
        let root = [5u8; 32];
        let composed = composed_salt(&[2u8; 32], LabelField::SyncChangePath);
        let label_key = derive_label_key(&root, &composed);
        for context in ["fauna.chunk.v1", "fauna.manifest.v1"] {
            let other = crate::domain_key::derive_domain_key(context, &root, composed.as_bytes());
            assert_ne!(label_key, other);
        }
    }

    #[test]
    fn unknown_envelope_version_fails_closed() {
        let root = LabelRoot::owner([7u8; 32]);
        let salt = path_salt();
        let mut sealed =
            seal_convergent(&root, &salt, LabelField::SyncChangePath, PATH.as_bytes()).unwrap();
        sealed.v = SEALED_LABEL_V1 + 1;
        let err = open([&[7u8; 32]], &salt, LabelField::SyncChangePath, &sealed)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown sealed-label envelope version"),
            "{err}"
        );
    }

    #[test]
    fn tampered_ciphertext_fails_closed() {
        let root = LabelRoot::owner([7u8; 32]);
        let salt = path_salt();
        let mut sealed =
            seal_convergent(&root, &salt, LabelField::SyncChangePath, PATH.as_bytes()).unwrap();
        sealed.ct[0] ^= 0xff;
        assert!(open([&[7u8; 32]], &salt, LabelField::SyncChangePath, &sealed).is_err());
    }

    #[test]
    fn empty_label_round_trips() {
        // An empty include-list is a legitimate value, not a NULL column.
        let root = LabelRoot::owner([7u8; 32]);
        let salt = [0u8; 32];
        let sealed = seal_random(&root, &salt, LabelField::FolderIncludePaths, b"").unwrap();
        assert_eq!(
            open([&[7u8; 32]], &salt, LabelField::FolderIncludePaths, &sealed).unwrap(),
            Vec::<u8>::new()
        );
    }
}
