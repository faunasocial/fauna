//! Alias-policy resolution-order defaults — **the one definition**.
//!
//! These four values are **produced** as `fauna_mail::aliases`'s A3
//! Bucket-C constants (neither projected to the bridge nor admin-writable
//! today — enforced here until the inbound-perimeter policy write-path wires
//! the real knobs, `mail-aliases.md` § Kind 1/2/3) and **carried** on the
//! wire as `fauna_protocol::bridge_routing::AliasPolicy`'s `Default` impl,
//! which used to hand-copy all four literals with a `// Mirrors
//! fauna_mail::aliases::{…} (no fauna-mail dep here)` comment — the mirror
//! was named but never built, because `fauna-protocol` must not depend on
//! `fauna-mail` (`fauna-mail` depends on `fauna-protocol`, not the other way
//! round). Living here, both crates re-export instead of copying — the same
//! shape [`crate::mail_auth`] and [`crate::mail_scan`] already established
//! for the verdict/scan-result types.

/// Per-account exact-alias cap default (`mail.account.exact_aliases_max`,
/// Tier-2 in `mail-aliases.md` § Kind 1 Exact `:33`). Hardcoded until the
/// inbound-perimeter policy write-path lands the admin knob (the A3 audit
/// placed it in Bucket C — not yet projected nor admin-writable). The cap is
/// **per actor across all local domains** (`mail-aliases.md:287`).
pub const EXACT_ALIASES_MAX_DEFAULT: u32 = 20;

/// The default uncircumventable reserved local-parts (lowercase), per
/// `mail-aliases.md` § Reserved local-parts `:259-265` + the architectural
/// rule `:312`. User-tier alias creation may never claim one of these.
///
/// Admin can extend the set via `mail.inbound.reserved_local_parts` (Tier-2)
/// — that knob is not wired yet (A3 Bucket C), so this default is the live
/// reservation set. `unsubscribe@` is intentionally **not** here: it routes
/// `unsubscribe+<token>@` at envelope time via a different mechanism
/// (`mail-mass-mailing.md` § Reserved local-part), not as a claimable
/// local-part.
pub const DEFAULT_RESERVED_LOCAL_PARTS: &[&str] = &[
    "postmaster",
    "abuse",
    "noc",
    "security",
    "dmarc-report",
    "tlsrpt",
];

/// Resolution-order feature defaults — the Tier-2 admin knobs
/// `mail.inbound.subaddressing_enabled` / `mail.inbound.wildcard_prefix_enabled`
/// (`mail-aliases.md` § Kind 2 `:48`, § Kind 3 `:64`). The A3 coverage audit
/// placed both in **Bucket C** (neither projected to the bridge nor
/// admin-writable today), so — exactly like [`EXACT_ALIASES_MAX_DEFAULT`] —
/// the resolver enforces the spec default as a constant until the
/// inbound-perimeter policy write-path track wires the knob.
pub const SUBADDRESSING_ENABLED_DEFAULT: bool = true;
pub const WILDCARD_PREFIX_ENABLED_DEFAULT: bool = true;
