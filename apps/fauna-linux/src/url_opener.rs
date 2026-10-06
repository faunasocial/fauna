//! Hand a URL to the OS default handler — the ONE opener resolution every
//! open-something-external call site in this app shares: the hosted-auth
//! verification URL (`views::onboarding::generic_provider_form`) and the
//! DNS/VPS "open in browser" provider-link buttons
//! (`views::onboarding::{dns_config,vps_config}`).
//!
//! Mirrors windows' `Services/UrlOpener.cs` and tui's `os_open` module, and
//! for the same reason: the OS already owns "which program opens a URL", so
//! fauna adds no program-picker knob — the only injection point is the e2e
//! suppression below.
//!
//! ⚠ Under a harness launch this must NOT reach the OS
//! (`e2e-conventions.md` point 10: an app launch isolates every inherited
//! channel). Measured on windows: opening the real browser on the bundled
//! provider's verification URL wedges the e2e `fake_cloud` fixture (it stops
//! answering past ~6 idle connections), stalling the app's own next request
//! for 60-90+ seconds. The handoff stays observable rather than silent: the
//! URL is logged at this seam instead of being silently dropped.

/// Hand `url` to the OS default handler, or — under e2e automation — log it
/// instead of launching.
pub fn open(url: &str) {
    open_with(url, crate::e2e_mode_enabled(), |u| {
        gtk::gio::AppInfo::launch_default_for_uri(u, None::<&gtk::gio::AppLaunchContext>)
    });
}

/// The gate plus the launch, with both the e2e state and the OS call
/// injected — the unit test drives this directly, so it never reads the real
/// environment or touches a real display.
fn open_with(url: &str, e2e: bool, launch: impl FnOnce(&str) -> Result<(), glib::Error>) {
    if e2e {
        tracing::info!("open-url: suppressed under the e2e harness: {url}");
        return;
    }
    if let Err(e) = launch(url) {
        tracing::warn!("open-url: the OS refused the launch: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn suppresses_the_real_launch_under_e2e() {
        let called = Cell::new(false);
        open_with("https://example.test/verify?code=abc123", true, |_| {
            called.set(true);
            Ok(())
        });
        assert!(!called.get(), "the OS launch must not fire under e2e");
    }

    #[test]
    fn launches_with_the_url_outside_e2e() {
        let seen = Cell::new(String::new());
        open_with("https://example.test/verify?code=abc123", false, |u| {
            seen.set(u.to_string());
            Ok(())
        });
        assert_eq!(seen.into_inner(), "https://example.test/verify?code=abc123");
    }
}
