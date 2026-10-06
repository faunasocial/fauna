//! [`VersionedManifest`] — how a placement manifest at rest negotiates its
//! own format version, owned once instead of per domain.
//!
//! ## What was duplicated
//!
//! `fauna-mail`, `fauna-calendar` and `fauna-contacts` each keep a placement
//! manifest: a compacted cache of one domain's segment placement state,
//! written as canonical dag-cbor and read back by the nest at boot. All three
//! had grown the *same five functions* — `encode`, `decode`, `load`,
//! `decode_any_version`, `save_atomic` — identical apart from the domain
//! vocabulary, doc comments included. What genuinely differs per domain is
//! the current version constant, a label, and access to the `format_version`
//! field — the trait's required items; the five functions are provided here.
//! (The frozen v1 shape and its v1-to-current upgrade were required items too
//! until the compat-remnant sweep retired them, 2026-09-24 —
//! `docs/goal/architecture/version-compatibility.md` § Dimension 2, program
//! 4: no pre-sweep manifest exists to upgrade.)
//!
//! This is the third and last half of the same lift: [`crate::atomic`] took the
//! durable-write sequence out of these three modules, then
//! [`crate::atomic::read_optional`] took the "an absent manifest means a fresh
//! actor" rule. The version ladder was what remained, and it is the half with
//! the most at stake — see below.
//!
//! ## Why one owner matters more here than for the other two
//!
//! [`decode_any_version`](VersionedManifest::decode_any_version) is the at-rest
//! **compatibility verdict**: it decides whether a manifest a *different*
//! binary wrote is understood, upgraded, or refused. Each of the three copies
//! documented the rule identically — a downgraded binary must refuse loudly,
//! never silently strip fields (`docs/goal/architecture/version-compatibility.md`
//! § 1) — and nothing made them agree. Three copies of a compat rule are three
//! chances to bump one domain's format and mis-transcribe the ladder into
//! another's, which at rest means a user's mail, calendar or contacts placement
//! state read wrong by a binary that believed it understood the file.
//!
//! ## Deliberate divergence from [`crate::manifest::Manifest`] — RULED 2026-08-24
//!
//! (Owner: `version-compatibility.md`, Dim 1's placement bullet.)
//!
//! The ladder implemented here is the **one-number** scheme: exactly
//! `CURRENT_VERSION` is read, anything else is refused — while the outer
//! [`crate::manifest::Manifest`] carries the **two-number** scheme
//! (`format_version` + `min_reader_format_version`, [`crate::version`]) under
//! which a *newer but purely additive* file stays readable. That asymmetry is
//! **ruled correct, not a gap**: a placement manifest is a wholesale-rewritten
//! derived cache whose nested structs carry no catch-alls, so "tolerating" a
//! newer-additive file would serde-drop its newer fields on the very next
//! append — a silent destruction where today's refusal is loud, write-free,
//! and rebuildable from the journal (all three kinds replay). The journal
//! records are stamp-less (`decode_any` shape-sniffs) and could not honour a
//! tolerance verdict anyway. Placement's forward-compat mechanism is a
//! version bump that freezes the outgoing record/manifest shapes, reads them
//! through an explicit upgrade, and rebuilds (the v1→v2 bump shipped exactly
//! that; its v1 arm was retired with every pre-sweep blob by the 2026-09-24
//! compat-remnant sweep, leaving the ladder at its baseline). If tolerance is
//! ever genuinely wanted, the ruling
//! requires the `fauna-index` shape (nested catch-alls + restamp-with-`max` +
//! a tier_1 unknown-fields-survive-rewrite gate), never a bare
//! `min_reader_format_version` field on the top-level struct. Full rationale +
//! the recorded residual: `version-compatibility.md`, the 2026-08-24 placement
//! bullet.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::SegmentStoreError;
use crate::codec;

/// A placement manifest that reads its own current format version and refuses
/// every other one.
///
/// Implementors supply the genuinely per-domain items; the read/write path
/// is provided. Bring the trait into scope to call the provided methods:
/// `use fauna_segment_store::VersionedManifest;`.
pub trait VersionedManifest: Serialize + DeserializeOwned + Sized {
    /// The `format_version` this binary writes.
    const CURRENT_VERSION: u16;

