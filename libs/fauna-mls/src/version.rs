//! At-rest schema-version compatibility for `mls.db` — the two-number scheme,
//! mirroring the nest DB's `(schema_version, min_reader_version)` verdict
//! (`check_schema_compatibility` in `bins/fauna-nest/src/db/migrations.rs`) and
//! the segment store's on-disk twin (`fauna-segment-store/src/version.rs`).
//! Ratified in `version-compatibility.md` § 2.2; this store is the Dim 1 gap
//! that section's client half names.
//!
//! `mls.db` holds the user's **irrecoverable** MLS state — group state, the
//! signing key, key packages, sequence counters. Only the client's own copy can
//! open its conversations (the nest side is ciphertext), so misreading it is
//! not a degraded experience, it is permanent loss of every conversation. Hence
//! the two numbers:
//!
//! - **`schema_version`** — bumped on **every** schema change, additive or not.
//!   The database's "what shape am I" stamp.
//! - **`min_reader_version`** — the oldest binary `schema_version` that can
//!   still safely **operate** a database this binary wrote. Bumped **only** on a
//!   non-additive/breaking change (the *contract* step, deferred to a major
//!   version per I3). Additive growth leaves it untouched — that is precisely
//!   what keeps an older build working against a newer database (I2).
//!
//! Both are baseline `1` today (no breaking change has shipped), so
//! [`SchemaVerdict::Incompatible`] is dormant by construction. The stamp, the
//! tolerant read, and the reconcile exist *now* so that a future bump is
//! survivable by every binary built from this commit onward — "tolerate before
//! any bump", exactly as the DB and segment-store schemes landed before any
//! breaking change of their own.

use thiserror::Error;

/// This binary's `mls.db` schema. Bump on **every** schema change (a new table,
/// a new column — additive or not).
pub const CURRENT_SCHEMA_VERSION: u16 = 1;

/// The oldest binary `schema_version` that can still safely operate a database
/// this binary writes. Bump **only** on a breaking, non-additive change — and
/// per I3 that is a major-version event. Leaving it at `1` while
/// [`CURRENT_SCHEMA_VERSION`] grows is what makes growth additive.
pub const MIN_READER_SCHEMA_VERSION: u16 = 1;

/// The reader floor can never exceed the writer version — a build that cannot
/// read its own output is incoherent, and `check_schema_compatibility` would
/// call every database it just wrote `Incompatible`. Enforced at compile time so
/// that raising [`MIN_READER_SCHEMA_VERSION`] without also raising
/// [`CURRENT_SCHEMA_VERSION`] fails the build rather than a test run.
const _: () = assert!(MIN_READER_SCHEMA_VERSION <= CURRENT_SCHEMA_VERSION);

/// The open-time verdict for a database's recorded
/// `(schema_version, min_reader_version)` against this binary's
/// [`CURRENT_SCHEMA_VERSION`] (`version-compatibility.md` § 2.2 table).
///
/// Three variants, matching the nest's `SchemaVerdict` rather than the segment
/// store's two: like the nest — and unlike a plain at-rest file read — opening
/// this store *mutates* it (the reconcile adds missing columns, then restamps),
/// so "older database we upgrade" and "newer database we merely tolerate" are
/// genuinely different actions, not one `Readable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaVerdict {
    /// `db_v ≤ bin_v` — this binary is current with, or newer than, the
    /// database. Reconcile any missing additive columns and restamp forward.
    UpgradeOrCurrent,
    /// `db_v > bin_v` **and** `db_min ≤ bin_v` — the database is newer than this
    /// binary but grew only *additively*, within our reader floor. Operate
    /// normally (I2 backward-compat); the extra columns are simply unread. The
    /// restamp declines to lower the version — see `record_schema_meta`.
    NewerCompatible,
    /// `db_v > bin_v` **and** `db_min > bin_v` — the database carries a
    /// **breaking** change this binary predates. Refuse honestly and touch
    /// nothing: never migrate it, never rewrite it, never fail lazily at the
    /// first insert (I1).
    Incompatible { db_v: u16, db_min: u16, bin_v: u16 },
}

/// Returned by `SqliteStorage::open` when `mls.db` carries a breaking schema
/// change this binary predates ([`SchemaVerdict::Incompatible`]).
///
/// It is **not** a generic open failure: callers downcast it
/// (`err.downcast_ref::<SchemaIncompatible>()`) to tell "this app is too old for
/// its own MLS state — update it, and the conversations are all still there"
/// apart from "the store is corrupt". Mirrors the nest's `SchemaIncompatible`,
/// which the boot path downcasts to serve degraded rather than crash-loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "incompatible MLS state database: it was written by a newer build \
     (schema_version={db_v}, min_reader_version={db_min}) that requires a build at \
     schema_version >= {db_min}, but this build is at schema_version {bin_v} \
     — this app must be updated; the MLS state is intact and untouched"
)]
pub struct SchemaIncompatible {
    pub db_v: u16,
    pub db_min: u16,
    pub bin_v: u16,
}

/// Compare a database's recorded `(db_v, db_min)` against this binary's writer
/// version, **without mutating anything**. Structurally identical to the nest's
/// `check_schema_compatibility` so the two stay one concept (#3).
pub fn check_schema_compatibility(db_v: u16, db_min: u16, bin_v: u16) -> SchemaVerdict {
    if db_v <= bin_v {
        SchemaVerdict::UpgradeOrCurrent
    } else if db_min <= bin_v {
        SchemaVerdict::NewerCompatible
    } else {
        SchemaVerdict::Incompatible {
            db_v,
            db_min,
            bin_v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_or_equal_db_upgrades() {
        assert_eq!(
            check_schema_compatibility(1, 1, 1),
            SchemaVerdict::UpgradeOrCurrent
        );
        assert_eq!(
            check_schema_compatibility(2, 1, 7),
            SchemaVerdict::UpgradeOrCurrent
        );
    }

    #[test]
    fn newer_additive_db_is_tolerated() {
        // Grew past us, but never raised the reader floor: I2 says operate.
        assert_eq!(
            check_schema_compatibility(2, 1, 1),
            SchemaVerdict::NewerCompatible
        );
        assert_eq!(
            check_schema_compatibility(9, 3, 3),
            SchemaVerdict::NewerCompatible
        );
    }

    #[test]
    fn newer_breaking_db_is_incompatible() {
        assert_eq!(
            check_schema_compatibility(2, 2, 1),
            SchemaVerdict::Incompatible {
                db_v: 2,
                db_min: 2,
                bin_v: 1,
            }
        );
    }

    /// The dormancy check: while both constants sit at the baseline, no
    /// database this codebase can write is unreadable by it.
    #[test]
    fn shipped_constants_are_self_compatible() {
        assert_eq!(
            check_schema_compatibility(
                CURRENT_SCHEMA_VERSION,
                MIN_READER_SCHEMA_VERSION,
                CURRENT_SCHEMA_VERSION
            ),
            SchemaVerdict::UpgradeOrCurrent
        );
        // (`MIN_READER <= CURRENT` is enforced at compile time — see the
        // `const _` assertion above.)
    }
}
