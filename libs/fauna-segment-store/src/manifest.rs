//! `Manifest` — kind-tagged outer manifest for one scope's segment store.
//! Replaces per-kind wrappers (`MailManifest` etc.); the `kind` field
//! self-tags the file so the loader can verify the scope/kind alignment.
//!
//! Encoding: canonical dag-cbor (via `fauna_cbor::encode_canonical` /
//! `decode_strict`). Layer 3 of the CBOR-DAG-everywhere migration replaced
//! the prior BARE encoding. Manifests are small (low KB), so the
//! canonical-CBOR overhead is negligible.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::KindManifest;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u16,
    /// The oldest binary `format_version` that can still safely read this
    /// manifest (the at-rest reader floor — § 2.2's `min_reader_version`,
    /// mirrored at-rest). `#[serde(default)]` so existing v1 manifests written
    /// before this field existed deserialize to the baseline `1` and stay
    /// readable. See [`crate::version`].
    #[serde(default = "crate::version::baseline_min_reader_version")]
    pub min_reader_format_version: u16,
    pub kind: String,
    pub kind_manifest: KindManifest,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding: {0}")]
    Encoding(String),
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
    #[error("kind mismatch: expected '{expected}', got '{got}'")]
    KindMismatch { expected: String, got: String },
}

/// The `format_version` this binary **writes** into new manifests. ⚠ A manifest
/// describes user mail/conv **segment data**, so under the alpha
/// no-user-data-loss rule (`docs/goal/architecture/version-compatibility.md`
/// § 1) the on-disk format must evolve **additively** within a major version
/// (forward-compat, like the wire `extra` catch-all — `transport.md`
/// § forward-compat); a genuinely incompatible change is a major-version event
/// with a migration (§ I3), never a reject-and-recreate. `load_or_empty` no
/// longer rejects on `format_version != MANIFEST_SCHEMA_VERSION`: it applies the
/// two-number verdict of [`crate::version`], tolerating a newer-*additive*
/// manifest (its `min_reader_format_version ≤` this constant) and erroring only
/// on a newer-*breaking* one — so a future additive bump does **not** orphan
/// existing manifests (I1/I2).
const MANIFEST_SCHEMA_VERSION: u16 = 1;

/// The `min_reader_format_version` this binary **stamps** into new manifests:
/// the oldest binary `format_version` that can still read what we write today.
/// Bumped **only** on a breaking, non-additive manifest change (the contract
/// step, deferred to a major version per I3); an additive bump of
/// `MANIFEST_SCHEMA_VERSION` leaves this at the prior floor so older binaries
/// keep reading. Mirrors the DB's `MIN_READER_SCHEMA_VERSION`.
const MANIFEST_MIN_READER_VERSION: u16 = 1;

impl Manifest {
    pub fn empty(kind: impl Into<String>) -> Self {
        Self {
            format_version: MANIFEST_SCHEMA_VERSION,
            min_reader_format_version: MANIFEST_MIN_READER_VERSION,
            kind: kind.into(),
            kind_manifest: KindManifest::default(),
        }
    }

    pub fn load_or_empty(path: &Path, expected_kind: &str) -> Result<Self, ManifestError> {
        if !path.exists() {
            return Ok(Self::empty(expected_kind));
        }
        let buf = std::fs::read(path)?;
        let m: Manifest = fauna_cbor::decode_strict(&buf)
            .map_err(|e| ManifestError::Encoding(format!("decode manifest: {e}")))?;
        if let crate::version::FormatVerdict::IncompatibleNewer {
            file_v,
            file_min,
            bin_v,
        } = crate::version::check_format_compatibility(
            m.format_version,
            m.min_reader_format_version,
            MANIFEST_SCHEMA_VERSION,
        ) {
            return Err(ManifestError::SchemaMismatch(format!(
                "manifest format_version {file_v} requires a reader at format_version >= \
                 {file_min}, but this binary writes format_version {bin_v} — this nest binary \
                 must be updated"
            )));
        }
        if m.kind != expected_kind {
            return Err(ManifestError::KindMismatch {
                expected: expected_kind.to_string(),
                got: m.kind,
            });
        }
        Ok(m)
    }

