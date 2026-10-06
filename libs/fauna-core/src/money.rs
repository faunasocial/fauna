//! Money units.
//!
//! The wire and every stored magnitude are denominated in **millisatoshis**;
//! every human-facing statement about them — a tip label, a tier's asking
//! price, a quota sentence — is phrased in **satoshis**. One factor converts
//! between them, and it lives here.
//!
//! It lives in `fauna-core` rather than beside any one consumer because
//! `fauna-core` is the only crate all of them can reach: `fauna-protocol`
//! (which re-exports it as `subscriptions::MSATS_PER_SAT`, the name the
//! payments plane already publishes) sits *above* `fauna-core`, so a copy
//! declared there — as one was, calling itself "the one place the sats↔msats
//! factor is written" — is unreachable from `fauna-core`'s own two copies and
//! could not have been that one place.

/// Millisatoshis per satoshi.
///
/// **The one home of this factor.** Read it; never write `1_000` (or `1000.0`)
/// again — a conversion factor that appears twice is a rounding bug waiting
/// for the two sites to disagree, and this one had four sites under two
/// spellings and two types before it was consolidated.
///
/// `u64` because every stored and wire magnitude is an integer count of
/// millisats. A `f64` consumer casts at the use site (`MSATS_PER_SAT as f64`)
/// rather than keeping a second float copy — the cast is exact for this value,
/// and it keeps the integer one authoritative.
pub const MSATS_PER_SAT: u64 = 1_000;
