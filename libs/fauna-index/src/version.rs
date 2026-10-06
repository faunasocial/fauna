//! At-rest format-version compatibility for `fauna-index` — the two-number scheme,
//! mirroring `fauna-mls/src/version.rs` (`mls.db`), `fauna-segment-store/src/version.rs`, and
//! the nest DB's `check_schema_compatibility`. Ratified in
//! `version-compatibility.md` § 2.2; this is the store that section's Dim 1
//! *Confirmations* paragraph names as **the last open at-rest hazard of this class**.
//!
//! # Versioned before the first write, not after
//!
//! Every sibling scheme was a *retrofit*: a store already held user bytes, so the
//! scheme had to be additive over a back-catalogue. This one is not. The index's only
//! writer — the nest-side `IndexRegistry::ingest_mail` — was **deleted** on
//! 2026-07-13 (it wrote *unsealed* segments), and its residue is
//! purged at every nest boot. Nothing writes this format today, so **not one byte of
//! it exists at rest anywhere**.
//!
//! That is precisely why the scheme lands *now*. The index is not staying empty: it
//! is built only at **capability positions** — a user client, or the MDA bridge
//! during a session (`content-index.md` § Where the index is built) — so it becomes a
//! *client* at-rest store, synced across a user's devices through the `__index` file
//! set. And a **breaking** manifest change is already ratified and pending: the
//! per-kind manifest split that lets the bridge open a mail/calendar manifest without
//! the cross-kind master key (`content-index.md` § Per-kind key split).
//!
//! So the choice was never "is the scheme worth it" but "before or after the format
//! goes live". Before, it is a field and a byte. After, it is a migration of user data
//! that no reader in the field can perform. The siblings landed "tolerate before any
//! bump"; this one gets the strictly stronger **version before any write**.
//!
//! # The two numbers
//!
//! - **`format_version`** — bumped on **every** format change, additive or not. The
//!   blob's "what shape am I" stamp.
//! - **`min_reader_version`** — the oldest binary `format_version` that can still
//!   safely read (and, for the manifest, *rewrite*) a blob this binary wrote. Bumped
//!   **only** on a non-additive change — the *contract* step, deferred to a major
//!   version per I3. Additive growth leaves it untouched, which is exactly what keeps
//!   an older build working against a newer blob (I2 backward-compat).
//!
//! # Why `u8` here, where the siblings use `u16`
//!
//! The seal framing carries its numbers as **single bytes in a binary AEAD header**
//! (`seal.rs`), so the physical medium caps them at 255 — ample, and a width the
//! serde-side manifest has no reason to disagree with. One width across the crate
//! means one verdict function serves both halves, which is what keeps the manifest and
//! the seal from drifting into two dialects of the same scheme (#3).

use crate::types::IndexError;

/// This binary's `fauna-index` at-rest format. Bump on **every** format change to the
/// manifest payload or either seal framing.
///
/// **v2 (2026-08-02, backend-2 rollout S1):** the S0-ratified per-kind key split —
/// the manifest carries its `class` (master vs mail/calendar, two files), mail/calendar
/// segments wrap their data keys under the [`crate::IndexSegmentKey`] instead of the
/// master key, and the advisory `ingest_cursors` field exists. Breaking (a v1 reader
/// consulting only `manifest.idx` would silently see no mail/calendar kinds), so the
/// floor below raised with it — free, because the raise landed before the first real
/// writer ever shipped.
///
/// **v3 (2026-08-10, the carrier ruling — `content-index.md` § Where the
/// index is built):** the stored `secondary_id` schema field (mail docs carry the
/// raw nest message id beside the ratified RFC `Message-ID` content id).
/// **Additive** — the field is appended last in schema build order and every
/// reader resolves fields by name, so a v2 binary reads an all-v3 slice without
/// noticing — hence the floor below stays at 2. What a v2 binary *cannot* do is
/// open a **mixed** v2+v3 segment set (`open_multi_segment` requires one schema),
/// which is why v3 builders tombstone every below-current-format segment at
/// resume rather than leaving a mixed slice on the rail (the ratified
/// tombstone-only retirement; the corpus re-stages from the next walk).
///
/// **v4 (2026-10-05, the engine ruling — `content-index.md` § Engine and shape,
/// *An engine bump is an index-format bump*):** Tantivy 0.22 → 0.26. The engine's own
/// segment format lives inside every segment and a 0.22 build cannot open a 0.26
/// segment, so this is **non-additive** and the floor below rises to 4 with it. No
/// migration: under the pre-baseline blank slate no segment or manifest exists at
/// rest, and a below-current segment met later is retired by the same tombstone-only
/// path v3 relies on.
pub const CURRENT_INDEX_FORMAT_VERSION: u8 = 4;

/// The oldest binary `format_version` that can still safely read a blob this binary
/// writes. Bump **only** on a breaking, non-additive change — per I3 a major-version
/// event (the v2 split predates the first write, the one window where a floor raise
/// costs nothing). Leaving it put while [`CURRENT_INDEX_FORMAT_VERSION`] grows is what
/// makes growth additive: an older build keeps reading (and re-writing) a newer blob.
///
/// Raised to 4 with the Tantivy 0.26 engine (v4): no older build can open a v4
/// segment, so letting one read a v4 manifest would only send it into the rebuild arm.
pub const MIN_READER_INDEX_FORMAT_VERSION: u8 = 4;