    /// Atomically save: write to `<path>.tmp`, fsync, rename, fsync the parent
    /// directory. The durability sequence — including the Windows carve-out
    /// this manifest's own 2026-06-15 incident produced — lives in
    /// [`crate::atomic::atomic_save`].
    pub fn save_atomic(&self, path: &Path) -> Result<(), ManifestError> {
        let bytes = fauna_cbor::encode_canonical(self)
            .map_err(|e| ManifestError::Encoding(format!("encode manifest: {e}")))?;
        let parent = path.parent().expect("manifest path must have parent");
        let tmp = parent.join(format!(
            "{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));
        crate::atomic::atomic_save(path, &tmp, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn empty_returns_empty_kind_manifest_and_kind() {
        let m = Manifest::empty("mail");
        assert_eq!(m.format_version, MANIFEST_SCHEMA_VERSION);
        assert_eq!(m.kind, "mail");
        assert_eq!(m.kind_manifest, KindManifest::default());
    }

    #[test]
    fn load_or_empty_returns_empty_for_missing_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mail");
        let m = Manifest::load_or_empty(&path, "mail").unwrap();
        assert_eq!(m, Manifest::empty("mail"));
    }

    #[test]
    fn round_trips_via_save_then_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mail");
        let mut m = Manifest::empty("mail");
        m.kind_manifest.next_seg_id = 7;
        m.kind_manifest.live_segments = vec![1, 2, 3];
        m.kind_manifest.tombstoned_segments = vec![4];
        m.save_atomic(&path).unwrap();
        let loaded = Manifest::load_or_empty(&path, "mail").unwrap();
        assert_eq!(loaded, m);
    }

    #[test]
    fn rejects_kind_mismatch_on_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mail");
        Manifest::empty("mail").save_atomic(&path).unwrap();
        let err = Manifest::load_or_empty(&path, "conv").unwrap_err();
        assert!(matches!(err, ManifestError::KindMismatch { .. }));
    }

    #[test]
    fn rejects_newer_breaking_format_on_load() {
        // A manifest from a newer binary that raised its reader floor past us
        // (format_version 99, min_reader_format_version 99 > our 1) is a
        // genuinely breaking format — honest SchemaMismatch, never a silent
        // orphan (version-compatibility.md § 2.2 / Dim 1).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mail");
        let breaking = Manifest {
            format_version: 99,
            min_reader_format_version: 99,
            kind: "mail".into(),
            kind_manifest: KindManifest::default(),
        };
        let bytes = fauna_cbor::encode_canonical(&breaking).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let err = Manifest::load_or_empty(&path, "mail").unwrap_err();
        assert!(matches!(err, ManifestError::SchemaMismatch(_)));
    }

    #[test]
    fn tolerates_newer_additive_format_on_load() {
        // A newer binary wrote an additively-grown manifest (format_version 99
        // but min_reader_format_version still 1 ≤ our 1): an older binary must
        // read it (I2 backward-compat). The kind/kind_manifest payload survives.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mail");
        let mut newer = Manifest::empty("mail");
        newer.format_version = 99;
        newer.min_reader_format_version = 1;
        newer.kind_manifest.next_seg_id = 5;
        newer.kind_manifest.live_segments = vec![2, 3];
        let bytes = fauna_cbor::encode_canonical(&newer).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let loaded = Manifest::load_or_empty(&path, "mail").expect("newer-additive must load");
        assert_eq!(loaded.format_version, 99);
        assert_eq!(loaded.kind_manifest.next_seg_id, 5);
        assert_eq!(loaded.kind_manifest.live_segments, vec![2, 3]);
    }
}
