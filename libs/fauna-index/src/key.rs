//! `IndexMasterKey` — the per-user 32-byte symmetric key that protects every
//! segment header and every master-direct AEAD blob (manifest, classifier
//! ledger).
//!
//! The bytes are zeroized on drop. Construction is deliberately explicit
//! (`from_bytes`) so callers don't accidentally derive the key from the wrong
//! source — the derivation is `fauna_core::crypto::derive_index_master_key`
//! (identity-seed-derived, ratified 2026-08-04 in `key-material-hierarchy.md`
//! § Path A-sibling); this crate just consumes the 32 bytes.

use zeroize::Zeroize;

/// Per-user index master key. Wraps every segment's per-segment data key and
/// directly encrypts manifest / classifier-ledger blobs.
#[derive(Clone)]
pub struct IndexMasterKey([u8; 32]);

impl IndexMasterKey {
    /// Construct from raw bytes. Callers are responsible for the derivation —
    /// this newtype only enforces ownership and zeroize-on-drop.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes. The `seal` module hands this to the AEAD
    /// primitive (XChaCha20-Poly1305) at encrypt/decrypt time.
    ///
    /// Visibility is `pub(crate)`: callers outside `fauna-index` should not
    /// need to read raw key material — they construct via `from_bytes` and
    /// hand the key to public methods like `Index::seal_encrypted`. If a
    /// future plan (Plan 4 — UniFFI/WASM exposure) needs to widen this, it
    /// should be a deliberate decision in that plan, not a default-`pub`
    /// surface adopted accidentally.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for IndexMasterKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The **mail/calendar index-segment key** — the per-kind wrap key of the
/// S0-ratified per-kind key split (`content-index.md` § Encryption posture;
/// `key-material-hierarchy.md` § Path B-sibling-4). Mail-kind and calendar-kind
/// segments' data keys, and `manifest-mailcal.idx`, seal under this key instead
/// of the master key — which is what bounds a MUA credential's index reach to
/// mail/calendar (rule #7) while every other kind stays master-key-wrapped.
///
/// Same stance as [`IndexMasterKey`]: this crate consumes the 32 bytes opaquely;
/// the derivation (`BLAKE3::derive_key("fauna.mail.index-seg.v1 2026-08-02", MSEK)`)
/// lives in `fauna-mls`'s wrapped-blob module beside its siblings.
#[derive(Clone)]
pub struct IndexSegmentKey([u8; 32]);

impl IndexSegmentKey {
    /// Construct from raw bytes. Callers are responsible for the derivation.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes — `pub(crate)` for the same reason as the master
    /// key's: key material never leaves this crate's seal module.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for IndexSegmentKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The wrap key for **one key class** of the per-kind index split — the runtime
/// pairing of [`KindClass`] with the key that class seals under.
///
/// Every sealing operation in this crate exists as a master/mail-calendar
/// **pair** (`seal_segment_bytes` / `seal_segment_bytes_mailcal`,
/// `IndexManifest::to_sealed_bytes` / `…_mailcal`, and four more). That is the
/// right shape for a caller that statically knows its class — the MDA bridge
/// can only ever hold the mail/calendar key, and the type system saying so is
/// exactly what enforces `key-material-hierarchy.md` rule #7.
///
/// It is the *wrong* shape for a caller that is generic over class: a builder
/// serving master-class kinds is otherwise a line-for-line copy of the
/// mail/calendar one with the other half of each pair called. `ClassKey` is
/// that one axis of difference made into a value, so **one** builder serves
/// both classes (`content-index.md` § Where the index is built — the client
/// leg builds every kind, the MDA leg only mail/calendar).
///
/// It deliberately does **not** widen anyone's key reach: constructing one
/// still requires already holding the class's key, and a `MailCal` value can
/// no more seal a master-class kind than the free function could — the
/// manifest's own [`KindClass`] check (`IndexError::WrongKindClass`) is what
/// enforces that, and it is unchanged. This type only removes the duplication
/// on the *caller* side.
pub enum ClassKey {
    /// Master-class kinds: conversation, post, file, contact, draft, media —
    /// plus `manifest.idx` and the classifier ledger.
    Master(IndexMasterKey),
    /// Mail + calendar kinds, and `manifest-mailcal.idx`.
    MailCal(IndexSegmentKey),
}

impl ClassKey {
    /// Which class this key seals. Total, and the only place the mapping is
    /// made — callers branch on this rather than on the variant.
    pub fn class(&self) -> crate::KindClass {
        match self {
            ClassKey::Master(_) => crate::KindClass::Master,
            ClassKey::MailCal(_) => crate::KindClass::MailCal,
        }
    }

    /// The `__index` path of this class's manifest file.
    pub fn manifest_path(&self) -> String {
        match self {
            ClassKey::Master(_) => crate::paths::manifest_path(),
            ClassKey::MailCal(_) => crate::paths::mailcal_manifest_path(),
        }
    }

