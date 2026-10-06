//! The **dial** seam: which URL a client opens a socket to for the nest that a
//! stored `nest_url` names.
//!
//! Every app reaches its authenticated session the same way — read the identity
//! trio out of the long-term store, drive [`LaunchMachine`](crate::LaunchMachine)
//! over the stored `nest_url`, then construct the authenticated client on it.
//! Production keeps those two strings identical: there is no override and no way
//! to install one, so [`resolved_dial_url`] is the identity function and this
//! module's state does not exist in the artifact at all
//! (`docs/goal/architecture/e2e-conventions.md` point 15 — the automation
//! surface is compiled out, not switched off).
//!
//! **What the seam is for.** A domain-shaped handle (`someone@fauna.test`) is
//! the only input that derives serving enablement ON (`onboarding.md` § 3b), and
//! no local DNS resolves it — so a harness claim used to reach `LoggedIn` and
//! then never connect. Every *pre-identity* call already redirected through the
//! onboarding machine's `provider_base_urls["nest"]` override; the
//! post-`LoggedIn` dial was the one leg with no seam.
//! `OnboardingMachine::set_provider_base_urls` mirrors its `"nest"` entry here,
//! which is what lets a **store-read** dial — a relaunch, an "Add account"
//! switch, or the post-claim launch — resolve without any app threading a URL
//! through its own call chain. That matters because the store read is where
//! every app but tui begins: there is no onboarding machine in scope to ask.
//!
//! **It redirects the socket, never the truth.** The stored `nest_url`, the
//! registry row, and every user-facing rendering keep the literal typed string;
//! callers resolve at the point of connection and nowhere else. Persisting a
//! resolved URL would make an app dial the harness forever after — at-rest
//! corruption, not a test-only wart.

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
mod overridden {
    use std::sync::RwLock;

    /// Process-global, because the launch path that consumes it has no object
    /// to hang it off: a cold relaunch and an account switch both begin at the
    /// store, with no onboarding machine in scope. Mirrors the nest-identity
    /// TOFU pin store (`fauna_anon_client::trust`) — the other automation seam
    /// the shared E2E bridge drives without a machine, and for the same reason
    /// (`OnboardingMachine::call_machine_free_method`).
    ///
    /// ⚠ **Its blast radius is the whole process, so two `OnboardingMachine`s
    /// in one process are never independent** — however separate their nests,
    /// ports and certs are. `OnboardingMachine::provider_base_url` falls back to
    /// this global for any machine holding no `"nest"` entry of its own, and
    /// `OnboardingMachine::new` neither installs nor clears it, so a machine
    /// that asked for no override silently inherits the last one installed.
    /// That is the intended behaviour for the case this exists to serve (one
    /// app rebuilding its wizard), and a trap everywhere else.
    ///
    /// **Consequence for tests: one onboarding test per integration binary.**
    /// Two in one binary were red on `origin/main` under parallel libtest until
    /// 2026-08-23 — one installed an override, the other dialled its port and
    /// got `Connection refused` after that nest was torn down, reported as a
    /// `ChallengeResponse` probe error pointing at neither crate. A serial run
    /// was green, which is what hid it. Do not answer a recurrence with
    /// `--test-threads=1`: that is the configuration that hides it
    /// (`e2e-conventions.md` convention 10). Worked example:
    /// `bins/fauna-nest/tests/floor_tls_nest/mod.rs`.
    static NEST_DIAL_OVERRIDE: RwLock<Option<String>> = RwLock::new(None);

    /// Install (`Some`) or clear (`None`) the nest dial override.
    ///
    /// Driven by `OnboardingMachine::set_provider_base_urls` — the one gesture
    /// the harness already makes — so no app, driver or agent command has to
    /// learn about this seam. Clearing is as load-bearing as installing: the
    /// override outlives the wizard that installed it, and a stale one would
    /// point a later test's launch at a torn-down nest.
    pub fn set_nest_dial_override(url: Option<String>) {
        *NEST_DIAL_OVERRIDE
            .write()
            .unwrap_or_else(|e| e.into_inner()) = url;
    }

    /// The installed override, if any.
    pub fn nest_dial_override() -> Option<String> {
        NEST_DIAL_OVERRIDE
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub use overridden::{nest_dial_override, set_nest_dial_override};

/// Test-capable build: the URL to open a socket to for `nest_url` — the
/// installed override when there is one, else `nest_url` unchanged.
///
/// UniFFI-exported so a native app can resolve at its own store-read
/// connection site (windows/macos/ios/android — `OnboardingMachine` already
/// had a method twin, `resolved_nest_dial_url`, for its own onboarding-time
/// callers; this is the free-function twin for everyone else).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn resolved_dial_url(nest_url: &str) -> String {
    overridden::nest_dial_override().unwrap_or_else(|| nest_url.to_string())
}

/// Production twin: a shipped build has no override state and no way to install
/// one, so the dial URL and the identity URL are the same string.
///
/// The `cfg`-split lives here — once, in the crate every app's launch path
/// already runs through — rather than as seven per-app wrappers, so no app can
/// grow its own dialect of the rule while a release artifact still carries
/// neither the override state nor its setter.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn resolved_dial_url(nest_url: &str) -> String {
    nest_url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three: the override is process-global, so separate test
    /// functions would race each other under the default thread pool and the
    /// suite's verdict would depend on scheduling. The transitions are asserted
    /// in sequence instead — which is also the order the harness drives them
    /// (install at fixture setup, clear at teardown).
    #[test]
    fn the_override_redirects_the_socket_and_clearing_restores_the_stored_url() {
        set_nest_dial_override(None);
        assert_eq!(
            resolved_dial_url("https://fauna.test"),
            "https://fauna.test",
            "with no override installed the dial URL is the stored URL"
        );

        set_nest_dial_override(Some("http://127.0.0.1:8099".to_string()));
        assert_eq!(
            resolved_dial_url("https://fauna.test"),
            "http://127.0.0.1:8099",
            "an installed override replaces scheme, host AND port — which is \
             what lets a domain-shaped handle reach a local harness nest"
        );

        set_nest_dial_override(None);
        assert_eq!(
            resolved_dial_url("https://fauna.test"),
            "https://fauna.test",
            "clearing restores the stored URL, so a torn-down fixture's nest \
             cannot capture a later launch"
        );
    }
}
