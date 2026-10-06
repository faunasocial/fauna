//! The Settings root's About block — the running version, the
//! user-triggered "is a newer version out?" check, and the once-per-sign-in look.
//!
//! **The promise** (`installers/README.md` § Knowing a newer version is out): on
//! a platform where you install the app yourself you can ask the app whether a
//! newer version is out, and when one is, it says so and where to get it. Asking
//! is the floor; the app also looks once each time you sign in ([`look`],
//! spawned by `session::establish`) and only paints the same notice — never a
//! timer, never a download or a self-update. The round trip, the asked check's
//! three answers and the sign-in look are the shared `fauna_client::update_look`, and *what counts as newer* is the one shared
//! `fauna_core::version::is_newer`, reached here through
//! `newer_release_from_latest_json`, so this app cannot disagree with linux,
//! windows or macOS about it. The terminal app owes this since its channel was
//! ratified (`installers/tui.md`): the notice's "get it here" is that channel's
//! release page (`fauna_core::version::release_page_url`).
//!
//! **Where it sits.** The Settings root (the Status landing), after the region
//! section: tui has no General sub-page, and `settings.md` § Live-data placement
//! puts every live cell of the landing here. Three ids, all user-approved
//! 2026-09-25 under rule A: `settings-app-version` (always painted — the one
//! product version every app shares, `product-version.md` § The model),
//! `settings-check-updates-button` (its label carries the check's state — the
//! linux General page's button flips its label the same way), and
//! `update-available-notice`, painted ONLY once a check found a newer release.
//!
//! **The e2e seam.** A walk cannot depend on the real GitHub feed (network,
//! rate limits, and whatever release happens to be newest that day), so under
//! e2e automation the feed's origin comes from `FAUNA_E2E_RELEASE_FEED_URL` —
//! compile-gated exactly like `FAUNA_E2E_DOWNLOAD_DIR` in `backups.rs`
//! (convention 15: the release binary neither reads nor names it), and behind
//! `crate::e2e_mode_enabled` within a test-capable build. Production reads
//! `GITHUB_API_ORIGIN` and nothing else: there is no user knob for where
//! releases come from, and there must not be one.

use fauna_i18n::strings::{common, settings as t};
use fauna_ui_ids as ids;

use super::{Action, SettingsState};
use crate::element::{Element, Gesture};

/// The running app's version — the workspace's one product version.
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the user-triggered check stands. Session-local UI state: nothing here
/// is persisted, and a fresh launch starts at [`UpdateCheck::Idle`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum UpdateCheck {
    /// No check run yet (or the last result was dismissed by a sign-out).
    #[default]
    Idle,
    /// A check is in flight — the button is disabled and reads "Checking…".
    Checking,
    /// The feed answered and nothing newer is out.
    UpToDate,
    /// The feed could not be read (offline, a non-JSON answer, …). The safe
    /// default: never a spurious update prompt, and the button says so.
    Failed,
    /// A newer release: its tag as the feed spells it (`v0.2.0`).
    Newer { tag: String },
}

/// The About block's elements, in paint order.
pub(super) fn elements(state: &SettingsState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::SETTINGS_APP_VERSION, CURRENT_VERSION),
        Element::gesture_button(
            ids::SETTINGS_CHECK_UPDATES_BUTTON,
            button_label(&state.update_check),
            state.update_check != UpdateCheck::Checking,
            Gesture::Settings(Action::CheckForUpdates),
        ),
    ];
    if let UpdateCheck::Newer { tag } = &state.update_check {
        els.push(Element::label(
            ids::UPDATE_AVAILABLE_NOTICE,
            t::update_available_notice(
                tag.trim_start_matches('v'),
                &fauna_core::version::release_page_url(tag),
            ),
        ));
    }
    els
}

/// The button reports the check's state in its own label — the linux General
/// page's shape (`settings/general.rs`), so the words are the shared ones.
fn button_label(check: &UpdateCheck) -> String {
    match check {
        UpdateCheck::Idle => t::CHECK_FOR_UPDATES.to_owned(),
        UpdateCheck::Checking => common::CHECKING.to_owned(),
        UpdateCheck::UpToDate => t::UP_TO_DATE.to_owned(),
        UpdateCheck::Failed => t::CHECK_FAILED.to_owned(),
        UpdateCheck::Newer { tag } => t::general_page::update_available(tag),
    }
}

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
    format!("fauna-tui/{CURRENT_VERSION}")
}

