//! `MailFloorMetadata` — the per-record floor blob the segment footer
//! carries for the mail kind.
//!
//! NOTE: `received_at`, `sender_domain`, `spam_disposition`,
//! `is_own_submission` are **also** mirrored into the
//! `segment_records` SQLite columns for SQL-queryable hot-path access
//! (per `docs/goal/architecture/message-segment-store.md` § What goes
//! in the mirror columns vs the segment footer's `floor_metadata`
//! blob). They appear here as well so the segment footer is the
//! authoritative source — recovery can rebuild the SQLite mirror by
//! walking segments.

use serde::{Deserialize, Serialize};

pub const MAIL_FLOOR_FORMAT_VERSION: u16 = 1;

/// A record's role in the continuation-record scheme
/// (`message-segment-store.md` § Continuation records). A normal message is one
/// inline record (`NORMAL`). An over-cap body rests as N `PART` records + one
/// `HEAD`. The role rides the floor (segment footer — authoritative) and is
/// mirrored into `segment_records.continuation_role` for SQL-queryable reaping.
///
/// `#[serde(default)]` on the field decodes an absent key as `NORMAL`, so a
/// floor carrying no role key is treated as an ordinary inline record.
pub const CONTINUATION_ROLE_NORMAL: u8 = 0;
/// A **part** record — a raw ciphertext range of the one sealed body. Carries
/// no placement / projection row; only the head's serve path reads it, and the
/// age-watermarked headless-part reaper reclaims it if its head is gone.
pub const CONTINUATION_ROLE_PART: u8 = 1;
/// A **head** record (envelope v3) — the commit point pinning the ordered part
/// CID list. Placed + served like a normal record.
pub const CONTINUATION_ROLE_HEAD: u8 = 2;