/// The reader floor can never exceed the writer version — a build that cannot read its
/// own output is incoherent, and [`check_index_format_compatibility`] would call every
/// blob it just sealed `Incompatible`. Enforced at compile time so raising the floor
/// without raising the writer fails the build, not a test run.
const _: () = assert!(MIN_READER_INDEX_FORMAT_VERSION <= CURRENT_INDEX_FORMAT_VERSION);

/// The pre-read (and, for the manifest, pre-**write**) verdict for a blob's recorded
/// `(format_version, min_reader_version)` against this binary's writer version
/// (`version-compatibility.md` § 2.2 table).
///
/// Three variants, matching the nest's and `mls.db`'s rather than the
/// segment store's two: the manifest is **rewritten wholesale** (every `append_segment`
/// / `tombstone_segment` re-emits the whole file), so "older blob we upgrade" and
/// "newer blob we merely tolerate" are genuinely different actions — the second must
/// **not** restamp the version down (§ 2.2, "Do not restamp down").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFormatVerdict {
    /// `file_v ≤ bin_v` — this binary is current with, or newer than, the blob. Read
    /// it; on rewrite, stamp forward.
    UpgradeOrCurrent,
    /// `file_v > bin_v` **and** `file_min ≤ bin_v` — the blob is newer than this binary
    /// but grew only *additively*, within our reader floor. Read it (I2
    /// backward-compat); on rewrite, keep its higher numbers, because the fields we
    /// cannot name round-trip blindly through the manifest's `extra` catch-all — so the
    /// blob we write back really is still that newer shape.
    NewerCompatible,
    /// `file_v > bin_v` **and** `file_min > bin_v` — the blob carries a **breaking**
    /// change this binary predates. Refuse honestly and touch nothing: never rewrite it,
    /// never "heal" it, never fail lazily at the next save (I1).
    Incompatible { file_v: u8, file_min: u8, bin_v: u8 },
}

/// Compare a blob's recorded `(file_v, file_min)` against this binary's writer version,
/// **without mutating anything**. Structurally identical to the nest's
/// `check_schema_compatibility` and the three sibling crates' so the schemes stay one
/// concept (#3).
pub fn check_index_format_compatibility(file_v: u8, file_min: u8, bin_v: u8) -> IndexFormatVerdict {
    if file_v <= bin_v {
        IndexFormatVerdict::UpgradeOrCurrent
    } else if file_min <= bin_v {
        IndexFormatVerdict::NewerCompatible
    } else {
        IndexFormatVerdict::Incompatible {
            file_v,
            file_min,
            bin_v,
        }
    }
}

/// The version pair stamped on one at-rest blob — a manifest payload or either seal
/// framing's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexFormatStamp {
    pub format_version: u8,
    pub min_reader_version: u8,
}

impl Default for IndexFormatStamp {
    fn default() -> Self {
        Self::current()
    }
}

impl IndexFormatStamp {
    /// This build's own stamp — what a blob written from scratch carries.
    pub const fn current() -> Self {
        Self {
            format_version: CURRENT_INDEX_FORMAT_VERSION,
            min_reader_version: MIN_READER_INDEX_FORMAT_VERSION,
        }
    }

    /// Read a stamp out of raw at-rest numbers, verbatim. A `0` in either slot is
    /// **not** normalized: it is a version no build writes, and [`Self::check`]
    /// refuses it. (Reading `0` as the baseline served only blobs written before the
    /// scheme — none of which exist — and was retired by the compat-remnant sweep,
    /// `version-compatibility.md` § Dimension 2, program 4.)
    pub fn from_raw(format_version: u8, min_reader_version: u8) -> Self {
        Self {
            format_version,
            min_reader_version,
        }
    }

    /// Whether either slot holds the `0` no build writes — an unstamped blob.
    pub fn is_unstamped(&self) -> bool {
        self.format_version == 0 || self.min_reader_version == 0
    }

    /// The verdict for reading (or rewriting) a blob carrying this stamp.
    pub fn verdict(&self) -> IndexFormatVerdict {
        check_index_format_compatibility(
            self.format_version,
            self.min_reader_version,
            CURRENT_INDEX_FORMAT_VERSION,
        )
    }

    /// Refuse with the typed [`IndexError::Incompatible`] if this blob is
    /// newer-breaking, and with [`IndexError::SchemaMismatch`] if it is unstamped (a
    /// `0` in either slot — [`Self::is_unstamped`]); otherwise `Ok`. **Call before any
    /// read of the body and before any write over the blob** — the whole point is that
    /// an incompatible blob is left byte-for-byte intact.
    pub fn check(&self) -> Result<(), IndexError> {
        if self.is_unstamped() {
            return Err(IndexError::SchemaMismatch(format!(
                "unstamped index blob (format_version={}, min_reader_version={}): \
                 version 0 is no build's stamp",
                self.format_version, self.min_reader_version
            )));
        }
        match self.verdict() {
            IndexFormatVerdict::UpgradeOrCurrent | IndexFormatVerdict::NewerCompatible => Ok(()),
            IndexFormatVerdict::Incompatible {
                file_v,
                file_min,
                bin_v,
            } => Err(IndexError::Incompatible {
                file_v,
                file_min,
                bin_v,
            }),
        }
    }

