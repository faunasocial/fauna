//! The payload of a carrying unknown arm — a value of an enum this build does
//! not name, kept whole so a reader that writes the record back out re-emits
//! exactly what it read (`docs/goal/architecture/transport.md` § Schema and
//! forward-compat discipline → *Rule 3 in full*: an enum with data variants is
//! open and carrying wherever a reader can write the value back out).
//!
//! It lives in the codec crate rather than `fauna-core` so a lean crate that
//! links no `fauna-core` — `fauna-ipc`, which the Windows shell extension
//! loads into Explorer — spells its carrying arms with the same type.
//! `fauna_core::carried` re-exports it beside the arm's rules.

use serde::{Deserialize, Serialize};

/// A value of an enum variant this build does not name, held undecoded.
///
/// Spelled as the owning enum's last variant,
/// `#[serde(untagged)] Unknown(CarriedValue)`: serde tries every named variant
/// first, and a value none of them accepts lands here. Re-encoding it through
/// the canonical encoder gives back the newer writer's canonical bytes.
///
/// What it means is the owning enum's to say: every `match` gives the arm the
/// most restrictive known behaviour or stricter — it never grants and never
/// deletes, no UI offers it, and no build writes one except to pass a carried
/// value through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CarriedValue(pub crate::Value);

/// `Eq` holds: the only value `PartialEq` is not reflexive over is a NaN
/// float, which dag-cbor refuses to decode (`serialization.md` § Canonical
/// IPLD dag-cbor), so a carried value never holds one.
impl Eq for CarriedValue {}