/// One factor's row on the scoring-metadata bus, as carried in the segment
/// footer. Mirrors `fauna_core::scoring::ScoreEntry` field-for-field — the
/// `segments-codec` feature deliberately pulls only `fauna-cbor` (WASM-safe,
/// no crypto deps), so the canonical type can't be imported here; the same
/// mirror reason as `fauna-protocol`'s `RspamdScore`. Wire/at-rest shape and
/// field semantics are owned by the canonical type
/// (`content-scoring.md` § The scoring-metadata bus).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorScoreEntry {
    pub factor: String,
    /// Integer per-mille (milli-int, signed); no floats in dag-cbor.
    pub score: i64,
    /// Model-authority tier 1/2/3 (user / admin / community).
    pub tier: u8,
    /// The scorer's version watermark (drives the re-score obligation).
    pub scorer_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailFloorMetadata {
    pub format_version: u16,
    /// Server-assigned receive time (epoch milliseconds).
    pub received_at: i64,
    /// Inner-message timestamp (Date: header, epoch seconds).
    pub timestamp: i64,
    /// Total ciphertext-envelope size (= `MailRecordEnvelope`-encoded
    /// length on disk; the CARv2 block length — the `segment_records.byte_length` column was dropped).
    pub ciphertext_size: u32,
    /// Envelope-FROM domain (RFC 5321 `MAIL FROM`).
    pub sender_domain: String,
    /// Mirrored to segment_records.
    pub spam_disposition: String,
    /// Mirrored to segment_records.
    pub is_own_submission: bool,
    pub spf: String,
    pub dkim: String,
    pub dmarc: String,
    pub dmarc_policy: String,
    pub arc: String,
    pub spam_score: u32,
    /// Per-actor monotonic relay/replay cursor, mirrored into the
    /// `segment_records.seq` column (sibling of `ConvFloorMetadata.seq`).
    /// Stamped by `segments::mail::append_record` at append time and read
    /// back by compaction so the cursor survives a segment rewrite — keeping
    /// the "floor is authoritative; the mirror is rebuildable" invariant true
    /// for mail's seq. `#[serde(default)]`: a record with no
    /// `seq` key decodes as `0` (a fresh
    /// relay nest starts empty, so this is benign).
    #[serde(default)]
    pub seq: i64,
    /// The uniform scoring-metadata bus rows for this record — the footer is
    /// authoritative, so recovery can rebuild the nest's `content_scores`
    /// mirror by walking segments (same rationale as the mirrored columns
    /// above). `#[serde(default)]`: a floor with no
    /// `scores` key decodes as empty.
    #[serde(default)]
    pub scores: Vec<FloorScoreEntry>,
    /// Canonical 32-byte report-hash (`report-sharing.md` § Content
    /// identity), computed at the MTA perimeter pre-seal and mirrored into
    /// `segment_records.report_hash` — the footer is authoritative, so
    /// recovery can rebuild the mirror column by walking segments. Empty =
    /// absent (an own-submission copy the perimeter
    /// didn't hash). `#[serde(default)]`: a floor with
    /// no key decodes as empty.
    #[serde(with = "serde_bytes", default)]
    pub report_hash: Vec<u8>,
    /// When **this nest** stored this record (epoch milliseconds), as opposed to
    /// when the message was *received* by whoever first got it.
    ///
    /// The two are the same at the origin nest and **differ at a relay
    /// destination**: `received_at` is the message's own receive time and is
    /// forwarded verbatim (it drives INTERNALDATE and the co-location bucket, so
    /// it must survive the relay), while this is always local and always now.
    /// `segments::mail::append_sealed_record` overwrites whatever a peer sent
    /// here, exactly as it does for [`Self::seq`] — a relayed value is
    /// meaningless locally and, for the reaper below, dangerous.
    ///
    /// The headless-part reaper's grace keys on **this**, never on `received_at`.
    /// Keying it on `received_at` was a data-loss bug: relayed historical mail
    /// (a backfill/catch-up relay carries a `received_at` of days ago) arrives
    /// already past the grace, so a compaction pass landing between a family's
    /// parts and its head tombstones parts that are still needed — and the source
    /// has purged on ack. Mirrored into `segment_records.stored_at` for the
    /// reaper's SQL, and carried through compaction like `seq`, so the "floor is
    /// authoritative; the mirror is rebuildable" invariant holds.
    ///
    /// `#[serde(default)]`: a floor with no key
    /// decodes as `0`, which means **unknown** — never "epoch".
    /// Readers must treat `0` as "no local storage time recorded" and refuse to
    /// age anything out on it (the mirror stores `NULL`), so a floor with no stamp can
    /// only ever leak a part, never lose one.
    #[serde(default)]
    pub stored_at: i64,
    /// This record's role in the continuation-record scheme — one of
    /// [`CONTINUATION_ROLE_NORMAL`] / [`CONTINUATION_ROLE_PART`] /
    /// [`CONTINUATION_ROLE_HEAD`]. Mirrored into
    /// `segment_records.continuation_role`. `#[serde(default)]`:
    /// a floor with no key decodes as
    /// `NORMAL` (an ordinary inline record).
    #[serde(default)]
    pub continuation_role: u8,
}

/// Catalog-aligned default: `format_version` is the current constant, every
/// payload field empty/zero. Exists so fixtures can be written as
/// `MailFloorMetadata { ..Default::default() }` — this type grows a field every
/// few sessions (`seq`, `scores`, `report_hash`, `stored_at`), and a hand-listed fixture
/// turns each additive growth into a `cfg(test)` compile break that
/// the merge gate (which runs no cargo tests) lands on `main` unnoticed.
impl Default for MailFloorMetadata {
    fn default() -> Self {
        Self {
            format_version: MAIL_FLOOR_FORMAT_VERSION,
            received_at: 0,
            timestamp: 0,
            ciphertext_size: 0,
            sender_domain: String::new(),
            spam_disposition: String::new(),
            is_own_submission: false,
            spf: String::new(),
            dkim: String::new(),
            dmarc: String::new(),
            dmarc_policy: String::new(),
            arc: String::new(),
            spam_score: 0,
            seq: 0,
            scores: Vec::new(),
            report_hash: Vec::new(),
            stored_at: 0,
            continuation_role: CONTINUATION_ROLE_NORMAL,
        }
    }
}

