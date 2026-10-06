//! The Settings → Web sub-page (`web-settings`) — the per-user authoring surface
//! for web-content hosting (`behavior/web-content-hosting.md`
//! § Published-post management; rail slot owned by `ui/settings.md`
//! § Navigation model).
//!
//! One control: the **subdomain opt-in toggle** (`web-settings-subdomain-toggle`,
//! default OFF) serving the actor's `web` content at `https://<handle>.<domain>/`.
//! Below it the live URL — or the reason there isn't one — and a static explainer
//! naming the two content sources (a `web`-mode folder, and web-published
//! posts).
//!
//! Renders off the shared `fauna-client-web` crate: the `WebClient` for the two
//! `fauna.web.*_subdomain_enabled` kinds and the pure `subdomain_view` projection
//! for the URL/disabled-reason decision, so every app shows the same thing
//! (priorities #1/#2). The nest-wide apex designation is the separate `admin-web`
//! page (`crate::admin::web`); the admin uses *this* page for their own site.
//!
//! The shell (`super`) owns the client, op and fold; this file is paint only.

use fauna_client_web::{SiteLinkDisabledReason, SiteLinkView, SubdomainDisabledReason};
use fauna_i18n::strings::web_publish as wp;
use fauna_i18n::strings::web_settings as t;
use fauna_ui_ids as ids;

use super::{Action, CopiedLink, SettingsState};
use crate::element::{Element, Gesture};

/// Where this actor's published content is reachable, resolved at PAINT time
/// from the two halves the page already holds: the `fauna.web.domain.get` rows
/// and the subdomain toggle's own confirmed state.
///
/// Derived rather than stored, so the toggle above and the links below can never
/// disagree — flipping the toggle re-resolves the origin on the next frame with
/// no extra round trip. Precedence (**active custom domain > enabled
/// subdomain**) and the disabled reasons are the shared
/// `fauna_client_web::site_link_view`, so all 7 apps answer identically
/// (`web-content-hosting.md` § Published-post management).
/// ⚠ The `domain` input is the nest's OWN serving domain
/// (`state.web.serving_domain`, read over `fauna.nest.info` at hydrate) — never
/// `url_host(&state.node_url)`, which is the address this app dialed. The two
/// coincide only when a nest is reached at its own serving name; everywhere else
/// the dialed host composes a URL the nest will never answer on, which is the
/// dead link `web-content-hosting.md` § Published-post management forbids.
pub(crate) fn site_link(state: &SettingsState) -> SiteLinkView {
    let enabled = state.web.view.as_ref().map(|v| v.enabled).unwrap_or(false);
    fauna_client_web::site_link_view(
        &state.web.domains,
        enabled,
        Some(state.handle.as_str()).filter(|h| !h.is_empty()),
        &state.web.serving_domain,
    )
}

/// The human-readable "your posts have no public address, and here's why" line
/// for a [`SiteLinkView`] with no origin — the shared
/// [`fauna_client_web::disabled_reason_text`] decision (previously hand-rolled identically here and on linux).
fn disabled_reason_text(reason: Option<SiteLinkDisabledReason>) -> String {
    fauna_client_web::disabled_reason_text(reason).resolve(fauna_i18n::strings::lookup)
}

pub(super) fn web_elements(state: &SettingsState) -> Vec<Element> {
    let view = state.web.view.as_ref();
    let enabled = view.map(|v| v.enabled).unwrap_or(false);

    // The URL row: the live URL whenever there is one (on OR off — it is where
    // the site *would* serve), else the disabled reason. Linux's `url_text`.
    let url_text = match view.map(|v| (v.url.as_deref(), v.disabled_reason)) {
        Some((Some(url), _)) => url.to_string(),
        Some((None, Some(SubdomainDisabledReason::NoHandle))) => t::SUBDOMAIN_NO_HANDLE.to_string(),
        Some((None, Some(SubdomainDisabledReason::ReservedLabel))) => {
            t::SUBDOMAIN_RESERVED.to_string()
        }
        // The nest serves no web content at any host. Painting the row (rather
        // than leaving it empty, which is what the missing case used to do)
        // is the point: "legal but unreachable — the UI must say so".
        Some((None, Some(SubdomainDisabledReason::NoServingDomain))) => {
            t::SUBDOMAIN_NO_SERVING_DOMAIN.to_string()
        }
        Some((None, None)) | None => String::new(),
    };

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
        Element::checkbox_gesture(
            ids::WEB_SETTINGS_SUBDOMAIN_TOGGLE,
            t::SUBDOMAIN_TOGGLE_LABEL,
            enabled,
            Gesture::Settings(Action::ToggleWebSubdomain),
        )
        // The driver's uniform read idiom. **Non-optimistic** — written only from
        // a nest-confirmed view, never from the click — which is exactly what
        // makes reading it back a round-trip proof rather than an echo of the
        // gesture (`test_web_authoring.py` § docstring).
        .attr("state", if enabled { "on" } else { "off" }),
        Element::chrome(t::SUBDOMAIN_TOGGLE_SUBTITLE),
    ];

    // An empty element never registers, so a pre-hydrate page has no
    // `web-settings-subdomain-url` for the e2e to read a stale value off.
    if !url_text.is_empty() {
        els.push(
            Element::label(ids::WEB_SETTINGS_SUBDOMAIN_URL, url_text)
                .labelled(t::SUBDOMAIN_URL_LABEL),
        );
    }
    // Painted only while the nest says the rendered pages are down — a healthy
    // site has no status element at all, so its presence IS the state. No
    // gesture: the nest restores the site by itself
    // (`web-content-hosting.md` § Routing, render, serving → *A blanked site
    // tells its author*).
    if state.web.rendered_pages_down {
        els.push(Element::label(
            ids::WEB_SETTINGS_RENDER_STATUS,
            t::RENDER_STATUS_DOWN,
        ));
    }
    els.push(Element::label(
        ids::WEB_SETTINGS_CONTENT_INFO,
        t::CONTENT_INFO,
    ));

    // The published-posts management section paints only once the page has
    // actually read the list. `view` is the hydrate signal (see
    // `WebSettingsState::view`): before it lands, `posts` is empty for the
    // uninteresting reason, and painting `web-published-posts-empty` off that
    // would tell the user "no published posts" about a list nobody has read.
    if view.is_some() {
        els.extend(published_posts_elements(state));
    }

    els
}

