//! Background `WrappedSubmissionTokenBlob` refresh hook.
//! **Skeleton** — the per-app task scheduler isn't wired in this
//! crate; this file holds the cadence constants + the shape the
//! future task will take.
//!
//! Authority: `docs/goal/behavior/mail-credentials.md` § Trigger
//! taxonomy § "Submission-token refresh".

/// Default re-mint cadence per credential. Tokens expire after 30
/// days (see `machine::SUBMISSION_TOKEN_LIFETIME_SECS`); the
/// refresher re-mints every 7 days so the unwrapped token at the
/// MTA is always well within its validity window.
pub const SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS: u64 = 7 * 86_400;

/// Tracking task name for telemetry / future structured logging.
pub const TOKEN_REFRESH_TASK: &str = "mail-submission-token-refresh";

// The driver function will land here once the per-app task
// surface exists. Phase A leaves the hook empty by design — the
// per-app glue layer can stub it cheaply, and the user-driven
// dispatch paths (EnableMail / AddCredential) already mint fresh
// tokens.
