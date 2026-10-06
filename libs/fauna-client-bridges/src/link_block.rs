//! Why a bridge's **Link** action is not actionable right now — the one answer
//! all 7 apps render, so a user never meets a control that looks live and does
//! nothing (`bridges.md` § Errors & edge cases, ratified 2026-08-11).
//!
//! The nest has a documented degraded shape: when `provider.status()` errors it
//! still emits the bridge, as
//! `available=true, linked=false, settings=[], link_modes=None, error=Some(why)`
//! (`bins/fauna-nest/src/bridges_ui_handlers.rs`, the `Err(e)` arm; the shape is
//! spelled out on [`BridgeStatus::error`]). Before this module every app decided
//! for itself what that meant, and all seven decided differently — five of them
//! by dropping the click on the floor with no message at all, while the nest had
//! sent a sentence explaining exactly what was wrong.
//!
//! **Why a count, not the mode list.** Which declared modes apply is genuinely
//! platform-local: apple keeps `platform == "desktop"` only on a Mac, android
//! keeps `"android"`, and web additionally drops a `nip07` mode when no browser
//! extension is present — a runtime capability no shared crate can observe. So
//! each app answers "how many modes apply *to me*" and this module rules on what
//! that answer means. The rule is the part that must not diverge.
//!
//! **Not in scope: `available == false`.** A provider the nest hasn't got
//! configured is a separate, already-ruled case — it "returns `available: false`
//! and renders disabled, not absent" (`bridges.md` § Errors & edge cases) and is
//! decided before a link mode is ever considered. This predicate answers only
//! "the user wants to link *this* bridge — can they, and if not, what do I tell
//! them?".

use fauna_protocol::bridges_ui::{BridgeLinkMode, BridgeStatus};

/// Does one declared link mode apply on the calling platform? **The** platform
/// mode-filter — the other half of this module's rule, lifted 2026-08-15 after
/// all seven apps were found holding their own copy of it (and tui holding
/// none, so a `platform`-scoped mode counted as applicable there).
///
/// The rule: a mode with no `platform` is universal; a scoped mode applies only
/// on the app whose **canonical name** it names. The vocabulary is the seven
/// app names — `linux`, `windows`, `macos`, `ios`, `android`, `web`, `tui` —
/// matching ui.yaml's platform vocabulary. The `"desktop"` alias apple's copy
/// matched is retired: no provider has ever emitted it (the tree's one scoped
/// mode is Nostr's `platform: "web"` NIP-07), and it left iOS answering to
/// *nothing*, so a hypothetical `platform: "ios"` mode would have been dropped
/// by the very app it targets.
///
/// What stays caller-local is exactly what a shared crate cannot observe:
/// *which name the caller is* (apple picks `macos` vs `ios` at runtime) and
/// runtime capability checks (web additionally drops a `nip07` mode when no
/// browser extension is present). The string-match itself must not diverge, and
/// now cannot.
pub fn mode_applies(mode_platform: Option<&str>, platform: &str) -> bool {
    mode_platform.is_none_or(|p| p == platform)
}

/// The declared modes that apply on `platform` — [`mode_applies`] over a
/// bridge's `link_modes`, for the apps that hold the wire type as Rust
/// (tui, linux). Feed its `len()` to [`link_block`]; render fields only from
/// modes that survived it, so the count and the form can never disagree.
pub fn applicable_modes(link_modes: &[BridgeLinkMode], platform: &str) -> Vec<BridgeLinkMode> {
    link_modes
        .iter()
        .filter(|m| mode_applies(m.platform.as_deref(), platform))
        .cloned()
        .collect()
}

/// Why the Link control is not actionable. `None` from [`link_block`] means it
/// **is** actionable — render it live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkBlock<'a> {
    /// `provider.status()` errored and the nest sent its own explanation.
    /// Already human-readable prose from the nest — **render it verbatim**, do
    /// not substitute a localized generic string for it. It is the only
    /// account of what actually went wrong, and discarding it is what left
    /// users staring at a dead button with no reason.
    ProviderError(&'a str),
    /// The bridge declares no mode that applies on this platform. Apps render
    /// their own localized string for this (`bridges.no_link_method`) — there
    /// is nothing provider-specific to say.
    NoApplicableMode,
}

impl LinkBlock<'_> {
    /// Display text — the nest's own explanation verbatim, or the localized
    /// generic when it sent none.
    pub fn text(&self) -> String {
        match self {
            LinkBlock::ProviderError(why) => (*why).to_string(),
            LinkBlock::NoApplicableMode => fauna_i18n::strings::bridges::NO_LINK_METHOD.to_string(),
        }
    }
}

/// Rule on a bridge's Link affordance, given how many of its declared modes
/// apply on the calling platform.
///
/// `applicable_modes` is the count *after* the caller's own platform filter (see
/// the module docs for why that half stays local).
///
/// A **linked** bridge is never blocked: its action button is Unlink, which
/// needs no mode.
pub fn link_block(bridge: &BridgeStatus, applicable_modes: usize) -> Option<LinkBlock<'_>> {
    link_block_of(bridge.linked, bridge.error.as_deref(), applicable_modes)
}

