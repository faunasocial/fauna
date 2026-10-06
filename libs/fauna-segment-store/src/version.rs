//! At-rest format-version compatibility — the segment store's two-number
//! scheme, mirroring the nest DB's `(schema_version, min_reader_version)`
//! verdict (`check_schema_compatibility` in
//! `bins/fauna-nest/src/db/migrations.rs`; `version-compatibility.md` § 2.2).
//!
//! Each on-disk file (`Manifest`, `SegmentSidecar`) carries two numbers:
//! `format_version` — the writer's format — and `min_reader_format_version` —
//! the oldest binary `format_version` that can still safely **read** it, bumped
//! only on a breaking, non-additive change (the *contract* step, deferred to a
//! major version per I3). A binary that writes up to `bin_v` reads any file
//! whose `min_reader ≤ bin_v`: an older/equal file, or a newer file that grew
//! only *additively* (I2 backward-compat — an old binary tolerates data a newer
//! binary wrote). Only a newer file that raised its reader floor past this
//! binary (`min_reader > bin_v`) is a genuinely breaking format it cannot read —
//! an honest error, never a silent orphan or panic (I1).
//!
//! Both numbers are baseline `1` today (no breaking on-disk change has shipped),
//! so the `IncompatibleNewer` branch is dormant. The field + tolerant read exist
//! so that a future *additive* bump is readable by every binary built from this
//! commit onward — "tolerate before any bump", exactly as the DB scheme landed
//! before any breaking DB change.

/// Verdict for an on-disk `(format_version, min_reader_format_version)` against
/// this binary's writer `format_version` (`bin_v`). The segment store reads in
/// both `Readable` cases — unlike the DB's `SchemaVerdict`, which splits them
/// because it runs migrations on the older one; the at-rest read path has no
/// migration step, so the two readable cases collapse to one variant.
pub(crate) enum FormatVerdict {
    /// `file_v ≤ bin_v` (older/equal — understood) **or** `file_v > bin_v` and
    /// `file_min ≤ bin_v` (newer, but only additively). Read it.
    Readable,
    /// `file_v > bin_v` **and** `file_min > bin_v` — a breaking on-disk format
    /// this binary predates. Surface an honest mismatch; never orphan or panic.
    IncompatibleNewer {
        file_v: u16,
        file_min: u16,
        bin_v: u16,
    },
}

/// Compare an on-disk file's `(format_version, min_reader_format_version)`
/// against this binary's writer `format_version`. Structurally identical to
/// `check_schema_compatibility` (`migrations.rs`) so the at-rest and DB schemes
/// stay one concept (#3). A well-formed file has `file_min ≤ file_v`, so when
/// `file_v ≤ bin_v` the `file_min ≤ bin_v` arm also holds — both spell
/// `Readable`; the explicit `||` matches the DB's two readable branches.
pub(crate) fn check_format_compatibility(file_v: u16, file_min: u16, bin_v: u16) -> FormatVerdict {
    if file_v <= bin_v || file_min <= bin_v {
        FormatVerdict::Readable
    } else {
        FormatVerdict::IncompatibleNewer {
            file_v,
            file_min,
            bin_v,
        }
    }
}

/// `#[serde(default)]` for `min_reader_format_version`: a file written before
/// the field existed (`format_version` 1, no breaking change) reads as baseline
/// `1`. This is what keeps existing v1 files loadable — purely additive at-rest
/// evolution, since `decode_strict` does not `deny_unknown_fields`.
pub(crate) fn baseline_min_reader_version() -> u16 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_or_equal_file_is_readable() {
        // file_v ≤ bin_v — this binary understands it.
        assert!(matches!(
            check_format_compatibility(1, 1, 1),
            FormatVerdict::Readable
        ));
        assert!(matches!(
            check_format_compatibility(1, 1, 3),
            FormatVerdict::Readable
        ));
    }

    #[test]
    fn newer_additive_file_is_readable() {
        // file_v > bin_v but the reader floor is still within this binary.
        assert!(matches!(
            check_format_compatibility(2, 1, 1),
            FormatVerdict::Readable
        ));
        assert!(matches!(
            check_format_compatibility(9, 3, 3),
            FormatVerdict::Readable
        ));
    }

    #[test]
    fn newer_breaking_file_is_incompatible() {
        // file_v > bin_v AND file_min > bin_v — a breaking format we predate.
        match check_format_compatibility(2, 2, 1) {
            FormatVerdict::IncompatibleNewer {
                file_v,
                file_min,
                bin_v,
            } => {
                assert_eq!((file_v, file_min, bin_v), (2, 2, 1));
            }
            FormatVerdict::Readable => panic!("expected IncompatibleNewer"),
        }
    }
}
