//! The mail-auth verdicts (SPF, DKIM, DMARC, ARC) — **the one definition**.
//!
//! These types sit at a three-way boundary, which is why they live here rather
//! than in either crate that uses them:
//!
//! - **Produced** by [`fauna_mail::auth::verify_inbound`], which wraps
//!   `mail-auth`, and exported over UniFFI so the Go MTA can build a verdict set
//!   at the SMTP perimeter (`bins/fauna-bridges/internal/mta`).
//! - **Carried** on the `fauna.bridges.mail.ingest_inbound` WS-RPC request and
//!   re-exported by `fauna_protocol::bridge_routing`, the L3 transport crate.
//! - **Consumed** nest-side, flattened to RFC strings for storage
//!   (`bins/fauna-nest/src/bridge_routing_handlers.rs`).
//!
//! Until 2026-08-17 the producer and the wire each carried a hand-mirrored copy,
//! and their agreement was pinned only by CBOR round-trip tests — a test that
//! could only ever notice drift *after* someone wrote it, for a wire shape whose
//! two ends sit in different languages. Both crates now re-export these
//! definitions, so every path (`fauna_mail::auth::DkimVerdict`,
//! `fauna_protocol::bridge_routing::DkimVerdict`) names one type and drift is
//! unrepresentable. Pinned by `the_verdict_types_have_exactly_one_definition`
//! in `libs/fauna-mail/tests/auth_tests.rs` — the only crate that can see both
//! paths, since `fauna-protocol` must not depend on `fauna-mail`.
//!
//! **`fauna-core` rather than `fauna-protocol`** (which the retired
//! `TODO: unify these definitions` comment proposed): protocol carries no UniFFI
//! surface at all, by design, and these types must be exported to their Go
//! producer. Putting them here needs no new UniFFI face on the transport crate
//! and no new generated Go module — `fauna_core` is already in the binding.
//!
//! ## Wire shape — do not change without a major bump
//!
//! The four verdict enums are **adjacently tagged**: `{"kind":"<variant>"}` for
//! unit variants, `{"kind":"<variant>","data":{…}}` for struct variants, every
//! name snake_case. That uniformity is what lets the Go mirror
//! (`internal/wsrpc/methods.go::AuthVerdicts`) model every variant as a struct
//! with a `Kind string` plus an optional payload. [`DmarcPolicy`] is the one
//! exception — a bare snake_case string, because it appears only as the
//! `policy` field of [`DmarcVerdict::Fail`] and the `{"kind":…}` ceremony would
//! be noise there.
//!
//! Each verdict's `None` is `#[default]`, so `AuthVerdicts::default()` is the
//! RFC-canonical "no determination made" set and fixtures pick it up with
//! `..Default::default()` instead of listing every field. See the note on
//! [`AuthVerdicts`] for what that does — and does not — buy on the wire.
//!
//! ## Adding a variant — two tests will stop you, and that is the design
//!
//! The Go MTA translates these onto the wire mirror by hand
//! (`internal/mailfauna/mailfauna.go`'s `*ToWire` switches), because UniFFI's Go
//! enums do not serialize to the adjacently-tagged map nest decodes. Nothing
//! joined the two until 2026-08-18, so a variant added here compiled everywhere
//! and the Go switch simply had no arm for it.
//!
//! Now a variant added here reds, in order:
//!
//! 1. `libs/fauna-core/tests/go_wire_variant_contract.rs` — it fails to
//!    *compile* (its per-enum lists end in an exhaustive `match`), then reds
//!    with the exact bytes for the `go-wire-variants.json` fixture;
//! 2. `internal/wsrpc/go_wire_variant_contract_test.go` — reds until the
//!    ToWire switch grows the arm, and checks the arm emits the name for *that*
//!    variant rather than merely some name in the set.
//!
//! ⚠ **`None` is not a safe fallback here.** On an authentication verdict it is
//! the most permissive point of the lattice — RFC 7208/6376/7489 read it as "no
//! policy published / no determination made" — so emitting it for a variant the
//! translator could not map asserts something false and permissive. The Go
//! `default:` arms therefore land on `TempError` ("could not determine"), ruled
//! 2026-08-18 with every consumer traced: nest flattens these to RFC strings and
//! *records* them, and no refusal decision rides on the wire mirror. Keep a
//! `TempError` on every verdict you add — the Go pin resolves its fallback
//! through it. [`DmarcPolicy`] is the deliberate exception (it is what the domain
//! *published*, not a determination, and RFC 7489 gives it no error member).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DkimVerdict {
    #[default]
    None,
    Pass,
    Fail {
        reason: String,
    },
    Neutral,
    PermError,
    TempError,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SpfVerdict {
    #[default]
    None,
    Pass,
    Fail,
    SoftFail,
    Neutral,
    PermError,
    TempError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum DmarcPolicy {
    None,
    Quarantine,
    Reject,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DmarcVerdict {
    #[default]
    None,
    Pass,
    Fail {
        policy: DmarcPolicy,
    },
    PermError,
    TempError,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ArcVerdict {
    #[default]
    None,
    Pass,
    Fail,
    PermError,
    TempError,
}

/// The four verdicts as one set.
///
/// ⚠ `deny_unknown_fields` is carried over **verbatim** from both prior copies,
/// which each had it; the unification is wire-neutral by construction
/// and deliberately did not revisit it. Read it for what it is rather than as a
/// ratified evolution posture: it means a *newer* sender that adds a fifth
/// verdict field (a BIMI verdict, say) is **rejected outright** by an older
/// reader, rather than having the unknown field ignored — which is the opposite
/// of the additive-everywhere rule in
/// `docs/goal/architecture/version-compatibility.md`. Nothing adds a field
/// today, so nothing is broken today; whoever adds the fifth verdict must
/// settle that first, and flipping the attribute is itself a wire-behavior
/// change, not a cleanup. That warning lives here, on the type, so the author
/// who adds the fifth verdict meets it at the definition they are editing.
///
/// The `Default` derives are about *construction* churn, not wire compat: an
/// all-`None` set is the RFC-canonical "no determination made" value, so
/// fixtures pick it up with `..Default::default()`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[serde(deny_unknown_fields)]
pub struct AuthVerdicts {
    pub dkim: DkimVerdict,
    pub spf: SpfVerdict,
    pub dmarc: DmarcVerdict,
    pub arc: ArcVerdict,
}