    /// Kebab-case domain label, e.g. `"cal-placement"`. Used verbatim in
    /// schema-mismatch messages and as the atomic-save tmp extension
    /// (`<path>.<LABEL>.tmp`), so it is **on-disk-visible**: changing it
    /// changes a transient filename in a durability path.
    const LABEL: &'static str;

    /// This instance's on-disk `format_version`.
    fn format_version(&self) -> u16;

    /// Canonical dag-cbor bytes.
    fn encode(&self) -> Result<Vec<u8>, SegmentStoreError> {
        codec::encode(self)
    }

    /// Decode bytes known to be at the current version. Use
    /// [`Self::decode_any_version`] for anything read off disk.
    fn decode(bytes: &[u8]) -> Result<Self, SegmentStoreError> {
        codec::decode(bytes)
    }

    /// Decode a manifest blob read off disk, whatever version it is stamped
    /// with. The returned manifest is always at [`Self::CURRENT_VERSION`].
    ///
    /// Undecodable bytes yield [`SegmentStoreError::Encoding`] — *damaged
    /// file*, which a caller with a durable journal may answer by rebuilding —
    /// while an intact file stamped with any other version yields
    /// [`SegmentStoreError::SchemaMismatch`] — refused loudly rather than
    /// silently stripped or reinterpreted. That includes the retired v1 stamp:
    /// the v1 read arm went with the compat-remnant sweep (2026-09-24).
    fn decode_any_version(buf: &[u8]) -> Result<Self, SegmentStoreError> {
        // The stamp FIRST (`transport.md` § Schema and forward-compat
        // discipline → *Rule 3 in full*, the `ladder` ground: the stamp is
        // read before the strict decode, or the refusal reads as corruption).
        // A later version may reshape the body — a new placement-record
        // variant inside it, a retyped field — and decoding that as this
        // build's struct fails as `Encoding`, the *damaged file* verdict a
        // caller answers by rebuilding from the journal: the downgrade that
        // strips fields this ladder exists to refuse. Every implementor keeps
        // its stamp at the map key `format_version`; a blob whose stamp does
        // not read at all is left to the full decode below.
        #[derive(serde::Deserialize)]
        struct Stamp {
            format_version: u16,
        }
        if let Ok(Stamp { format_version }) = codec::decode::<Stamp>(buf)
            && format_version != Self::CURRENT_VERSION
        {
            return Err(Self::schema_mismatch(format_version));
        }
        // A blob whose shape happens to decode as the current struct does not
        // by itself prove the version — check the field.
        let m = codec::decode::<Self>(buf)?;
        let found = m.format_version();
        if found != Self::CURRENT_VERSION {
            return Err(Self::schema_mismatch(found));
        }
        Ok(m)
    }

    /// Load from disk. `Ok(None)` means the file does not exist — a fresh
    /// actor, not an error. See [`Self::decode_any_version`] for the refusal
    /// rules.
    fn load(path: &Path) -> Result<Option<Self>, SegmentStoreError> {
        match crate::read_optional(path)? {
            Some(buf) => Self::decode_any_version(&buf).map(Some),
            None => Ok(None),
        }
    }

    /// Atomically save: write to `<path>.<LABEL>.tmp`, fsync the write fd,
    /// rename over `<path>`, fsync the parent directory. The durability
    /// sequence — including the Windows carve-out — lives in
    /// [`crate::atomic::atomic_save`].
    fn save_atomic(&self, path: &Path) -> Result<(), SegmentStoreError> {
        let bytes = self.encode()?;
        let tmp = path.with_extension(format!("{}.tmp", Self::LABEL));
        crate::atomic_save(path, &tmp, &bytes).map_err(SegmentStoreError::Io)
    }

