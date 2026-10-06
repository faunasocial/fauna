//! Restore-replay helpers. Part of the IMAP/CalDAV restore design (tracked
//! internally). Each kind has a
//! `replay_<kind>_manifest_into_sqlite(tx, actor, &manifest)` entry
//! point that writes the compacted current state of the placement
//! manifest into the kind's bridge_* tables. Caller owns the SQLite
//! transaction and the pre-replay `DELETE` of existing rows.

pub mod cal;
pub mod card;
pub mod divergence;
pub mod mail;