    /// Seal one segment blob under this class's wrap key.
    pub fn seal_segment_bytes(&self, plaintext: &[u8]) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::seal::seal_segment_bytes(plaintext, k),
            ClassKey::MailCal(k) => crate::seal::seal_segment_bytes_mailcal(plaintext, k),
        }
    }

    /// Inverse of [`Self::seal_segment_bytes`].
    pub fn open_segment_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::seal::open_segment_bytes(sealed, k),
            ClassKey::MailCal(k) => crate::seal::open_segment_bytes_mailcal(sealed, k),
        }
    }

    /// Seal a small metadata blob **directly** under this class's key (the
    /// master-direct framing the manifest and classifier ledger use), rather
    /// than through the per-segment wrapped-data-key path.
    pub fn seal_blob(&self, plaintext: &[u8]) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::seal::seal_under_master(plaintext, k),
            ClassKey::MailCal(k) => crate::seal::seal_under_mailcal_key(plaintext, k),
        }
    }

    /// Inverse of [`Self::seal_blob`].
    pub fn open_blob(&self, sealed: &[u8]) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::seal::open_under_master(sealed, k),
            ClassKey::MailCal(k) => crate::seal::open_under_mailcal_key(sealed, k),
        }
    }

    /// Seal a built in-memory index as this class's segment blob.
    pub fn seal_index(&self, index: &mut crate::Index) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => index.seal_encrypted(k),
            ClassKey::MailCal(k) => index.seal_encrypted_mailcal(k),
        }
    }

    /// Inverse of [`Self::seal_index`].
    pub fn open_index(&self, sealed: &[u8]) -> Result<crate::Index, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::Index::open_encrypted(sealed, k),
            ClassKey::MailCal(k) => crate::Index::open_encrypted_mailcal(sealed, k),
        }
    }

    /// Seal this class's manifest.
    pub fn seal_manifest(
        &self,
        manifest: &crate::IndexManifest,
    ) -> Result<Vec<u8>, crate::IndexError> {
        match self {
            ClassKey::Master(k) => manifest.to_sealed_bytes(k),
            ClassKey::MailCal(k) => manifest.to_sealed_bytes_mailcal(k),
        }
    }

    /// Inverse of [`Self::seal_manifest`].
    pub fn open_manifest(&self, sealed: &[u8]) -> Result<crate::IndexManifest, crate::IndexError> {
        match self {
            ClassKey::Master(k) => crate::IndexManifest::from_sealed_bytes(sealed, k),
            ClassKey::MailCal(k) => crate::IndexManifest::from_sealed_bytes_mailcal(sealed, k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KindClass, TOKENIZER_PIPELINE_VERSION};

    #[test]
    fn index_master_key_round_trips_through_bytes() {
        let raw = [42u8; 32];
        let key = IndexMasterKey::from_bytes(raw);
        assert_eq!(key.as_bytes(), &raw);
    }

    fn master() -> ClassKey {
        ClassKey::Master(IndexMasterKey::from_bytes([7u8; 32]))
    }

    fn mailcal() -> ClassKey {
        ClassKey::MailCal(IndexSegmentKey::from_bytes([9u8; 32]))
    }

    #[test]
    fn class_and_manifest_path_follow_the_variant() {
        assert_eq!(master().class(), KindClass::Master);
        assert_eq!(mailcal().class(), KindClass::MailCal);
        assert_eq!(master().manifest_path(), crate::paths::manifest_path());
        assert_eq!(
            mailcal().manifest_path(),
            crate::paths::mailcal_manifest_path()
        );
        // The two classes never share a file — that separation is what lets the
        // MDA open only its own manifest.
        assert_ne!(master().manifest_path(), mailcal().manifest_path());
    }

    #[test]
    fn each_class_round_trips_its_own_blobs_and_manifests() {
        for key in [master(), mailcal()] {
            let sealed = key.seal_blob(b"metadata").unwrap();
            assert_eq!(key.open_blob(&sealed).unwrap(), b"metadata");

            let sealed = key.seal_segment_bytes(b"segment").unwrap();
            assert_eq!(key.open_segment_bytes(&sealed).unwrap(), b"segment");

            let manifest = crate::IndexManifest::empty(key.class(), TOKENIZER_PIPELINE_VERSION);
            let sealed = key.seal_manifest(&manifest).unwrap();
            assert_eq!(key.open_manifest(&sealed).unwrap().class, key.class());
        }
    }

    #[test]
    fn a_class_key_cannot_open_the_other_class_seal() {
        // The whole point of the split: dispatching through `ClassKey` must not
        // quietly make one class's key work on the other's bytes.
        let sealed_master = master().seal_segment_bytes(b"secret").unwrap();
        assert!(mailcal().open_segment_bytes(&sealed_master).is_err());

        let sealed_mailcal = mailcal().seal_blob(b"secret").unwrap();
        assert!(master().open_blob(&sealed_mailcal).is_err());
    }
}
