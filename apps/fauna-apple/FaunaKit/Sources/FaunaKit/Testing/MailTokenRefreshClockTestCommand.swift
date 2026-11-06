import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it — the runtime
// `FAUNA_E2E_*` gates are the inner switch WITHIN a test-capable build, never
// the boundary.
#if DEBUG

/// Shared handler for the cross-app `mail_token_refresh_advance_clock`
/// TestAgent command — the submission-token refresh's clock
/// (`mail-credentials.md` § Architectural rules, rule 10; `testing.md`
/// convention 14 — a fake clock, never a sleep).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `TrustFacetClockTestCommand`. Drives the same shared Rust seam
/// tui / linux / web / android reach —
/// `fauna_client_mail_settings::token_refresh::set_clock_offset_secs`, through
/// the `test-helpers` UniFFI free function `setMailTokenRefreshClockOffsetSecs`
/// — which moves the 7-day re-mint cadence and each re-minted token's
/// `issued_at`, never the bearer clock.
///
/// Nothing runs here: the shared connect-time loop FaunaKit starts
/// (`startMailEpochRefreshOnEveryConnect`) reads the clock on its next pass —
/// a reconnect or the daily tick.
///
/// ⚠ The offset is **process-wide and nothing auto-resets it** — the test
/// resets it to `0` once its assertions are done.
public enum MailTokenRefreshClockTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure (convention 11).
    public static func apply(_ command: [String: Any]) -> String? {
        let offset = TestCommandArgs.nowOffsetSecs(command)
        setMailTokenRefreshClockOffsetSecs(offsetSecs: offset)
        return nil
    }
}

#endif