    /// The stamp to write when rewriting a blob that carried this one: `max` on both
    /// numbers, **never** a restamp down (§ 2.2).
    ///
    /// A build that legitimately rewrites a *newer-additive* manifest must keep the
    /// newer numbers — its `extra` catch-all round-trips the unknown fields, so the blob
    /// it writes back really is still that newer shape. Stamping our own lower version
    /// over it would be a lie the *next* build acts on: it would read a genuinely-v5
    /// manifest claiming to be v1.
    pub fn restamped(&self) -> Self {
        Self {
            format_version: self.format_version.max(CURRENT_INDEX_FORMAT_VERSION),
            min_reader_version: self.min_reader_version.max(MIN_READER_INDEX_FORMAT_VERSION),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_or_equal_blob_upgrades() {
        assert_eq!(
            check_index_format_compatibility(1, 1, 1),
            IndexFormatVerdict::UpgradeOrCurrent
        );
        assert_eq!(
            check_index_format_compatibility(2, 1, 7),
            IndexFormatVerdict::UpgradeOrCurrent
        );
    }

    /// Grew past us, but never raised the reader floor: I2 says read it — the manifest's
    /// `extra` catch-all carries the fields we cannot name.
    #[test]
    fn newer_additive_blob_is_tolerated() {
        assert_eq!(
            check_index_format_compatibility(2, 1, 1),
            IndexFormatVerdict::NewerCompatible
        );
        assert_eq!(
            check_index_format_compatibility(9, 3, 3),
            IndexFormatVerdict::NewerCompatible
        );
    }

    #[test]
    fn newer_breaking_blob_is_incompatible() {
        assert_eq!(
            check_index_format_compatibility(2, 2, 1),
            IndexFormatVerdict::Incompatible {
                file_v: 2,
                file_min: 2,
                bin_v: 1,
            }
        );
    }

    /// The cliff the account index paid for once already (§ 2.2 "Do not restamp down").
    #[test]
    fn restamp_never_lowers() {
        let newer = IndexFormatStamp {
            format_version: 5,
            min_reader_version: 1,
        };
        let out = newer.restamped();
        assert_eq!(
            out.format_version, 5,
            "must not restamp a v5 manifest down to v2"
        );
        assert_eq!(
            out.min_reader_version, MIN_READER_INDEX_FORMAT_VERSION,
            "a rewrite by this build emits this build's shape, so the floor rises to ours \
             (max), never falls below the file's own"
        );

        let older = IndexFormatStamp {
            format_version: 1,
            min_reader_version: 1,
        };
        assert_eq!(older.restamped(), IndexFormatStamp::current());
    }

    /// A `0` stamp — what a writer predating the scheme left in the reserved slot —
    /// is refused, never read as a baseline version. The pre-scheme normalization
    /// was retired by the compat-remnant sweep (`version-compatibility.md`
    /// § Dimension 2, program 4).
    #[test]
    fn a_zero_stamp_is_refused_not_normalized() {
        for (v, min) in [(0, 0), (CURRENT_INDEX_FORMAT_VERSION, 0), (0, 1)] {
            let stamp = IndexFormatStamp::from_raw(v, min);
            assert_eq!(stamp.format_version, v, "read verbatim");
            assert_eq!(stamp.min_reader_version, min, "read verbatim");
            assert!(
                matches!(stamp.check(), Err(IndexError::SchemaMismatch(_))),
                "({v}, {min}) is unstamped and must be refused"
            );
        }
    }

    /// No blob this codebase writes is unreadable by it.
    #[test]
    fn shipped_constants_are_self_compatible() {
        assert_eq!(
            IndexFormatStamp::current().verdict(),
            IndexFormatVerdict::UpgradeOrCurrent
        );
        assert!(IndexFormatStamp::current().check().is_ok());
    }

    /// The typed refusal is what makes an unreadable blob *actionable* — the caller must
    /// be able to tell "intact, this build is too old" from "corrupt".
    #[test]
    fn incompatible_blob_refuses_with_the_typed_error() {
        // Relative to the shipped constant, so bumping the format does not
        // silently turn this "future" blob into a current one (it did once:
        // the v3 bump caught a hardcoded 3 here).
        let future = IndexFormatStamp {
            format_version: CURRENT_INDEX_FORMAT_VERSION + 1,
            min_reader_version: CURRENT_INDEX_FORMAT_VERSION + 1,
        };
        let err = future
            .check()
            .expect_err("a newer-breaking blob must refuse");
        assert!(
            matches!(err, IndexError::Incompatible { .. }),
            "must be the typed Incompatible, not a generic Crypto/SchemaMismatch: {err}"
        );
    }
}
