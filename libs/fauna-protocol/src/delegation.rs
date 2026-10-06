//! WS-RPC payload types for the task-delegation heartbeat lease — the
//! `fauna.delegation.*` plane that makes `current_candidates` exactly-one-runner
//! at runtime (participants.md § Coordination primitive; wire + rationale
//! tracked internally).
//!
//! Two RPCs + one push (the push payload lives in `push_events.rs`, as every
//! push does):
//!
//! - `fauna.delegation.heartbeat` — a participant claims/renews the lease for a
//!   task kind. The nest is a **dumb per-actor in-memory last-writer-wins
//!   blackboard**: it always accepts, records `holder = caller`, and returns the
//!   post-write [`LeaseState`]. No CAS, no epoch, no rejection (participants.md
//!   :115 — the lease is advisory, never nest-authoritative). The *client's*
//!   `fauna_core::delegation::decide` gates whether to call this at all.
//! - `fauna.delegation.observe` — read the current per-kind lease snapshot
//!   (request/reply, not push-only: a watcher must catch a holder that silently
//!   went stale, and the e2e harness cannot receive Push frames).
//! - `fauna.delegation.lease_changed` (push) — a best-effort "re-observe this
//!   kind" nudge, fired when a heartbeat *changes* the holder; the observe poll
//!   is the correctness backstop.
//!
//! ## Conventions
//!
//! - The participant identity is `fauna_core::data::ParticipantRef` and the
//!   class `fauna_core::delegation::ParticipantClass`, reused verbatim from the
//!   at-rest `fauna.state.delegation` types (priority #3 — one participant shape
//!   across at-rest and wire; the slice-1 dag-cbor round-trip covers them).
//!   `ParticipantRef::Nest { actor_pubkey: [u8; 32] }` encodes the pubkey as a
//!   32-byte CBOR byte string, like every fixed-width byte field
//!   (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
//!   "Fixed-size byte arrays"), so a Python caller sends it as raw `bytes`.
//! - Every struct carries the mandatory `#[serde(flatten, default)] extra`
//!   forward-compat catch-all (the 2026-06-15 universal-`extra` sweep).
//! - Kind registry metadata lives in `kind.rs::register_delegation_kinds`; nest
//!   handler dispatch is `bins/fauna-nest/src/delegation_handlers.rs`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_cbor::Value;
use fauna_core::data::ParticipantRef;
use fauna_core::delegation::ParticipantClass;

/// `fauna.delegation.heartbeat` — claim or renew the lease for a task kind.
pub const KIND_HEARTBEAT: &str = "fauna.delegation.heartbeat";
/// `fauna.delegation.observe` — read the current per-kind lease snapshot.
pub const KIND_OBSERVE: &str = "fauna.delegation.observe";

// ── fauna.delegation.heartbeat ─────────────────────────────────────────────

/// Claim/renew the lease for `task_kind`. The caller asserts its own
/// `holder`/`holder_class` (all participants are the one authenticated actor's
/// own devices, so self-assertion carries no trust concern). The nest writes it
/// last-writer-wins and returns the post-write [`LeaseState`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatRequest {
    pub task_kind: String,
    pub holder: ParticipantRef,
    pub holder_class: ParticipantClass,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The lease state after applying the heartbeat (`holder = caller`,
/// `age_ms = 0`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatReply {
    pub lease: LeaseState,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.delegation.observe ───────────────────────────────────────────────

/// Read the current lease snapshot. `task_kinds` empty ⇒ every lease the
/// authenticated actor currently has a record for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ObserveRequest {
    #[serde(default)]
    pub task_kinds: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The current lease per requested kind **that has a record**. Kinds with no
/// lease are simply absent from `leases` (the observer treats absent = free).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ObserveReply {
    pub leases: Vec<LeaseState>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── the shared record ──────────────────────────────────────────────────────

/// The advisory lease record as the nest currently holds it, returned by both
/// RPCs. `age_ms` is nest-computed (`server_now - last_write`, one monotonic
/// clock) so the observer can compare it to
/// `fauna_core::delegation::LEASE_STALE_MS` without trusting any client clock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LeaseState {
    pub task_kind: String,
    pub holder: ParticipantRef,
    pub holder_class: ParticipantClass,
    pub age_ms: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    fn lease(kind: &str, holder: ParticipantRef, class: ParticipantClass, age: u64) -> LeaseState {
        LeaseState {
            task_kind: kind.to_string(),
            holder,
            holder_class: class,
            age_ms: age,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn heartbeat_request_round_trips_device_holder() {
        let req = HeartbeatRequest {
            task_kind: "backup-upload".into(),
            holder: dev("dev-a"),
            holder_class: ParticipantClass::PluggedInDesktop,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: HeartbeatRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn heartbeat_request_round_trips_nest_holder() {
        // The Nest variant (a [u8;32] → CBOR array) still round-trips in Rust,
        // even though slices 2-3 never send it over the wire.
        let req = HeartbeatRequest {
            task_kind: "content-rescore".into(),
            holder: ParticipantRef::Nest {
                actor_pubkey: [7u8; 32],
            },
            holder_class: ParticipantClass::AlwaysOnNest,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: HeartbeatRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn heartbeat_reply_round_trips() {
        let reply = HeartbeatReply {
            lease: lease(
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
                0,
            ),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: HeartbeatReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn observe_request_round_trips_and_defaults_empty() {
        let req = ObserveRequest {
            task_kinds: vec!["backup-upload".into(), "index".into()],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: ObserveRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
        // Empty ⇒ "all leases" — default round-trips too.
        let all = ObserveRequest::default();
        let bytes = encode_canonical(&all).unwrap();
        let back: ObserveRequest = decode(&bytes).unwrap();
        assert_eq!(back, all);
        assert!(back.task_kinds.is_empty());
    }

    #[test]
    fn observe_reply_round_trips_multiple_kinds() {
        let reply = ObserveReply {
            leases: vec![
                lease(
                    "backup-upload",
                    dev("dev-a"),
                    ParticipantClass::PluggedInDesktop,
                    1_500,
                ),
                lease(
                    "index",
                    dev("dev-b"),
                    ParticipantClass::PluggedInDesktop,
                    42,
                ),
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: ObserveReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn empty_observe_reply_round_trips() {
        let reply = ObserveReply::default();
        let bytes = encode_canonical(&reply).unwrap();
        let back: ObserveReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
        assert!(back.leases.is_empty());
    }
}