/// The Published-posts management section (`web-content-hosting.md`
/// § Published-post management): the caller's `fauna.web.publish.list` rows,
/// each offering the two copy affordances and a takedown.
///
/// **Row scoping** follows the `subscription-mine-row` idiom: a
/// `web-published-post-item` marker per row, with every leaf
/// `.within(ids::WEB_PUBLISHED_POST_ITEM, i)` — which keeps each leaf addressable
/// both flat (`get_text(id, index=i)`) and scoped
/// (`scope="web-published-post-item[0]"`). The list container is FLAT, not the
/// rows' registry ancestor, for the reason `muted_words.rs` records: nesting
/// them would put `web-published-posts-list` first in every leaf's path and make
/// every scoped read resolve to nothing while the page painted perfectly.
///
/// **Both copy buttons disable when the actor has no serving origin**, with the
/// reason painted beside them. Publishing with no origin is legal but
/// unreachable, and the doc is explicit that the UI must say so rather than hand
/// out a link that cannot load.
fn published_posts_elements(state: &SettingsState) -> Vec<Element> {
    let web = &state.web;
    let link = site_link(state);
    let origin = link.origin.as_deref();
    let any_gated = web.posts.iter().any(|p| p.gated_tier.is_some());

    let mut els = vec![
        Element::chrome(t::PUBLISHED_POSTS_TITLE),
        // The row container. Empty text: it is the landmark the driver waits on,
        // and the rows paint their own lines.
        Element::label(ids::WEB_PUBLISHED_POSTS_LIST, String::new()),
    ];

    if web.posts.is_empty() {
        els.push(Element::label(
            ids::WEB_PUBLISHED_POSTS_EMPTY,
            t::PUBLISHED_POSTS_EMPTY,
        ));
        return els;
    }

    // Said once for the section rather than per row: the ratified ~10-minute
    // validity, and the claim code as the durable alternative
    // (`monetization.md` § Pillar 2 → *Creator comp-link surface*, which states
    // the UI copy carries the validity and that no TTL knob exists). Only when
    // a gated row is actually present — otherwise it explains an affordance
    // nothing on screen offers.
    if any_gated {
        els.push(Element::chrome(wp::PAYWALL_LINK_NOTE));
    }
    // Why the copy buttons below are dead, in the user's own terms. Placed
    // above the rows so it reads as a statement about the section, not about
    // whichever row happens to be last.
    if origin.is_none() {
        els.push(Element::chrome(disabled_reason_text(link.disabled_reason)));
    }

    for (i, post) in web.posts.iter().enumerate() {
        // The row marker doubles as the scope container. Its text carries the
        // gated badge, which ui.yaml's component describes ("slug + a gated-tier
        // badge") but gives no id of its own — so the row line is where it
        // belongs. Ungated rows get a blank marker, the `subscription-mine-row`
        // shape.
        let badge = match post.gated_tier.as_deref() {
            Some(tier) => t::published_post_gated_badge(tier),
            None => " ".to_string(),
        };
        els.push(
            Element::label(ids::WEB_PUBLISHED_POST_ITEM, badge)
                .within(ids::WEB_PUBLISHED_POST_ITEM, i)
                // The tier as DATA, so an assertion on "this row is gated" reads
                // a field instead of matching a translated badge string.
                .attr("gated-tier", post.gated_tier.clone().unwrap_or_default()),
        );
        els.push(
            Element::label(ids::WEB_PUBLISHED_POST_SLUG, post.slug.clone())
                .within(ids::WEB_PUBLISHED_POST_ITEM, i),
        );
        els.push(
            copy_button(
                "web-published-post-copy-link-button",
                wp::COPY_WEB_LINK,
                origin.map(|o| fauna_client_web::post_page_url(o, &post.slug)),
                Action::CopyWebLink(i),
                matches!(&web.copied, Some(CopiedLink::Web { index, .. }) if *index == i),
                &web.copied,
            )
            .within(ids::WEB_PUBLISHED_POST_ITEM, i),
        );
        // Gated rows only: an ungated post has no paywalled body to hand out, so
        // the affordance would mint a token for nothing.
        if post.gated_tier.is_some() {
            els.push(
                copy_button(
                    "web-published-post-copy-paywall-link-button",
                    wp::COPY_PAYWALL_LINK,
                    // Unlike the public link, this value does not exist until
                    // the mint round-trips — so the button advertises no `value`
                    // up front, only what it actually copied afterwards.
                    origin.map(|_| String::new()),
                    Action::CopyPaywallLink(i),
                    matches!(&web.copied, Some(CopiedLink::Paywall { index, .. }) if *index == i),
                    &web.copied,
                )
                .within(ids::WEB_PUBLISHED_POST_ITEM, i),
            );
        }
        els.push(
            Element::gesture_button(
                ids::WEB_PUBLISHED_POST_UNPUBLISH_BUTTON,
                wp::UNPUBLISH,
                // Always live: a takedown needs no serving origin, and it is the
                // one thing a user with an unreachable site may well want.
                true,
                Gesture::Settings(Action::UnpublishPost(i)),
            )
            .within(ids::WEB_PUBLISHED_POST_ITEM, i),
        );
    }

    // What actually went on the clipboard, shown once under the list. OSC 52 is
    // fire-and-forget into a terminal that may ignore it, so painting the value
    // is what keeps a clipboard-less terminal from losing the link (the
    // `admin/dns.rs` doctrine) — and for the paywall link it repeats the
    // validity, since that copy is the one with an expiry to remember.
    if let Some(copied) = &web.copied {
        els.push(Element::chrome(match copied {
            CopiedLink::Web { url, .. } => wp::copied_link(url),
            CopiedLink::Paywall { url, .. } => wp::copied_paywall_link(url),
        }));
    }

    els
}

