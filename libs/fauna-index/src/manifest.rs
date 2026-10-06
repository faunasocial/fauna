//! `IndexManifest` — the typed payload of `__index/manifest.idx`.
//!
//! Per spec D4, the manifest is the only mutable file in `__index/`. It lists
//! each content kind's live segments + tombstones + next-id, plus
//! crate-format and tokenizer versions that drive Plan 10's reindex.
//!
//! ## On-disk format (post-Layer 3)
//!
//! The plaintext bytes are a standard **single-block CARv2 file**:
//!
//! ```text
//! [pragma (11 bytes)]
//! [v2 header (40 bytes)]
//! [CARv1 data payload]
//!     varint(header_len) || dag_cbor({roots: [root_cid], version: 1})
//!     varint(record_len) || root_cid_bytes || dag_cbor_payload
//! [MultihashIndexSorted index]
//! ```
//!
//! where `dag_cbor_payload = encode_canonical(IndexManifest)` and `root_cid =
//! Cid::of_dag_cbor(dag_cbor_payload)`. Any CARv2-capable tool (`go-car`, `kubo dag
//! import`, the `fauna-carv2` Reader) decodes the same bytes byte-identically.
//!
//! At rest on a nest the CARv2 is always wrapped in an XChaCha20-Poly1305
//! outer seal (no-modes — every nest stores it opaque): the master-class
//! manifest under the user's [`IndexMasterKey`] (`to_sealed_bytes`), the
//! mail/calendar manifest under the [`IndexSegmentKey`]
//! (`to_sealed_bytes_mailcal`) — the v2 per-kind split. The plaintext form
//! exists for the builder's local working copy and tests.

use crate::key::{IndexMasterKey, IndexSegmentKey};
use crate::seal::{
    open_under_mailcal_key, open_under_master, seal_under_mailcal_key, seal_under_master,
};
use crate::types::{ContentKind, IndexError, KindClass};
use crate::version::{CURRENT_INDEX_FORMAT_VERSION, IndexFormatStamp};
use fauna_carv2::{Reader, Writer};
use fauna_cbor::{Cid, decode_strict, encode_canonical};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Cursor;

/// On-disk format version for `IndexManifest`. Bump triggers a Plan 10
/// re-index. The crate's at-rest format carries **one** version pair
/// (`crate::version`), shared with the seal framings, so the manifest and the
/// envelope can never drift into two dialects of the same scheme (#3).
pub const MANIFEST_FORMAT_VERSION: u8 = CURRENT_INDEX_FORMAT_VERSION;

pub use fauna_segment_store::KindManifest;