    /// The shared refusal message. Kept identical to what the three hand-rolled
    /// copies emitted, so an operator grepping logs across a version bump sees
    /// one sentence.
    fn schema_mismatch(found: u16) -> SegmentStoreError {
        SegmentStoreError::SchemaMismatch(format!(
            "{} manifest format_version {} (expected {})",
            Self::LABEL,
            found,
            Self::CURRENT_VERSION
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    // A toy manifest exercising the ladder itself. The three real
    // implementors keep their own suites, which pin the per-domain wiring
    // (right constant, right label, right upgrade); these pin the shared
    // algorithm's own properties, which no single domain's tests state.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Toy {
        format_version: u16,
        rows: Vec<u32>,
        // v2-only: absent from the v1 shape below.
        note: Option<String>,
    }

    impl VersionedManifest for Toy {
        const CURRENT_VERSION: u16 = 2;
        const LABEL: &'static str = "toy-placement";

        fn format_version(&self) -> u16 {
            self.format_version
        }
    }

    #[test]
    fn current_version_round_trips() {
        let m = Toy {
            format_version: 2,
            rows: vec![1, 2, 3],
            note: Some("hi".into()),
        };
        let decoded = Toy::decode_any_version(&m.encode().unwrap()).unwrap();
        assert_eq!(decoded, m);
    }

    /// A v1-stamped blob is refused, not upgraded. The pre-sweep v1 read arm
    /// (and the in-place upgrade it drove) was retired by the compat-remnant
    /// sweep (program 4, 2026-09-24): no v1 manifest exists anywhere. The
    /// blob decodes as the current struct (`note` is optional), so this pins
    /// the version-field check rather than a shape failure.
    #[test]
    fn a_v1_stamped_blob_is_refused_not_upgraded() {
        let v1 = Toy {
            format_version: 1,
            rows: vec![7],
            note: None,
        };
        let err = Toy::decode_any_version(&v1.encode().unwrap()).unwrap_err();
        match err {
            SegmentStoreError::SchemaMismatch(msg) => {
                assert!(msg.contains("toy-placement"), "{msg}");
                assert!(msg.contains("format_version 1"), "{msg}");
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_version_is_refused_loudly_not_stripped() {
        let future = Toy {
            format_version: 99,
            rows: vec![],
            note: None,
        };
        let err = Toy::decode_any_version(&future.encode().unwrap()).unwrap_err();
        match err {
            SegmentStoreError::SchemaMismatch(msg) => {
                assert!(msg.contains("toy-placement"), "{msg}");
                assert!(msg.contains("99"), "{msg}");
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }

    /// **The stamp is read before the body**: a later version's manifest
    /// whose body this build cannot decode at all — `rows` retyped, as a new
    /// placement-record variant would reshape a real manifest — is an intact
    /// file this binary predates (`SchemaMismatch`, refused), never a damaged
    /// one (`Encoding`, which the nest answers by rebuilding from the journal
    /// and so stripping what it cannot read). The same body under the current
    /// stamp IS damaged.
    #[test]
    fn a_later_versions_reshaped_body_is_refused_not_rebuilt() {
        #[derive(Serialize)]
        struct Reshaped {
            format_version: u16,
            rows: Vec<String>,
        }
        let bytes = |format_version| {
            codec::encode(&Reshaped {
                format_version,
                rows: vec!["a later record".into()],
            })
            .unwrap()
        };
        assert!(
            Toy::decode(&bytes(3)).is_err(),
            "the body does not decode here"
        );
        match Toy::decode_any_version(&bytes(3)).unwrap_err() {
            SegmentStoreError::SchemaMismatch(msg) => assert!(msg.contains("format_version 3")),
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
        assert!(matches!(
            Toy::decode_any_version(&bytes(Toy::CURRENT_VERSION)).unwrap_err(),
            SegmentStoreError::Encoding(_)
        ));
    }

    #[test]
    fn undecodable_bytes_are_encoding_not_schema_mismatch() {
        // The distinction a caller with a durable journal branches on: damaged
        // file (rebuild) vs. intact file this binary predates (refuse).
        let err = Toy::decode_any_version(b"not cbor at all").unwrap_err();
        assert!(
            matches!(err, SegmentStoreError::Encoding(_)),
            "expected Encoding, got {err:?}"
        );
    }

    #[test]
    fn load_of_a_missing_file_is_ok_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.cbor");
        assert!(Toy::load(&missing).unwrap().is_none());
    }

    #[test]
    fn save_atomic_round_trips_and_leaves_no_tmp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toy.cbor");
        let m = Toy {
            format_version: 2,
            rows: vec![4, 5],
            note: None,
        };
        m.save_atomic(&path).unwrap();
        assert_eq!(Toy::load(&path).unwrap().unwrap(), m);
        // The tmp name is derived from LABEL and is on-disk-visible; a rename
        // that did not happen would leave it sitting next to the manifest.
        assert!(!path.with_extension("toy-placement.tmp").exists());
    }
}