/// One copy affordance: disabled (with no `value`) when there is no serving
/// origin, and carrying the exact string it put on the clipboard once it has
/// fired.
///
/// The `copied` attr is what lets a test assert the copied **contents** rather
/// than the mere presence of a button — the devices-page lesson that unasserted
/// copy affordances rot invisibly. It is written from the same value that
/// reached `copy_to_clipboard`, never re-derived, so the two cannot drift.
fn copy_button(
    id: &str,
    label: &str,
    value: Option<String>,
    action: Action,
    is_last_copied: bool,
    copied: &Option<CopiedLink>,
) -> Element {
    let el = Element::gesture_button(id, label, value.is_some(), Gesture::Settings(action))
        .attr("value", value.unwrap_or_default());
    match (is_last_copied, copied) {
        (true, Some(c)) => el.attr("copied", c.url()),
        _ => el,
    }
}

/// This sub-page's error — read by `App::screen_error_text`, not painted here.
/// The page's error has to reach the ONE funnel that feeds the paint, the
/// registry and the state protocol's `messages.error` alike
/// (`App::error_line_text`'s honesty contract); a page-pushed `error-message`
/// element is registered but unreadable through the cross-app `error_text()`,
/// which reads `messages.error` first and only falls back to the element when
/// that key is ABSENT — and tui always emits it.
pub(super) fn page_error(state: &SettingsState) -> Option<String> {
    state
        .web
        .error
        .as_deref()
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::nest_for_test;
    use crate::settings::{Op, SubPage, WebSettingsState};
    use fauna_client_web::SubdomainView;

    /// An authenticated app parked on the Web sub-page. Authenticated on
    /// purpose: `App::error_line_text` reports the *launch* surface's error
    /// while signed out, so a signed-out fixture cannot see a page error at all.
    fn state_with(view: SubdomainView) -> crate::app::App {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::Web;
        app.settings.web = WebSettingsState {
            view: Some(view),
            // A hydrated page always carries the nest's own serving domain (the
            // `nest.info` read that opens `read_web_page`); the fixtures that
            // care about the no-serving-domain path override it with "".
            serving_domain: "example.com".to_string(),
            ..Default::default()
        };
        app
    }

    /// A `publish.list` row. `tier` `Some` ⇒ the gated shape that earns the
    /// paywall-link affordance.
    fn post(slug: &str, tier: Option<&str>) -> fauna_protocol::web::PublishedPost {
        fauna_protocol::web::PublishedPost {
            post_id: fauna_protocol::ByteBuf::from(vec![0xab; 32]),
            slug: slug.to_string(),
            gated_tier: tier.map(str::to_string),
            ..Default::default()
        }
    }

    /// An app on the Web sub-page whose subdomain hosting is ON with a live
    /// handle — i.e. the ordinary case where an origin resolves — carrying
    /// `posts`.
    fn state_with_posts(posts: Vec<fauna_protocol::web::PublishedPost>) -> crate::app::App {
        let mut app = state_with(SubdomainView {
            enabled: true,
            url: Some("https://alice.example.com/".to_string()),
            disabled_reason: None,
        });
        // `site_link` resolves from the handle + the nest's SERVING DOMAIN (not
        // from the view's pre-baked url, and pointedly not from `node_url` —
        // the address dialed is a different concept), so the fixture has to set
        // the same handle the view claims or the two would describe different
        // actors. `node_url` is left deliberately UNLIKE the serving domain, so
        // any regression that reaches back for the dialed host fails loudly
        // here instead of passing by coincidence.
        app.settings.handle = "alice".to_string();
        app.settings.node_url = "https://127.0.0.1:8443".to_string();
        app.settings.web.posts = posts;
        // The two mutating verbs need a client to dispatch against.
        app.settings.nest = Some(nest_for_test());
        app
    }

    /// Every element whose id matches, in paint order.
    fn all_with<'a>(els: &'a [Element], id: &str) -> Vec<&'a Element> {
        els.iter().filter(|e| e.id == id).collect()
    }

    fn attr<'a>(el: &'a Element, key: &str) -> Option<&'a str> {
        el.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn enabled_view_paints_toggle_on_with_the_live_url() {
        let app = state_with(SubdomainView {
            enabled: true,
            url: Some("https://alice.example.com/".to_string()),
            disabled_reason: None,
        });
        let els = web_elements(&app.settings);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        // The published-posts section joined the page (`web-content-hosting.md`
        // § Published-post management), so the hydrated page's tail is now the
        // list plus — this fixture publishes nothing — its empty state.
        assert_eq!(
            tagged,
            vec![
                "page-heading",
                "settings-nav-back",
                "web-settings-subdomain-toggle",
                "web-settings-subdomain-url",
                "web-settings-content-info",
                "web-published-posts-list",
                "web-published-posts-empty",
            ]
        );
        let toggle = els
            .iter()
            .find(|e| e.id == "web-settings-subdomain-toggle")
            .expect("toggle painted");
        assert_eq!(attr(toggle, "state"), Some("on"));
        assert_eq!(
            els.iter()
                .find(|e| e.id == "web-settings-subdomain-url")
                .map(|e| e.text.as_str()),
            Some("https://alice.example.com/")
        );
    }

    /// The default state the e2e asserts first: OFF. The URL still renders — it
    /// is where the site *would* serve — which is why the row is not gated on
    /// `enabled`.
    #[test]
    fn disabled_view_still_shows_where_the_site_would_serve() {
        let app = state_with(SubdomainView {
            enabled: false,
            url: Some("https://alice.example.com/".to_string()),
            disabled_reason: None,
        });
        let els = web_elements(&app.settings);
        let toggle = els
            .iter()
            .find(|e| e.id == "web-settings-subdomain-toggle")
            .expect("toggle painted");
        assert_eq!(attr(toggle, "state"), Some("off"));
        assert!(els.iter().any(|e| e.id == "web-settings-subdomain-url"));
    }

    /// No handle ⇒ no URL: the row carries the reason instead, so the human is
    /// never shown a blank where an address belongs.
    #[test]
    fn no_handle_renders_the_reason_in_the_url_row() {
        let app = state_with(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: Some(SubdomainDisabledReason::NoHandle),
        });
        let els = web_elements(&app.settings);
        assert_eq!(
            els.iter()
                .find(|e| e.id == "web-settings-subdomain-url")
                .map(|e| e.text.as_str()),
            Some(t::SUBDOMAIN_NO_HANDLE)
        );
    }

    #[test]
    fn reserved_label_renders_its_own_reason() {
        let app = state_with(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: Some(SubdomainDisabledReason::ReservedLabel),
        });
        let els = web_elements(&app.settings);
        assert_eq!(
            els.iter()
                .find(|e| e.id == "web-settings-subdomain-url")
                .map(|e| e.text.as_str()),
            Some(t::SUBDOMAIN_RESERVED)
        );
    }

    /// Pre-hydrate the page paints no URL row at all (an empty element never
    /// registers), so the e2e cannot read a stale or blank value off it.
    #[test]
    fn pre_hydrate_paints_no_url_row_and_toggle_reads_off() {
        let mut app = crate::app::tests::test_app();
        app.settings.sub = SubPage::Web;
        let els = web_elements(&app.settings);
        assert!(!els.iter().any(|e| e.id == "web-settings-subdomain-url"));
        let toggle = els
            .iter()
            .find(|e| e.id == "web-settings-subdomain-toggle")
            .expect("toggle painted");
        assert_eq!(attr(toggle, "state"), Some("off"));
    }

    /// **The non-optimism proof.** Clicking the toggle must NOT move the painted
    /// `state` — only a nest-confirmed `Outcome::WebView` may. This is what makes
    /// the e2e's read-back a genuine round-trip proof rather than an echo of the
    /// gesture it just performed, and it is exactly the property an
    /// existence-style assertion would miss.
    #[test]
    fn clicking_the_toggle_does_not_move_state_until_the_nest_confirms() {
        let mut app = state_with(SubdomainView {
            enabled: false,
            url: Some("https://alice.example.com/".to_string()),
            disabled_reason: None,
        });
        // A live session, so the gesture actually produces its network op rather
        // than bailing on a missing client.
        app.settings.nest = Some(nest_for_test());
        assert_eq!(painted_state(&app), Some("off".to_string()));

        let op =
            crate::settings::apply_local(&mut app, crate::settings::Action::ToggleWebSubdomain);
        assert!(
            matches!(op, Some(Op::SetWebSubdomain { .. })),
            "the toggle must produce its network op, not just flip local state"
        );
        assert_eq!(
            painted_state(&app),
            Some("off".to_string()),
            "the click alone must not flip the painted state — that would make the \
             e2e's read-back an echo of the gesture, not a round-trip proof"
        );

        // Only the nest's echoed view moves it.
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebView(Ok(SubdomainView {
                enabled: true,
                url: Some("https://alice.example.com/".to_string()),
                disabled_reason: None,
            })),
        );
        assert_eq!(painted_state(&app), Some("on".to_string()));
    }

    /// A rejected write leaves the last nest-confirmed state painted and adds the
    /// reason — the toggle never claims a state the nest refused.
    #[test]
    fn a_failed_write_keeps_the_confirmed_state_and_surfaces_the_error() {
        let mut app = state_with(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: None,
        });
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebView(Err("nest refused".to_string())),
        );
        assert_eq!(painted_state(&app), Some("off".to_string()));
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("nest refused"),
            "the refusal must reach the screen's one error line"
        );
    }

    fn painted_state(app: &crate::app::App) -> Option<String> {
        web_elements(&app.settings)
            .iter()
            .find(|e| e.id == "web-settings-subdomain-toggle")
            .and_then(|e| attr(e, "state").map(str::to_string))
    }

    #[test]
    fn error_reaches_the_screens_error_line() {
        let mut app = state_with(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: None,
        });
        app.settings.web.error = Some("subdomain write failed".to_string());
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("subdomain write failed"),
            "the page error must reach the ONE funnel the paint, the registry and \
             `messages.error` all read"
        );
        assert!(
            !web_elements(&app.settings)
                .iter()
                .any(|e| e.id == "error-message"),
            "and must NOT be a second, page-pushed copy of the id"
        );
    }

    // ── The Published-posts management section ──────────────────────────

    /// Pre-hydrate the section does not paint AT ALL — an empty `posts` before
    /// the first read is empty for the uninteresting reason, and claiming "no
    /// published posts" about a list nobody has read is the same lie the URL row
    /// avoids.
    #[test]
    fn the_section_stays_unpainted_until_the_page_has_read_the_list() {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::Web;
        app.settings.web = WebSettingsState::default();
        let els = web_elements(&app.settings);
        assert!(
            !els.iter().any(|e| e.id.starts_with("web-published-posts")),
            "an un-hydrated page must not paint the list or its empty state"
        );
    }

    #[test]
    fn a_hydrated_empty_list_paints_the_container_and_the_empty_state() {
        let app = state_with_posts(vec![]);
        let els = web_elements(&app.settings);
        assert_eq!(all_with(&els, "web-published-posts-list").len(), 1);
        assert_eq!(all_with(&els, "web-published-posts-empty").len(), 1);
        assert!(
            !els.iter().any(|e| e.id == "web-published-post-item"),
            "no rows for an empty list"
        );
    }

    /// A site the nest took dark says so, and only then: a healthy page has no
    /// status element at all, and the takedown's reload — whose render is one
    /// of the renders that restores a site — clears it without a re-visit.
    #[test]
    fn the_render_status_paints_only_while_the_rendered_pages_are_down() {
        let mut app = state_with_posts(vec![post("first-post", None)]);
        assert!(
            all_with(&web_elements(&app.settings), "web-settings-render-status").is_empty(),
            "a healthy site paints no status line"
        );

        app.settings.web.rendered_pages_down = true;
        let els = web_elements(&app.settings);
        let status = all_with(&els, "web-settings-render-status");
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].text, t::RENDER_STATUS_DOWN);
        assert_eq!(
            all_with(&els, "web-published-post-slug").len(),
            1,
            "a dark site still lists its posts"
        );

        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPosts(Ok(fauna_client_web::PublishedSite::default())),
        );
        assert!(
            all_with(&web_elements(&app.settings), "web-settings-render-status").is_empty(),
            "the list re-read that follows a restoring render clears the line"
        );
    }

    #[test]
    fn each_row_carries_its_slug_and_the_rows_are_scoped_to_their_index() {
        let app = state_with_posts(vec![post("first-post", None), post("second-post", None)]);
        let els = web_elements(&app.settings);
        assert!(
            !els.iter().any(|e| e.id == "web-published-posts-empty"),
            "a non-empty list must not paint the empty state"
        );
        let slugs: Vec<&str> = all_with(&els, "web-published-post-slug")
            .iter()
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(slugs, ["first-post", "second-post"]);
        // Every leaf of row 1 must be addressable under `web-published-post-item[1]`
        // — the scoped-read contract the shared actions rely on.
        for id in [
            "web-published-post-slug",
            "web-published-post-copy-link-button",
            "web-published-post-unpublish-button",
        ] {
            let leaf = all_with(&els, id);
            assert_eq!(leaf.len(), 2, "{id} should paint once per row");
            assert_eq!(
                leaf[1].path.first().map(|(c, i)| (c.as_str(), *i)),
                Some(("web-published-post-item", 1)),
                "{id} row 1 must sit inside its own row container at index 1"
            );
        }
    }

    /// The paywall-link affordance is published **and** gated only — an ungated
    /// post has no sealed body to hand out, so minting a token for it would be
    /// meaningless.
    #[test]
    fn only_a_gated_row_offers_the_paywall_link() {
        let app = state_with_posts(vec![
            post("public-one", None),
            post("paid-one", Some("gold")),
        ]);
        let els = web_elements(&app.settings);
        let paywall = all_with(&els, "web-published-post-copy-paywall-link-button");
        assert_eq!(paywall.len(), 1, "exactly one gated row offers it");
        assert_eq!(
            paywall[0].path.first().map(|(c, i)| (c.as_str(), *i)),
            Some(("web-published-post-item", 1)),
            "and it is the GATED row's, not the first row's"
        );
        // Both rows keep the public link + takedown.
        assert_eq!(
            all_with(&els, "web-published-post-copy-link-button").len(),
            2
        );
        assert_eq!(
            all_with(&els, "web-published-post-unpublish-button").len(),
            2
        );
    }

    #[test]
    fn a_gated_row_carries_its_tier_as_data_and_paints_the_badge() {
        let app = state_with_posts(vec![post("paid-one", Some("gold"))]);
        let els = web_elements(&app.settings);
        let row = all_with(&els, "web-published-post-item")[0];
        assert_eq!(attr(row, "gated-tier"), Some("gold"));
        assert!(
            row.text.contains("gold"),
            "the gated badge must be VISIBLE on the row line, not only in the attr; got {:?}",
            row.text
        );
    }

    #[test]
    fn an_ungated_row_advertises_no_tier() {
        let app = state_with_posts(vec![post("public-one", None)]);
        let els = web_elements(&app.settings);
        let row = all_with(&els, "web-published-post-item")[0];
        assert_eq!(attr(row, "gated-tier"), Some(""));
    }

    /// The section states the ratified ~10-minute validity whenever it offers a
    /// paywall link, and does not explain an affordance it is not offering.
    #[test]
    fn the_paywall_validity_note_rides_the_gated_rows() {
        let gated = state_with_posts(vec![post("paid-one", Some("gold"))]);
        let painted = crate::ui::painted_line_texts(&web_elements(&gated.settings));
        assert!(
            painted.iter().any(|l| l.contains("10 minutes")),
            "a section offering paywall links must state their validity; painted: {painted:?}"
        );
        let ungated = state_with_posts(vec![post("public-one", None)]);
        let painted = crate::ui::painted_line_texts(&web_elements(&ungated.settings));
        assert!(
            !painted.iter().any(|l| l.contains("10 minutes")),
            "and must not explain an affordance no row offers"
        );
    }

    // ── No serving origin: legal but unreachable, and the UI must say so ──

    /// Subdomain hosting OFF and no custom domain ⇒ nothing this actor publishes
    /// is reachable. Both copy affordances go dead **with the reason on screen**
    /// rather than handing out a link that cannot load.
    #[test]
    fn no_serving_origin_disables_both_copy_affordances_with_the_reason() {
        let mut app = state_with_posts(vec![post("paid-one", Some("gold"))]);
        app.settings.web.view = Some(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: None,
        });
        let els = web_elements(&app.settings);
        for id in [
            "web-published-post-copy-link-button",
            "web-published-post-copy-paywall-link-button",
        ] {
            let btn = all_with(&els, id)[0];
            assert!(!btn.enabled, "{id} must be dead with no serving origin");
            assert_eq!(
                attr(btn, "value"),
                Some(""),
                "{id} must advertise no link it cannot serve"
            );
        }
        assert!(
            all_with(&els, "web-published-post-unpublish-button")[0].enabled,
            "the takedown stays live — it needs no origin"
        );
        let painted = crate::ui::painted_line_texts(&web_elements(&app.settings));
        assert!(
            painted.iter().any(|l| l.contains("Publish my website")),
            "the reason must be on screen, not merely implied by a dead button; \
             painted: {painted:?}"
        );
    }

    #[test]
    fn a_reserved_handle_names_its_own_reason() {
        let mut app = state_with_posts(vec![post("public-one", None)]);
        app.settings.handle = "www".to_string();
        let painted = crate::ui::painted_line_texts(&web_elements(&app.settings));
        assert!(
            painted.iter().any(|l| l.contains("reserved name")),
            "painted: {painted:?}"
        );
    }

    /// An **active custom domain beats an enabled subdomain** — the ratified
    /// precedence. The link the row advertises must be the custom one.
    #[test]
    fn an_active_custom_domain_wins_over_the_subdomain() {
        let mut app = state_with_posts(vec![post("my-post", None)]);
        app.settings.web.domains = vec![
            fauna_client_web::WebDomainRow {
                domain: "pending.example".to_string(),
                status: "pending".to_string(),
            },
            fauna_client_web::WebDomainRow {
                domain: "live.example".to_string(),
                status: "active".to_string(),
            },
        ];
        let els = web_elements(&app.settings);
        let btn = all_with(&els, "web-published-post-copy-link-button")[0];
        assert_eq!(
            attr(btn, "value"),
            Some("https://live.example/post/my-post.html"),
            "the active custom domain must win over alice.example.com"
        );
    }

    /// A domain that has not gone active yet has no cert, so linking to it would
    /// hand out a URL that fails to load — the subdomain must still win.
    #[test]
    fn a_pending_custom_domain_does_not_displace_the_subdomain() {
        let mut app = state_with_posts(vec![post("my-post", None)]);
        app.settings.web.domains = vec![fauna_client_web::WebDomainRow {
            domain: "pending.example".to_string(),
            status: "pending".to_string(),
        }];
        let els = web_elements(&app.settings);
        assert_eq!(
            attr(
                all_with(&els, "web-published-post-copy-link-button")[0],
                "value"
            ),
            Some("https://alice.example.com/post/my-post.html")
        );
    }

    // ── Copying ─────────────────────────────────────────────────────────

    /// The whole point of the affordance: the string that reaches the clipboard
    /// is the one the button advertised, and it is painted back so a
    /// clipboard-less terminal still has the link.
    #[test]
    fn copying_a_web_link_records_the_exact_url_it_copied() {
        let mut app = state_with_posts(vec![post("first-post", None), post("second-post", None)]);
        let op = crate::settings::apply_local(&mut app, Action::CopyWebLink(1));
        assert!(op.is_none(), "the public link costs no round trip");
        assert_eq!(
            app.settings.web.copied,
            Some(CopiedLink::Web {
                index: 1,
                url: "https://alice.example.com/post/second-post.html".to_string(),
            }),
            "row 1's link, not row 0's"
        );
        let els = web_elements(&app.settings);
        let buttons = all_with(&els, "web-published-post-copy-link-button");
        assert_eq!(
            attr(buttons[1], "copied"),
            Some("https://alice.example.com/post/second-post.html"),
            "the copied value paints back onto the button that produced it"
        );
        assert_eq!(
            attr(buttons[0], "copied"),
            None,
            "and onto no other row's button"
        );
        assert!(
            crate::ui::painted_line_texts(&web_elements(&app.settings))
                .iter()
                .any(|l| l.contains("https://alice.example.com/post/second-post.html")),
            "the copied link must also be VISIBLE — OSC 52 may be ignored by the terminal"
        );
    }

    /// With no origin there is nothing true to copy, so the action refuses even
    /// if it is somehow dispatched — the paint's disabled button is not the only
    /// guard, because a dead link on the clipboard is worse than no copy.
    #[test]
    fn copying_with_no_serving_origin_copies_nothing() {
        let mut app = state_with_posts(vec![post("first-post", None)]);
        app.settings.web.view = Some(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: None,
        });
        assert!(
            crate::settings::apply_local(&mut app, Action::CopyWebLink(0)).is_none(),
            "the refusal must not dispatch an op either"
        );
        assert_eq!(app.settings.web.copied, None);
    }

    /// The defect this page was rebuilt around (`web-content-hosting.md`
    /// § Published-post management → *Implementation status today*): the link
    /// must be composed on the host **the nest answers on**, which is not the
    /// address this app dialed. The fixture dials `127.0.0.1:8443` and the nest
    /// serves `web.test`; before the fix every copy affordance handed the user
    /// `https://alice.127.0.0.1:8443/…`, a URL the serving layer's subdomain
    /// resolver can never match.
    #[test]
    fn a_copied_link_is_built_on_the_nests_serving_domain_not_the_dialed_host() {
        let mut app = state_with_posts(vec![post("servable-page", None)]);
        app.settings.web.serving_domain = "web.test".to_string();
        app.settings.node_url = "https://127.0.0.1:8443".to_string();

        let _ = crate::settings::apply_local(&mut app, Action::CopyWebLink(0));
        let CopiedLink::Web { url, .. } = app
            .settings
            .web
            .copied
            .clone()
            .expect("a resolvable origin must copy something")
        else {
            panic!("the web copy affordance must record a Web link");
        };
        assert_eq!(
            url, "https://alice.web.test/post/servable-page.html",
            "the copied link must name the host the NEST serves on"
        );
        assert!(
            !url.contains("127.0.0.1"),
            "the dialed host must not reach the clipboard: {url}"
        );
    }

    /// A nest with no serving domain at all must say so on the URL row rather
    /// than paint nothing — the pre-fix behaviour was a silently blank row,
    /// because "no url and no reason" had no arm.
    #[test]
    fn a_nest_with_no_serving_domain_says_so_rather_than_painting_a_blank_row() {
        let mut app = state_with(fauna_client_web::subdomain_view(true, Some("alice"), ""));
        app.settings.handle = "alice".to_string();
        app.settings.web.serving_domain = String::new();

        let els = web_elements(&app.settings);
        let row = els
            .iter()
            .find(|e| e.id == "web-settings-subdomain-url")
            .expect("the URL row must be PAINTED — an absent row explains nothing");
        assert_eq!(row.text, t::SUBDOMAIN_NO_SERVING_DOMAIN);

        // …and the copy affordances refuse, rather than composing on "".
        app.settings.web.posts = vec![post("a-post", None)];
        app.settings.nest = Some(nest_for_test());
        assert!(
            crate::settings::apply_local(&mut app, Action::CopyWebLink(0)).is_none(),
            "there is no origin to copy on a nest that serves no web content"
        );
        assert_eq!(app.settings.web.copied, None);
    }

    /// A click on a row that no longer exists resolves to nothing rather than to
    /// whatever slid into that slot.
    #[test]
    fn copying_a_row_that_moved_resolves_to_nothing() {
        let mut app = state_with_posts(vec![post("only-post", None)]);
        assert!(crate::settings::apply_local(&mut app, Action::CopyWebLink(7)).is_none());
        assert_eq!(app.settings.web.copied, None);
    }

    #[test]
    fn the_paywall_link_mints_for_the_gated_row_and_carries_the_resolved_origin() {
        let mut app = state_with_posts(vec![
            post("public-one", None),
            post("paid-one", Some("gold")),
        ]);
        let op = crate::settings::apply_local(&mut app, Action::CopyPaywallLink(1));
        match op {
            Some(Op::WebMintPaywallLink {
                index,
                slug,
                origin,
                ..
            }) => {
                assert_eq!(index, 1);
                assert_eq!(slug, "paid-one");
                assert_eq!(origin, "https://alice.example.com/");
            }
            _ => panic!("expected a WebMintPaywallLink op"),
        }
        assert_eq!(
            app.settings.web.copied, None,
            "nothing is on the clipboard until the mint lands"
        );
    }

    /// An UNGATED row has no paywalled body, so a stale index must not mint
    /// against it.
    #[test]
    fn the_paywall_link_refuses_an_ungated_row() {
        let mut app = state_with_posts(vec![post("public-one", None)]);
        assert!(crate::settings::apply_local(&mut app, Action::CopyPaywallLink(0)).is_none());
    }

    #[test]
    fn a_minted_paywall_link_is_copied_and_painted_with_its_validity() {
        let mut app = state_with_posts(vec![post("paid-one", Some("gold"))]);
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPaywallLink(Ok((
                0,
                "https://alice.example.com/post/paid-one.html?token=AbC-_123".to_string(),
            ))),
        );
        assert_eq!(
            app.settings.web.copied,
            Some(CopiedLink::Paywall {
                index: 0,
                url: "https://alice.example.com/post/paid-one.html?token=AbC-_123".to_string(),
            })
        );
        let painted = crate::ui::painted_line_texts(&web_elements(&app.settings));
        assert!(
            painted.iter().any(|l| l.contains("token=AbC-_123")),
            "painted: {painted:?}"
        );
        assert!(
            painted
                .iter()
                .any(|l| l.contains("10 minutes") && l.contains("token=AbC-_123")),
            "the copied paywall link must restate its expiry beside the value; painted: {painted:?}"
        );
    }

    /// A failed mint copies nothing — the user is never left believing a dead
    /// string is on their clipboard — and the reason reaches the error line.
    #[test]
    fn a_failed_mint_copies_nothing_and_surfaces_the_reason() {
        let mut app = state_with_posts(vec![post("paid-one", Some("gold"))]);
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPaywallLink(Err("mint paywall link: refused".to_string())),
        );
        assert_eq!(app.settings.web.copied, None);
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("mint paywall link: refused")
        );
    }

    // ── Takedown ────────────────────────────────────────────────────────

    #[test]
    fn unpublish_sends_the_rows_own_post_id() {
        let mut app = state_with_posts(vec![post("first-post", None), post("second-post", None)]);
        app.settings.web.posts[1].post_id = fauna_protocol::ByteBuf::from(vec![0x11; 32]);
        match crate::settings::apply_local(&mut app, Action::UnpublishPost(1)) {
            Some(Op::WebUnpublish { post_id, .. }) => assert_eq!(post_id, vec![0x11; 32]),
            _ => panic!("expected a WebUnpublish op"),
        }
    }

    /// The list repaints from the nest's re-read, so a row leaves only once the
    /// takedown actually committed.
    #[test]
    fn a_takedown_repaints_from_the_nests_re_read() {
        let mut app = state_with_posts(vec![post("first-post", None), post("second-post", None)]);
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPosts(Ok(fauna_client_web::PublishedSite {
                posts: vec![post("second-post", None)],
                ..Default::default()
            })),
        );
        let els = web_elements(&app.settings);
        let slugs: Vec<&str> = all_with(&els, "web-published-post-slug")
            .iter()
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(slugs, ["second-post"]);
    }

    /// A takedown that failed leaves the list exactly as it was — the row the
    /// nest still serves stays on screen — and reports why.
    #[test]
    fn a_failed_takedown_keeps_the_row_and_surfaces_the_reason() {
        let mut app = state_with_posts(vec![post("first-post", None)]);
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPosts(Err("unpublish post: refused".to_string())),
        );
        assert_eq!(app.settings.web.posts.len(), 1);
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("unpublish post: refused")
        );
    }

    /// After the list moves, a stale "Copied: …" line would name a row that no
    /// longer sits at that index.
    #[test]
    fn a_takedown_drops_the_stale_copied_line() {
        let mut app = state_with_posts(vec![post("first-post", None), post("second-post", None)]);
        assert!(crate::settings::apply_local(&mut app, Action::CopyWebLink(1)).is_none());
        assert!(app.settings.web.copied.is_some());
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPosts(Ok(fauna_client_web::PublishedSite {
                posts: vec![post("second-post", None)],
                ..Default::default()
            })),
        );
        assert_eq!(app.settings.web.copied, None);
    }

    // ── The hydrate ─────────────────────────────────────────────────────

    /// One visit reads all three halves, and the fold lands every one — a page
    /// that got posts but not domains would resolve the wrong origin.
    #[test]
    fn the_hydrate_lands_the_view_the_domains_and_the_posts_together() {
        let mut app = state_with(SubdomainView {
            enabled: false,
            url: None,
            disabled_reason: None,
        });
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPage(Box::new(Ok(crate::settings::WebPageRead {
                view: SubdomainView {
                    enabled: true,
                    url: Some("https://alice.example.com/".to_string()),
                    disabled_reason: None,
                },
                serving_domain: "example.com".to_string(),
                domains: vec![fauna_client_web::WebDomainRow {
                    domain: "live.example".to_string(),
                    status: "active".to_string(),
                }],
                posts: vec![post("my-post", Some("gold"))],
                rendered_pages_down: false,
            }))),
        );
        assert_eq!(app.settings.web.domains.len(), 1);
        assert_eq!(app.settings.web.posts.len(), 1);
        assert_eq!(painted_state(&app), Some("on".to_string()));
    }

    /// A partial read is no read: the section stays unpainted rather than
    /// showing an origin resolved from half the inputs.
    #[test]
    fn a_failed_hydrate_leaves_the_section_unpainted_and_reports_why() {
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Settings;
        app.settings.sub = SubPage::Web;
        crate::settings::apply_outcome(
            &mut app,
            crate::settings::Outcome::WebPage(Box::new(Err("load web domains: down".to_string()))),
        );
        assert!(
            !web_elements(&app.settings)
                .iter()
                .any(|e| e.id.starts_with("web-published-posts"))
        );
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("load web domains: down")
        );
    }
}
