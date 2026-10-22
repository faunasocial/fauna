//! The background **submission-token refresh** — the cadence, the due test
//! and the clock it reads. The driver is
//! [`MailSettingsMachine::refresh_submission_tokens`](crate::MailSettingsMachine::refresh_submission_tokens);
//! the native apps run it through the shared connect-time trigger
//! (`rpc_glue::refresh_epoch_schedule_on_every_connect`), on every connect and
//! once a day in between.
//!
//! Authority: `docs/goal/behavior/mail-credentials.md` § Architectural rules,
//! rule 10, and § Trigger taxonomy § "Submission-token refresh".

use fauna_core::data::MailCredential;

/// Default re-mint cadence per credential. Tokens expire after 30
/// days (see `machine::SUBMISSION_TOKEN_LIFETIME_SECS`); the
/// refresher re-mints every 7 days so the unwrapped token at the
/// MTA is always well within its validity window.
pub const SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS: u64 = 7 * 86_400;

/// Tracking task name for telemetry / future structured logging.
pub const TOKEN_REFRESH_TASK: &str = "mail-submission-token-refresh";

/// How often a long-lived signed-in session re-runs the refresh between
/// connects — a client that never reconnects must still re-mint inside the
/// cadence.
pub const TOKEN_REFRESH_TICK_SECS: u64 = 86_400;

/// Whether `credential`'s token is owed a re-mint at `now` (unix seconds):
/// a live row whose last mint is at least the cadence old, or unknown
/// (`token_minted_at == None`). A marked row (burned or revoked) holds no
/// token and is never owed one.
#[must_use]
pub fn token_refresh_due(credential: &MailCredential, now: u64) -> bool {
    !credential.is_marked()
        && credential.token_minted_at.is_none_or(|minted| {
            now.saturating_sub(minted) >= SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS
        })
}

/// Seconds added to the token clock, set by the automation agents'
/// `mail_token_refresh_advance_clock` command — its own clock, not the bearer
/// clock (`fauna_protocol::client_clock`): aging a mail token must never move
/// the session's bearer schedule. Zero in every real run; compiled out of a
/// release build without this crate's `e2e-agent` feature (convention 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
static CLOCK_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The token clock's offset — zero in every real run and every release build.
fn clock_offset_secs() -> i64 {
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        CLOCK_OFFSET_SECS.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
    {
        0
    }
}

/// Set the offset above. ⚠ **Process-wide and nothing auto-resets it** — a
/// test that moves the clock zeroes it once its assertions are done.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_clock_offset_secs(offset: i64) {
    CLOCK_OFFSET_SECS.store(offset, std::sync::atomic::Ordering::SeqCst);
}

/// Unix seconds on the token clock — the `issued_at` every mint stamps and
/// the `now` the refresh measures the cadence against. The real clock in
/// every real run.
#[must_use]
pub fn now_secs() -> u64 {
    let now = fauna_core::data::Timestamp::now_secs().saturating_add(clock_offset_secs());
    u64::try_from(now).unwrap_or(0)
}