/// One round trip to the feed, folded into the state the block paints — the
/// shared asked check, its three answers mapped onto the button's states.
pub(super) async fn check() -> UpdateCheck {
    use fauna_client::update_look::{NewerReleaseCheck, check_for_newer_release_over_http};
    match check_for_newer_release_over_http(
        &reqwest::Client::new(),
        &feed_origin(),
        &user_agent(),
        CURRENT_VERSION,
    )
    .await
    {
        NewerReleaseCheck::Newer { tag } => UpdateCheck::Newer { tag },
        NewerReleaseCheck::UpToDate => UpdateCheck::UpToDate,
        NewerReleaseCheck::Failed => UpdateCheck::Failed,
    }
}

/// The once-per-sign-in look: a newer release's tag, or `None` (nothing newer,
/// or a failed look — silent by rule). Same feed, same origin seam as
/// [`check`].
pub(super) async fn look() -> Option<String> {
    fauna_client::update_look::look_at_sign_in_over_http(
        &reqwest::Client::new(),
        &feed_origin(),
        &user_agent(),
        CURRENT_VERSION,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str) -> &'a str {
        &els.iter().find(|e| e.id == id).expect(id).text
    }

    #[test]
    fn idle_paints_the_version_and_the_button_but_no_notice() {
        let state = SettingsState::default();
        let els = elements(&state);
        assert_eq!(
            ids_of(&els),
            vec![
                ids::SETTINGS_APP_VERSION,
                ids::SETTINGS_CHECK_UPDATES_BUTTON
            ]
        );
        assert_eq!(text_of(&els, ids::SETTINGS_APP_VERSION), CURRENT_VERSION);
        assert_eq!(
            text_of(&els, ids::SETTINGS_CHECK_UPDATES_BUTTON),
            t::CHECK_FOR_UPDATES
        );
        assert!(els[1].enabled);
    }

    #[test]
    fn checking_disables_the_button() {
        let state = SettingsState {
            update_check: UpdateCheck::Checking,
            ..Default::default()
        };
        let els = elements(&state);
        assert!(!els[1].enabled);
        assert_eq!(
            text_of(&els, ids::SETTINGS_CHECK_UPDATES_BUTTON),
            common::CHECKING
        );
    }

    #[test]
    fn a_newer_release_paints_the_notice_with_the_version_and_the_release_page() {
        let state = SettingsState {
            update_check: UpdateCheck::Newer {
                tag: "v9.9.9".to_owned(),
            },
            ..Default::default()
        };
        let els = elements(&state);
        assert_eq!(
            ids_of(&els),
            vec![
                ids::SETTINGS_APP_VERSION,
                ids::SETTINGS_CHECK_UPDATES_BUTTON,
                ids::UPDATE_AVAILABLE_NOTICE
            ]
        );
        let notice = text_of(&els, ids::UPDATE_AVAILABLE_NOTICE);
        assert!(notice.contains("9.9.9"), "{notice}");
        assert!(
            notice.contains(&fauna_core::version::release_page_url("v9.9.9")),
            "{notice}"
        );
    }

    #[test]
    fn up_to_date_and_failed_paint_no_notice() {
        for check in [UpdateCheck::UpToDate, UpdateCheck::Failed] {
            let state = SettingsState {
                update_check: check,
                ..Default::default()
            };
            let els = elements(&state);
            assert!(!ids_of(&els).contains(&ids::UPDATE_AVAILABLE_NOTICE));
        }
    }

    #[test]
    fn the_feed_origin_is_github_outside_e2e() {
        // No FAUNA_E2E_AGENT_PORT in a unit test → e2e mode is off, so the
        // override is not even consulted.
        assert_eq!(feed_origin(), fauna_core::version::GITHUB_API_ORIGIN);
    }
}
