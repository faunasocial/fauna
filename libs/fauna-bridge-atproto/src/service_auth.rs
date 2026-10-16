//! The oracle's `atproto.service_auth` class — the one lifetime both halves
//! read (`docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* → *The bridge-held custodian*).
//!
//! The class is performed in two halves: the nest's door decides
//! (`bins/fauna-nest/src/atproto_oracle.rs`) and the `atproto.pds` bridge
//! mints with the repo signing key only it holds. Both clamp a token's
//! lifetime, so the cap has exactly one home: here. The nest reads
//! [`MAX_LIFETIME_SECS`] directly; the Go bridge's `getServiceAuth` cap
//! (`atprotopds.MaxServiceAuthLifetime`) is pinned to
//! [`service_auth_max_lifetime_secs`] by a cross-binary test, because its
//! package is cgo-free by design.
//!
//! Pure + wasm-clean, in the always-on core beside [`crate::authz`], whose
//! `service_auth_lxm_admitted` is the class's deny half.

/// The ceiling on a service-auth token's validity, in seconds
/// (`atproto-pds-full.md` § F3 detail: `exp ≤ 60 s`). A hard constant — no
/// user or admin would choose it: a service JWT is a bearer credential that
/// travels to a third party, and its lifetime is its whole revocation story.
pub const MAX_LIFETIME_SECS: u64 = 60;

/// [`MAX_LIFETIME_SECS`] for the Go bridge — uniffi cannot export a bare
/// `const`, and a hard-coded Go copy could drift, at which point the nest
/// would promise a lifetime the bridge does not mint.
#[cfg_attr(feature = "uniffi", uniffi::export)]
#[must_use]
pub fn service_auth_max_lifetime_secs() -> u64 {
    MAX_LIFETIME_SECS
}

/// The absolute `exp` (unix seconds) a mint is asked for, given the caller's
/// optional request at `now`: the request when it lies in
/// `(now, now + MAX_LIFETIME_SECS]`, otherwise `now + MAX_LIFETIME_SECS`.
///
/// An out-of-range request is read as "unspecified" rather than refused — the
/// bridge's own `effectiveLifetime` rule — so every outcome is bounded by the
/// cap and no caller's arithmetic can widen the window.
#[must_use]
pub fn clamp_exp(requested: Option<i64>, now: i64) -> i64 {
    let cap = now.saturating_add(MAX_LIFETIME_SECS as i64);
    match requested {
        Some(exp) if exp > now && exp <= cap => exp,
        _ => cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exported_cap_is_the_constant() {
        assert_eq!(service_auth_max_lifetime_secs(), MAX_LIFETIME_SECS);
        assert_eq!(MAX_LIFETIME_SECS, 60);
    }

    #[test]
    fn exp_is_clamped_to_the_cap_and_never_into_the_past() {
        let now = 1_000_000;
        assert_eq!(clamp_exp(None, now), now + 60);
        assert_eq!(clamp_exp(Some(now + 30), now), now + 30);
        assert_eq!(clamp_exp(Some(now + 60), now), now + 60);
        assert_eq!(clamp_exp(Some(now + 61), now), now + 60);
        assert_eq!(clamp_exp(Some(now), now), now + 60);
        assert_eq!(clamp_exp(Some(now - 5), now), now + 60);
        assert_eq!(clamp_exp(Some(i64::MAX), now), now + 60);
        assert_eq!(clamp_exp(Some(i64::MIN), now), now + 60);
    }
}
