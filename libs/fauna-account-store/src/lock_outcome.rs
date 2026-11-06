//! The one outcome shape every try-lock role answers, on every platform.
//!
//! The engine-singleton and seed-leg roles each answer a non-blocking try the
//! same three ways — held, refused by a live holder, or unaskable — natively
//! over `flock` and on web over the Web Locks API. `locks::EngineLockOutcome`
//! and `locks::SeedLegLockOutcome` are this type over each role's held value,
//! so a consumer converts the outcome once (`fauna-account-plane`'s
//! `ElectionOutcome`) instead of hand-matching one enum per role per platform.

/// Result of a non-blocking role try over the held-role value `L`.
#[derive(Debug)]
pub enum LockOutcome<L> {
    /// This process (or tab) now holds the role; it releases when the value
    /// drops (or the process dies).
    Held(L),
    /// Another live holder has the role — another process or tab, or another
    /// runtime in this one. Run without the role and re-try on the backstop
    /// cadence.
    Refused,
    /// The lock could not be taken (natively) or asked for (on web) for a
    /// reason other than a live holder. The caller owns what this means.
    Degraded(std::io::Error),
}