impl MailFloorMetadata {
    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        fauna_cbor::encode_canonical(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let f: MailFloorMetadata = fauna_cbor::decode_strict(bytes)?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MailFloorMetadata {
        MailFloorMetadata {
            format_version: MAIL_FLOOR_FORMAT_VERSION,
            received_at: 1_715_000_000_000,
            timestamp: 1_714_999_900,
            ciphertext_size: 4096,
            sender_domain: "example.com".to_string(),
            spam_disposition: "accept".to_string(),
            is_own_submission: false,
            spf: "pass".to_string(),
            dkim: "pass".to_string(),
            dmarc: "pass".to_string(),
            dmarc_policy: "reject".to_string(),
            arc: "pass".to_string(),
            spam_score: 12,
            seq: 7,
            scores: vec![FloorScoreEntry {
                factor: "spam".to_string(),
                score: 12,
                tier: 1,
                scorer_version: 1,
            }],
            report_hash: vec![0xAB; 32],
            // Deliberately different from `received_at`: this floor is a relayed
            // part, so the message was received long before this nest stored it.
            stored_at: 1_715_000_900_000,
            continuation_role: CONTINUATION_ROLE_PART,
        }
    }

    #[test]
    fn round_trip() {
        let f = sample();
        let bytes = f.encode().expect("encode");
        let f2 = MailFloorMetadata::decode(&bytes).expect("decode");
        assert_eq!(f, f2);
    }

    /// Every byte at rest in a CARv2 segment goes through one canonical encoder
    /// (`serialization.md:29`). serde_bare bytes fail `decode_strict`. Twin of
    /// `cal`/`card`'s guard; likely redundant here since `decode()` itself
    /// already calls `decode_strict`, kept for the same explicit-guard shape.
    #[test]
    fn floor_at_rest_is_canonical_dagcbor() {
        let bytes = sample().encode().expect("encode");
        fauna_cbor::decode_strict::<MailFloorMetadata>(&bytes)
            .expect("floor bytes must be canonical dag-cbor");
    }

    /// Every mail-specific `segment_records` column, and every payload field
    /// the footer alone carries, must have exactly one home across the three
    /// at-rest slots (floor / envelope / placement manifest), or snapshot
    /// restore silently drops it. Twin of `cal`/`card`'s partition pin, adapted
    /// to mail's shape: unlike calendar/contacts, mail has no per-kind bridge
    /// table (no `bridge_mail_messages`) — its truth lives in the segment
    /// footer, with a mail-sparse subset mirrored into `segment_records` for
    /// SQL-queryable hot-path access (message-segment-store.md § What goes in
    /// the mirror columns).
    #[test]
    fn floor_covers_every_immutable_non_payload_non_scope_column() {
        // Mirrored into segment_records (mail-sparse columns):
        //   received_at        -> floor (segment_records.received_at)
        //   sender_domain      -> floor (segment_records.sender_dom)
        //   spam_disposition   -> floor (segment_records.spam_disp)
        //   is_own_submission  -> floor (segment_records.is_own_submission)
        //   seq                -> floor (segment_records.seq)
        //   report_hash        -> floor (segment_records.report_hash)
        //   continuation_role  -> floor (segment_records.continuation_role)
        //   stored_at          -> floor (segment_records.stored_at)
        // Footer-only (no segment_records column):
        //   timestamp, ciphertext_size, spf, dkim, dmarc, dmarc_policy, arc,
        //   spam_score, scores -> floor
        //   encrypted_body / encrypted_index_hint / encrypted_fauna_ext -> envelope
        // Allocated by the placement journal, not the floor (see
        // floor_carries_no_sql_allocated_state below):
        //   uid, modseq, flags -> placement manifest (MailPlacementRecord::Append)
        // actor_id -> segment scope (the __mail/<actor_hex> dir); kind/segment_id/
        // bucket/record_cid/tombstoned/changed_seq are kind-agnostic segment-store
        // bookkeeping, not part of this message-shaped partition.
        let f = sample();
        let rebuilt = MailFloorMetadata::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(rebuilt.received_at, 1_715_000_000_000);
        assert_eq!(rebuilt.timestamp, 1_714_999_900);
        assert_eq!(rebuilt.ciphertext_size, 4096);
        assert_eq!(rebuilt.sender_domain, "example.com");
        assert_eq!(rebuilt.spam_disposition, "accept");
        assert!(!rebuilt.is_own_submission);
        assert_eq!(rebuilt.spf, "pass");
        assert_eq!(rebuilt.dkim, "pass");
        assert_eq!(rebuilt.dmarc, "pass");
        assert_eq!(rebuilt.dmarc_policy, "reject");
        assert_eq!(rebuilt.arc, "pass");
        assert_eq!(rebuilt.spam_score, 12);
        assert_eq!(rebuilt.seq, 7);
        assert_eq!(rebuilt.scores, f.scores);
        assert_eq!(rebuilt.report_hash, vec![0xABu8; 32]);
        assert_eq!(rebuilt.stored_at, 1_715_000_900_000);
        assert_eq!(rebuilt.continuation_role, CONTINUATION_ROLE_PART);
    }

    /// The floor must stay encodable before the record is placed — no field
    /// the placement journal allocates may creep back into it. Twin of
    /// `cal`/`card`'s guard, adapted to mail's split: a *placement journal*
    /// entry, not a SQL transaction, allocates mail's ordering state
    /// (`uid`/`modseq`/`flags`, `MailPlacementRecord::Append` — see the module
    /// docs above), but the same "allocated state never rides the floor"
    /// invariant applies.
    #[test]
    fn floor_carries_no_sql_allocated_state() {
        // Canonical dag-cbor encodes struct fields as text keys, so the field
        // name appears verbatim in the encoded bytes.
        let encoded = sample().encode().expect("encode");
        for forbidden in ["uid", "modseq", "flags"] {
            assert!(
                !encoded
                    .windows(forbidden.len())
                    .any(|w| w == forbidden.as_bytes()),
                "{forbidden} is allocated by the placement journal and belongs \
                 to MailPlacementRecord::Append, not the content floor (see \
                 module docs)"
            );
        }
    }

    #[test]
    fn decodes_v1_floor_without_seq_as_zero() {
        // A floor with no `seq` key (the key simply absent
        // from the canonical map) must still decode, with seq defaulting to 0
        // via `#[serde(default)]`. Encode the seq-less shape verbatim (no seq
        // field) and decode it into the grown struct.
        #[derive(serde::Serialize)]
        struct MailFloorMetadataV1 {
            format_version: u16,
            received_at: i64,
            timestamp: i64,
            ciphertext_size: u32,
            sender_domain: String,
            spam_disposition: String,
            is_own_submission: bool,
            spf: String,
            dkim: String,
            dmarc: String,
            dmarc_policy: String,
            arc: String,
            spam_score: u32,
        }
        let v1 = MailFloorMetadataV1 {
            format_version: MAIL_FLOOR_FORMAT_VERSION,
            received_at: 1_715_000_000_000,
            timestamp: 1_714_999_900,
            ciphertext_size: 4096,
            sender_domain: "example.com".to_string(),
            spam_disposition: "accept".to_string(),
            is_own_submission: false,
            spf: "pass".to_string(),
            dkim: "pass".to_string(),
            dmarc: "pass".to_string(),
            dmarc_policy: "reject".to_string(),
            arc: "pass".to_string(),
            spam_score: 12,
        };
        let bytes = fauna_cbor::encode_canonical(&v1).expect("encode v1");
        let decoded = MailFloorMetadata::decode(&bytes).expect("decode v1 into grown struct");
        assert_eq!(decoded.seq, 0, "missing seq key defaults to 0");
        assert_eq!(decoded.sender_domain, "example.com");
        assert!(
            decoded.scores.is_empty(),
            "missing scores key defaults to empty"
        );
        assert!(
            decoded.report_hash.is_empty(),
            "missing report_hash key defaults to empty"
        );
        assert_eq!(
            decoded.continuation_role, CONTINUATION_ROLE_NORMAL,
            "missing continuation_role key defaults to NORMAL"
        );
        assert_eq!(
            decoded.stored_at, 0,
            "missing stored_at key defaults to 0 = UNKNOWN (never epoch): the \
             headless-part reaper must refuse to age a record out on it"
        );
    }

    #[test]
    fn continuation_role_round_trips() {
        for role in [
            CONTINUATION_ROLE_NORMAL,
            CONTINUATION_ROLE_PART,
            CONTINUATION_ROLE_HEAD,
        ] {
            let f = MailFloorMetadata {
                continuation_role: role,
                ..Default::default()
            };
            let bytes = f.encode().expect("encode");
            assert_eq!(
                MailFloorMetadata::decode(&bytes)
                    .expect("decode")
                    .continuation_role,
                role
            );
        }
    }
}
