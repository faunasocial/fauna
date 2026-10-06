//! In-app update check — asks the GitHub Releases API whether a newer release is out.
//!
//! This is the linux desktop's lightweight *notify-only* checker, distinct from
//! the `fauna-update` self-update loop (download + verify + apply) that
//! `fauna-nest` runs. Everything that decides the answer is shared:
//! the endpoint (`fauna_core::version::latest_release_api_url` over
//! `RELEASE_REPO`), the feed's shape and the `is_newer` semver rule
//! (`newer_release_from_latest_json`) — so every app that checks agrees on
//! *what counts as newer* and *where to look* (`installers/README.md` § Knowing
//! a newer version is out); the round trip and the asked check's three answers
//! are the shared `fauna_client::update_look::check_for_newer_release_over_http`.
//! This module is the asked check's call, the once-per-sign-in look's call ([`look_at_sign_in`], spawned
//! by `client.rs::check_for_updates`, over the shared
//! `fauna_client::update_look::look_at_sign_in_over_http`) and the desktop
//! notification, nothing more. Both reach the feed through the one
//! [`feed_origin`], and whatever either finds is painted as the
//! `update-available-notice` on Settings → General
//! (`settings/general.rs::show_update_notice`).
//!
//! **The e2e seam** — tui's shape (`apps/fauna-tui/src/settings/about.rs`): a
//! walk cannot depend on the real GitHub feed, so under e2e automation the
//! feed's origin comes from `FAUNA_E2E_RELEASE_FEED_URL`, compile-gated
//! (convention 15: a release binary neither reads nor names it) and behind
//! `crate::e2e_mode_enabled` within a test-capable build. Production reads
//! `GITHUB_API_ORIGIN` and nothing else — where releases come from is no user
//! knob.

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release feed's origin: production's GitHub API, or — only inside a
/// test-capable build under e2e automation — the harness's stub feed.
fn feed_origin() -> String {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if crate::e2e_mode_enabled()
        && let Some(origin) = std::env::var_os("FAUNA_E2E_RELEASE_FEED_URL")
            .filter(|v| !v.is_empty())
            .and_then(|v| v.into_string().ok())
    {
        return origin;
    }
    fauna_core::version::GITHUB_API_ORIGIN.to_owned()
}

fn user_agent() -> String {
    format!("fauna-linux/{CURRENT_VERSION}")
}

/// The asked check: ask the release feed for a newer release — the shared
/// three-way answer (newer, up to date, failed), so a failed round trip reads
/// as failed, never as "up to date".
pub async fn check_for_update(
    http: &reqwest::Client,
) -> fauna_client::update_look::NewerReleaseCheck {
    fauna_client::update_look::check_for_newer_release_over_http(
        http,
        &feed_origin(),
        &user_agent(),
        CURRENT_VERSION,
    )
    .await
}

/// The once-per-sign-in look: a newer release's tag, or `None` (nothing newer,
/// or a failed look — silent by rule). Same feed, same origin seam as
/// [`check_for_update`].
pub async fn look_at_sign_in(http: &reqwest::Client) -> Option<String> {
    fauna_client::update_look::look_at_sign_in_over_http(
        http,
        &feed_origin(),
        &user_agent(),
        CURRENT_VERSION,
    )
    .await
}

/// Show a desktop notification that an update is available.
pub fn notify_update_available(tag: &str) {
    use notify_rust::Notification;
    let _ = Notification::new()
        .summary(crate::i18n::strings::notifications::UPDATE_AVAILABLE_SUMMARY)
        .body(&crate::i18n::strings::notifications::update_available_body(
            tag,
        ))
        .icon("fauna")
        .timeout(notify_rust::Timeout::Milliseconds(8000))
        .show();
}
