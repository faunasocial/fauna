//! Canonical handle-format validation — the single source of truth shared by
//! the nest and every app.
//!
//! A Fauna handle is the bare local part stored on the nest (the `@domain`
//! suffix a wizard handle may carry is only for nest *discovery* during
//! onboarding, never part of the stored handle — see
//! `docs/goal/behavior/onboarding.md`). [`validate_handle`] is the one place the
//! format rules live: the nest enforces it on every handle-bearing RPC
//! (account register, claim, invite, discovery, profile handle-change), and
//! clients call it pre-submit for instant, identical feedback (no per-app
//! re-implementation — `docs/goal/ui/settings.md` § Architectural rules:
//! "handle change run[s] identical validation across all 7 apps (shared
//! Rust)"). Mirrors the other shared presentation/validation contracts in this
//! crate (`fauna_protocol::spam`, `fauna_protocol::email`).
//!
//! WASM-safe and UniFFI-exportable: pure `&str` in, `Result<(), &'static str>`
//! out, no transport/runtime deps. Clients reach it natively (linux), via the
//! UniFFI face in `fauna-ffi` (apple/windows/android), or the wasm wrapper in
//! `fauna-wasm` (web).

/// Validate a handle against the canonical format rules. Returns a human-facing
/// error message (already terse enough to surface in a client `error-message`
/// element) when the handle is malformed.
///
/// Rules: 3–63 characters; lowercase ASCII alphanumerics and hyphens only; no
/// leading or trailing hyphen. (The `@` a wizard discovery-handle may carry is
/// rejected here — the stored handle is the bare local part.)
/// The handles no user may ever register: infrastructure mailbox names
/// (`postmaster`, `abuse`, …), deployment service names, and every reserved web
/// subdomain label (`fauna_core::web::RESERVED_SUBDOMAIN_LABELS` — kept a
/// subset by test, so a handle can never be minted that would later derive a
/// reserved web host).
///
/// One hard-coded list, shared by the nest's register/discovery/invite/
/// handle-change gates (`RegistrationConfig::default()`) and `fauna-router`'s
/// pre-flight checks. Nobody *chooses* this list — it is a correctness
/// constant, not a policy — so it is a Rust constant, never a config key, CLI
/// flag, or env var. (Both the nest's `--reserved-handle` flag and the
/// router's `[registration] reserved_handles` TOML key were exactly that
/// theatre, and the nest flag's plumbing silently replaced this list with the
/// flag's empty default on every production boot; deleted 2026-07-17.)
///
/// **Deliberate carve-out:** the one-time admin-claim ceremony
/// (`bins/fauna-nest/src/claim_core.rs::claim_admin_core`) does NOT enforce
/// this list — the box owner may claim `admin` (the ordinary expected
/// self-hosted handle) or any other reserved name. Self-inflicted only (no
/// other actor is affected), recoverable via handle-change, and web
/// derivation is independently blocked at consumption. Flagged, not silently
/// unified, by the security review.
pub const RESERVED_HANDLES: &[&str] = &[
    "admin",
    "postmaster",
    "abuse",
    "root",
    "www",
    "mail",
    "app",
    "mta-sts",
    "relay",
    // The SNI router L4-routes `pds.<domain>` to the ATProto PDS bridge, whose
    // service identity is `did:web:pds.<domain>` — so this label can never be a
    // user's. Must stay in lock-step with RESERVED_SUBDOMAIN_LABELS.
    "pds",
    "support",
    "help",
    "info",
    "security",
    "noreply",
    "system",
];

/// `true` when `handle` is on the [`RESERVED_HANDLES`] list. Case-insensitive:
/// stored handles are lowercase by [`validate_handle`], but pre-flight callers
/// (`fauna-router`) see raw client input.
pub fn is_reserved_handle(handle: &str) -> bool {
    RESERVED_HANDLES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(handle))
}

pub fn validate_handle(handle: &str) -> Result<(), &'static str> {
    if handle.len() < 3 {
        return Err("handle must be at least 3 characters");
    }
    if handle.len() > 63 {
        return Err("handle must be at most 63 characters");
    }
    if handle.starts_with('-') || handle.ends_with('-') {
        return Err("handle cannot start or end with a hyphen");
    }
    if !handle
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("handle must be lowercase alphanumeric or hyphens");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_handle;

    #[test]
    fn accepts_well_formed_handles() {
        for h in [
            "abc",
            "alice",
            "a1b2c3",
            "with-hyphen",
            "x".repeat(63).as_str(),
        ] {
            assert!(validate_handle(h).is_ok(), "expected {h:?} to be valid");
        }
    }

    #[test]
    fn rejects_too_short() {
        assert_eq!(
            validate_handle("ab"),
            Err("handle must be at least 3 characters")
        );
        assert_eq!(
            validate_handle(""),
            Err("handle must be at least 3 characters")
        );
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(64);
        assert_eq!(
            validate_handle(&long),
            Err("handle must be at most 63 characters")
        );
    }

    #[test]
    fn rejects_leading_or_trailing_hyphen() {
        assert_eq!(
            validate_handle("-abc"),
            Err("handle cannot start or end with a hyphen")
        );
        assert_eq!(
            validate_handle("abc-"),
            Err("handle cannot start or end with a hyphen")
        );
    }

    #[test]
    fn rejects_disallowed_characters() {
        // Uppercase, '@' (the discovery suffix is not part of a stored handle),
        // dots, underscores, spaces, unicode.
        for h in ["Alice", "alice@nest.example", "a.b", "a_b", "a b", "café"] {
            assert_eq!(
                validate_handle(h),
                Err("handle must be lowercase alphanumeric or hyphens"),
                "expected {h:?} to be rejected for disallowed characters",
            );
        }
    }

    #[test]
    fn every_reserved_subdomain_label_is_a_reserved_handle() {
        // a handle that is a reserved web subdomain label must be
        // unregistrable, or registering it would later derive a reserved web
        // host. The two lists live in different crates; this pins the subset.
        for label in fauna_core::web::RESERVED_SUBDOMAIN_LABELS {
            assert!(
                super::is_reserved_handle(label),
                "reserved subdomain label {label:?} missing from RESERVED_HANDLES"
            );
        }
    }

    #[test]
    fn pds_is_reserved_because_the_sni_router_owns_that_subdomain() {
        // The deploy image routes `pds.<domain>` at L4 to the ATProto PDS bridge
        // (docker/s6/fauna-sni-router/run), whose own service identity is
        // `did:web:pds.<domain>`. A user holding handle `pds` would therefore get
        // a dead subdomain (the router shadows nest) and, once federation lands,
        // an ATProto handle colliding with the PDS's own service host. Same
        // treatment `relay` got when its route landed.
        assert!(super::is_reserved_handle("pds"));
        assert!(super::is_reserved_handle("PDS"));
    }

    #[test]
    fn reserved_handle_check_is_case_insensitive_and_nonempty() {
        assert!(!super::RESERVED_HANDLES.is_empty());
        for h in ["admin", "ADMIN", "Postmaster", "mta-sts"] {
            assert!(super::is_reserved_handle(h), "{h:?} must be reserved");
        }
        assert!(!super::is_reserved_handle("alice"));
    }
}