/// [`link_block`] over the three values the rule actually reads, for callers
/// that never hold a whole [`BridgeStatus`]: the wasm/UniFFI faces (records
/// cross by value, so passing the full row would copy every setting and mode on
/// each render) and linux's detail pane, which is handed its pieces separately.
///
/// Keeping this the core — and [`link_block`] a two-line delegate — is what
/// stops the rule being written a second time at a boundary.
pub fn link_block_of(
    linked: bool,
    error: Option<&str>,
    applicable_modes: usize,
) -> Option<LinkBlock<'_>> {
    if linked || applicable_modes > 0 {
        return None;
    }
    match error.map(str::trim).filter(|e| !e.is_empty()) {
        Some(why) => Some(LinkBlock::ProviderError(why)),
        None => Some(LinkBlock::NoApplicableMode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(linked: bool, error: Option<&str>) -> BridgeStatus {
        BridgeStatus {
            id: "activitypub".to_string(),
            name: "ActivityPub".to_string(),
            available: true,
            linked,
            identity: None,
            mode: None,
            settings: Vec::new(),
            supports_follows: false,
            supports_follow_requests: false,
            link_modes: None,
            glyph: None,
            error: error.map(str::to_string),
            extra: Default::default(),
        }
    }

    fn mode(name: &str, platform: Option<&str>) -> BridgeLinkMode {
        BridgeLinkMode {
            mode: name.to_string(),
            label: name.to_string(),
            client_action: None,
            platform: platform.map(str::to_string),
            fields: Vec::new(),
            extra: Default::default(),
        }
    }

    /// A mode with no `platform` is universal; a scoped one applies only on
    /// the app it names. This is the exact rule all seven apps hand-wrote
    /// (and tui didn't) — pinned once, where they all now read it.
    #[test]
    fn universal_modes_apply_everywhere_and_scoped_modes_only_on_their_app() {
        let modes = vec![mode("enable", None), mode("nip07", Some("web"))];
        let on_web: Vec<_> = applicable_modes(&modes, "web")
            .into_iter()
            .map(|m| m.mode)
            .collect();
        assert_eq!(on_web, vec!["enable", "nip07"]);
        for p in ["linux", "windows", "macos", "ios", "android", "tui"] {
            let elsewhere: Vec<_> = applicable_modes(&modes, p)
                .into_iter()
                .map(|m| m.mode)
                .collect();
            assert_eq!(
                elsewhere,
                vec!["enable"],
                "a web-scoped mode must not apply on {p}"
            );
        }
    }

    /// The `"desktop"` alias is RETIRED: it was apple's client-side invention,
    /// never emitted by any provider, and it left iOS answering to nothing. A
    /// mode scoped to an unknown family applies nowhere — the safe direction,
    /// since rendering a form whose client_action this app cannot perform is
    /// the live-but-inert control the link-block rule exists to prevent.
    #[test]
    fn an_unknown_platform_value_applies_nowhere() {
        let modes = vec![mode("x", Some("desktop"))];
        for p in ["linux", "windows", "macos", "ios", "android", "web", "tui"] {
            assert!(applicable_modes(&modes, p).is_empty());
        }
    }

    /// The predicate alone, as the FFI faces call it.
    #[test]
    fn the_predicate_matches_the_list_filter() {
        assert!(mode_applies(None, "tui"));
        assert!(mode_applies(Some("android"), "android"));
        assert!(!mode_applies(Some("web"), "android"));
    }

    /// The whole point: the nest's own explanation is what the user gets,
    /// verbatim, instead of a dead control.
    #[test]
    fn the_nests_explanation_is_the_reason_when_status_errored() {
        let b = bridge(false, Some("relay handshake failed: connection refused"));
        assert_eq!(
            link_block(&b, 0),
            Some(LinkBlock::ProviderError(
                "relay handshake failed: connection refused"
            ))
        );
    }

    /// A degraded bridge with no `error` still must not present a live control —
    /// it falls back to the localized generic reason, never to `None`.
    #[test]
    fn no_modes_and_no_error_is_still_blocked() {
        assert_eq!(
            link_block(&bridge(false, None), 0),
            Some(LinkBlock::NoApplicableMode)
        );
    }

    /// A blank/whitespace `error` is not a reason — degrade to the generic
    /// string rather than rendering an empty explanation.
    #[test]
    fn a_blank_error_degrades_to_the_generic_reason() {
        assert_eq!(
            link_block(&bridge(false, Some("   ")), 0),
            Some(LinkBlock::NoApplicableMode)
        );
        assert_eq!(
            link_block(&bridge(false, Some("")), 0),
            Some(LinkBlock::NoApplicableMode)
        );
    }

    /// The ordinary case: modes apply, so nothing is blocked — even if the
    /// bridge also carries an error (a partial-status provider still lets the
    /// user try).
    #[test]
    fn applicable_modes_mean_the_control_is_live() {
        assert_eq!(link_block(&bridge(false, None), 1), None);
        assert_eq!(link_block(&bridge(false, Some("stale cache")), 2), None);
    }

    /// A linked bridge's button is Unlink; it needs no mode and is never
    /// blocked, error or not.
    #[test]
    fn a_linked_bridge_is_never_blocked() {
        assert_eq!(link_block(&bridge(true, None), 0), None);
        assert_eq!(link_block(&bridge(true, Some("status flaky")), 0), None);
    }

    /// The nest's own explanation renders verbatim — never replaced by the
    /// localized generic, which is reserved for the no-explanation case.
    #[test]
    fn provider_error_text_is_the_nests_sentence_verbatim() {
        assert_eq!(
            LinkBlock::ProviderError("status flaky").text(),
            "status flaky"
        );
    }

    #[test]
    fn no_applicable_mode_text_is_the_localized_generic() {
        assert_eq!(
            LinkBlock::NoApplicableMode.text(),
            fauna_i18n::strings::bridges::NO_LINK_METHOD
        );
    }
}