/// The manifest payload. **Two instances per actor since the v2 per-kind split**
/// (`content-index.md` § Encryption posture, ratified 2026-08-02): the
/// **master-class** manifest at `__index/manifest.idx` (kinds conversation /
/// post / file / contact / draft / media, sealed under the [`IndexMasterKey`])
/// and the **mail/calendar** manifest at `__index/manifest-mailcal.idx` (kinds
/// mail + calendar, sealed under the [`IndexSegmentKey`] so the MDA bridge can
/// open it without the cross-kind master key). Same payload type, same framing;
/// the `class` field is the self-describing identity and every kind-taking
/// method refuses a wrong-class kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexManifest {
    /// Crate-format version — bumped on **every** format change (`crate::version`).
    /// Required: every writer stamps it, and a manifest without it is refused (the
    /// pre-scheme "absent ⇒ baseline" default was retired by the compat-remnant
    /// sweep, `version-compatibility.md` § Dimension 2, program 4).
    pub format_version: u8,
    /// The oldest binary `format_version` that may still **rewrite** this manifest.
    /// Bumped only on a breaking change. Required, like [`Self::format_version`].
    pub min_reader_version: u8,
    /// Which key class this manifest file covers (v2 split). Required: every
    /// writer stamps it (the pre-v2 `Master` default went with the sweep above).
    pub class: KindClass,
    /// Tokenizer-pipeline version from `fauna_mail`. Mismatch likewise
    /// triggers reindex (D9).
    pub tokenizer_version: u32,
    /// Vec of (kind, per-kind state). Stored as a Vec rather than a HashMap
    /// because the canonical dag-cbor encoder needs deterministic ordering
    /// and the total entry count is bounded by `ContentKind::ALL.len()` (= 8).
    pub kinds: Vec<(ContentKind, KindManifest)>,
    /// Advisory per-kind ingest cursors (S0(d), `content-index.md` § Ingest
    /// triggers, v1): "content at/below this per-kind position has been ingested
    /// by some builder". Max-merged across builders; **correctness never rests
    /// on them** — `Index::add_doc` upserts by content id and the query seam
    /// dedups across segments, so a lost advance means re-ingest, never
    /// duplication. Additive + omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ingest_cursors: Vec<(ContentKind, u64)>,
    /// Advisory per-kind **corpus markers**: an opaque stamp of the corpus a
    /// third-ingest-class builder last staged, so an unchanged corpus can be
    /// recognized and skipped (`content-index.md` § Ingest triggers, v1 — the
    /// third-ingest-class template, piece 2: *change-suppressed by the corpus's
    /// own version marker*).
    ///
    /// **Why this is not [`Self::ingest_cursors`].** A cursor is a monotonic
    /// position, which is what makes max-merging it across builders correct. A
    /// corpus marker is an *identity*, not a position — one corpus is not
    /// "greater than" another — so entries here are **last-writer-wins** and
    /// compared only for equality.
    ///
    /// Opaque bytes, and per kind rather than per anything narrower, so the
    /// shared format learns nothing about any one kind's corpus: contacts stamp
    /// a digest over their `(addressbook_id, ctag)` set, and a later third-class
    /// kind stamps whatever its own wire offers.
    ///
    /// **Correctness never rests on this.** A lost, rewound or colliding marker
    /// costs one redundant whole-corpus re-read and republish — never a missed
    /// change, because the walk re-runs at every attach and every change signal.
    /// Additive + omitted when empty, exactly like the cursors above.
    #[serde(default, with = "marker_list", skip_serializing_if = "Vec::is_empty")]
    pub corpus_markers: Vec<(ContentKind, Vec<u8>)>,
    /// Every map key this build has no named field for — a field a **newer** build
    /// added. The manifest is rewritten wholesale on every `append_segment` /
    /// `tombstone_segment`, so without this catch-all an older build would silently
    /// *drop* a newer build's fields on the next rewrite. Mirrors `AccountIndex::extra`
    /// (`version-compatibility.md` § 5).
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// The version pair alone, decoded from a manifest payload **without** requiring the
/// rest of the payload to parse.
///
/// This names only the two version fields on purpose: serde ignores map keys a struct
/// has no field for, so it decodes out of a payload whose *other* fields a future
/// non-additive change has made unreadable to this build (a retyped `kinds`, say).
/// That is what turns an unreadable manifest from a bare "corrupt" into the honest,
/// actionable "this build is too old for the index; the index is intact". Without the
/// peek, a breaking change and a bit-flip are indistinguishable, and the only safe
/// response to both is a refusal the user cannot act on.
/// `corpus_markers` as a list of `(kind, byte string)` pairs: a marker is raw
/// bytes, and every raw-byte field rides as a CBOR byte string
/// (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
/// "Variable-length byte fields") — `serde_bytes` has no impl for a byte
/// vector inside a tuple.
mod marker_list {
    use super::ContentKind;
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::ptr_arg)] // `with =` hands the field's own type.
    pub fn serialize<S: Serializer>(
        v: &Vec<(ContentKind, Vec<u8>)>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(v.iter().map(|(k, m)| (k, serde_bytes::Bytes::new(m))))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(ContentKind, Vec<u8>)>, D::Error> {
        let v = Vec::<(ContentKind, serde_bytes::ByteBuf)>::deserialize(deserializer)?;
        Ok(v.into_iter().map(|(k, m)| (k, m.into_vec())).collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestVersionStamp {
    format_version: u8,
    min_reader_version: u8,
}

/// Pull the single dag-cbor block out of a plaintext CARv2 manifest, verifying the
/// root CID against the payload. Shared by the full decode and the version peek — the
/// peek must not re-implement framing checks, or the two could disagree about which
/// bytes are even a manifest.
fn carv2_payload(bytes: &[u8]) -> Result<Vec<u8>, IndexError> {
    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor)
        .map_err(|e| IndexError::SchemaMismatch(format!("carv2 reader init: {e}")))?;
    if reader.len() != 1 {
        return Err(IndexError::SchemaMismatch(format!(
            "expected single-block CARv2 manifest, got {} blocks",
            reader.len()
        )));
    }
    let (cid, payload) = reader
        .iter()
        .next()
        .expect("len() == 1 implies one block")
        .map_err(|e| IndexError::SchemaMismatch(format!("carv2 read block: {e}")))?;
    // Belt-and-suspenders: the on-disk CID must hash the on-disk payload.
    // `Reader::get` skips this check on the hot path (see reader.rs docs);
    // for the manifest we do the second-tier check because it's one tiny
    // block and the rest of the system trusts manifest integrity.
    if !cid.matches(&payload) {
        return Err(IndexError::SchemaMismatch(format!(
            "manifest payload digest does not match root cid {cid}"
        )));
    }
    Ok(payload)
}

impl IndexManifest {
    /// Build an empty manifest of `class`, pinned to this build's version pair
    /// and the caller-supplied `tokenizer_version`. No kinds are populated;
    /// callers add entries via `append_segment`.
    pub fn empty(class: KindClass, tokenizer_version: u32) -> Self {
        let stamp = IndexFormatStamp::current();
        Self {
            format_version: stamp.format_version,
            min_reader_version: stamp.min_reader_version,
            class,
            tokenizer_version,
            kinds: Vec::new(),
            ingest_cursors: Vec::new(),
            corpus_markers: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// The wrong-class refusal every kind-taking method shares.
    fn check_kind_class(&self, kind: ContentKind) -> Result<(), IndexError> {
        if kind.class() == self.class {
            Ok(())
        } else {
            Err(IndexError::WrongKindClass {
                kind: kind.as_str().to_string(),
                kind_class: format!("{:?}", kind.class()),
                manifest_class: format!("{:?}", self.class),
            })
        }
    }

    /// This manifest's recorded version pair.
    pub fn stamp(&self) -> IndexFormatStamp {
        IndexFormatStamp::from_raw(self.format_version, self.min_reader_version)
    }

    /// Read the version pair out of plaintext CARv2 manifest bytes **without**
    /// decoding the manifest itself — the peek (see [`ManifestVersionStamp`]). Use it
    /// to explain a refusal that [`Self::from_plaintext_bytes`] could only report as a
    /// decode failure.
    pub fn peek_stamp(bytes: &[u8]) -> Result<IndexFormatStamp, IndexError> {
        let payload = carv2_payload(bytes)?;
        let stamp: ManifestVersionStamp = decode_strict(&payload)
            .map_err(|e| IndexError::SchemaMismatch(format!("peek manifest version: {e}")))?;
        Ok(IndexFormatStamp::from_raw(
            stamp.format_version,
            stamp.min_reader_version,
        ))
    }

    /// Stamp this manifest forward before a rewrite: `max` on both numbers, **never**
    /// a restamp down (§ 2.2). Call before re-encoding a manifest that was read from
    /// disk — an older build rewriting a *newer-additive* manifest must keep the newer
    /// numbers, because `extra` round-trips the fields it cannot name, so the bytes it
    /// writes back really are still that newer shape.
    pub fn restamp(&mut self) {
        let restamped = self.stamp().restamped();
        self.format_version = restamped.format_version;
        self.min_reader_version = restamped.min_reader_version;
    }

    /// Serialize the manifest as plaintext bytes — a single-block CARv2 file
    /// containing one canonical dag-cbor block (the encoded `IndexManifest`).
    /// Suitable for direct on-disk storage on personal nest.
    pub fn to_plaintext_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let payload = encode_canonical(self)
            .map_err(|e| IndexError::SchemaMismatch(format!("encode manifest: {e}")))?;
        let root_cid = Cid::of_dag_cbor(&payload);

        let mut buf = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = Writer::new(&mut buf, &[&root_cid])
                .map_err(|e| IndexError::SchemaMismatch(format!("carv2 writer init: {e}")))?;
            writer
                .write_block(&root_cid, &payload)
                .map_err(|e| IndexError::SchemaMismatch(format!("carv2 write block: {e}")))?;
            writer
                .finalize()
                .map_err(|e| IndexError::SchemaMismatch(format!("carv2 finalize: {e}")))?;
        }
        Ok(buf.into_inner())
    }

    /// Deserialize from plaintext CARv2 bytes. Validates the CARv2 framing, pulls the
    /// single block, applies the § 2.2 version verdict, then dag-cbor-decodes.
    ///
    /// The verdict runs **before** the decode, off the peeked stamp, so a
    /// newer-*breaking* manifest refuses with the typed [`IndexError::Incompatible`]
    /// ("intact, this build is too old") rather than whatever decode error its
    /// unfamiliar shape happens to produce — which a caller could not tell from
    /// "corrupt". A newer-*additive* manifest decodes normally: the fields this build
    /// cannot name land in [`Self::extra`] and survive the next rewrite (I2).
    pub fn from_plaintext_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let payload = carv2_payload(bytes)?;
        let stamp: ManifestVersionStamp = decode_strict(&payload)
            .map_err(|e| IndexError::SchemaMismatch(format!("peek manifest version: {e}")))?;
        IndexFormatStamp::from_raw(stamp.format_version, stamp.min_reader_version).check()?;
        decode_strict(&payload)
            .map_err(|e| IndexError::SchemaMismatch(format!("decode manifest: {e}")))
    }

    /// Serialize as plaintext CARv2, then AEAD-seal under `master` using the
    /// direct framing. Refuses a mail/calendar-class manifest — that file seals
    /// under the index-segment key ([`Self::to_sealed_bytes_mailcal`]), which is
    /// the whole point of the split.
    pub fn to_sealed_bytes(&self, master: &IndexMasterKey) -> Result<Vec<u8>, IndexError> {
        if self.class != KindClass::Master {
            return Err(IndexError::SchemaMismatch(
                "mail/calendar manifest seals under the index-segment key \
                 (to_sealed_bytes_mailcal), never the master key"
                    .into(),
            ));
        }
        let plaintext = self.to_plaintext_bytes()?;
        seal_under_master(&plaintext, master)
    }

    /// [`Self::to_sealed_bytes`]'s mail/calendar twin. Refuses a master-class
    /// manifest for the symmetric reason.
    pub fn to_sealed_bytes_mailcal(&self, key: &IndexSegmentKey) -> Result<Vec<u8>, IndexError> {
        if self.class != KindClass::MailCal {
            return Err(IndexError::SchemaMismatch(
                "master-class manifest seals under the master key (to_sealed_bytes), \
                 never the index-segment key"
                    .into(),
            ));
        }
        let plaintext = self.to_plaintext_bytes()?;
        seal_under_mailcal_key(&plaintext, key)
    }

    /// AEAD-decrypt under `master`, then parse the inner CARv2 and decode the
    /// manifest. Belt-and-suspenders: verifies the decoded `class` is
    /// master-class (a wrong-key open already fails the AEAD, but a
    /// mis-provisioned key equal across classes must not smuggle a manifest
    /// into the wrong reader).
    pub fn from_sealed_bytes(sealed: &[u8], master: &IndexMasterKey) -> Result<Self, IndexError> {
        let plaintext = open_under_master(sealed, master)?;
        let manifest = Self::from_plaintext_bytes(&plaintext)?;
        if manifest.class != KindClass::Master {
            return Err(IndexError::SchemaMismatch(
                "sealed blob decoded as a mail/calendar manifest — wrong reader".into(),
            ));
        }
        Ok(manifest)
    }

    /// [`Self::from_sealed_bytes`]'s mail/calendar twin.
    pub fn from_sealed_bytes_mailcal(
        sealed: &[u8],
        key: &IndexSegmentKey,
    ) -> Result<Self, IndexError> {
        let plaintext = open_under_mailcal_key(sealed, key)?;
        let manifest = Self::from_plaintext_bytes(&plaintext)?;
        if manifest.class != KindClass::MailCal {
            return Err(IndexError::SchemaMismatch(
                "sealed blob decoded as a master-class manifest — wrong reader".into(),
            ));
        }
        Ok(manifest)
    }

    /// Borrow the per-kind state if it's been populated.
    pub fn kind(&self, kind: ContentKind) -> Option<&KindManifest> {
        self.kinds
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, km)| km)
    }

    /// Mutably borrow the per-kind state, creating an empty entry if none
    /// exists yet. Internal helper for `append_segment` / `tombstone_segment`.
    fn kind_mut_or_default(&mut self, kind: ContentKind) -> &mut KindManifest {
        if let Some(idx) = self.kinds.iter().position(|(k, _)| *k == kind) {
            &mut self.kinds[idx].1
        } else {
            self.kinds.push((kind, KindManifest::empty()));
            &mut self.kinds.last_mut().expect("just pushed").1
        }
    }

    /// Reserve the next segment id for `kind`, append it to `live_segments`,
    /// and return the assigned id. Creates the kind entry on first use.
    /// Refuses a kind of the other key class ([`IndexError::WrongKindClass`]) —
    /// the per-kind split is enforced here, never trusted to convention.
    pub fn append_segment(&mut self, kind: ContentKind) -> Result<u32, IndexError> {
        self.check_kind_class(kind)?;
        Ok(self.kind_mut_or_default(kind).append_segment())
    }

    /// Max-merge an advisory ingest cursor for `kind` (class-guarded like
    /// [`Self::append_segment`]). Returns whether the recorded cursor advanced.
    pub fn merge_ingest_cursor(
        &mut self,
        kind: ContentKind,
        cursor: u64,
    ) -> Result<bool, IndexError> {
        self.check_kind_class(kind)?;
        if let Some(idx) = self.ingest_cursors.iter().position(|(k, _)| *k == kind) {
            if cursor > self.ingest_cursors[idx].1 {
                self.ingest_cursors[idx].1 = cursor;
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            self.ingest_cursors.push((kind, cursor));
            Ok(true)
        }
    }

    /// The recorded advisory ingest cursor for `kind`, if any builder has set one.
    pub fn ingest_cursor(&self, kind: ContentKind) -> Option<u64> {
        self.ingest_cursors
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, c)| *c)
    }

    /// Record the advisory corpus marker for `kind` (class-guarded like
    /// [`Self::append_segment`]). Returns whether the stored marker changed.
    ///
    /// **Last-writer-wins, deliberately** — see the field's docs: a marker names
    /// which corpus was staged, and the builder writing it is the one that just
    /// staged it. Max-merging identities the way cursors merge positions would
    /// be meaningless, and picking the "larger" of two corpora would suppress a
    /// real change.
    pub fn set_corpus_marker(
        &mut self,
        kind: ContentKind,
        marker: Vec<u8>,
    ) -> Result<bool, IndexError> {
        self.check_kind_class(kind)?;
        if let Some(idx) = self.corpus_markers.iter().position(|(k, _)| *k == kind) {
            if self.corpus_markers[idx].1 == marker {
                return Ok(false);
            }
            self.corpus_markers[idx].1 = marker;
        } else {
            self.corpus_markers.push((kind, marker));
        }
        Ok(true)
    }

    /// The recorded advisory corpus marker for `kind`, if a builder has staged
    /// one. `None` means "no corpus of this kind has been staged by any builder
    /// this manifest has seen" — which a walk must read as *unknown*, and
    /// therefore as a reason to read the corpus, never as *unchanged*.
    pub fn corpus_marker(&self, kind: ContentKind) -> Option<&[u8]> {
        self.corpus_markers
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, m)| m.as_slice())
    }

    /// Move `seg_id` from `live_segments` to `tombstoned_segments` for
    /// `kind`. Returns `true` if the move happened, `false` if the id wasn't
    /// in `live_segments` (or the kind didn't exist). `next_seg_id` is never
    /// decreased.
    ///
    /// `tombstoned_segments` is kept sorted ascending so callers (Plan 5/8
    /// compaction) can binary-search for GC eligibility.
    pub fn tombstone_segment(&mut self, kind: ContentKind, seg_id: u32) -> bool {
        let Some(idx) = self.kinds.iter().position(|(k, _)| *k == kind) else {
            return false;
        };
        self.kinds[idx].1.tombstone_segment(seg_id)
    }
}
