//! The form-validity predicates' UniFFI faces (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the page rulings in
//! `docs/goal/ui/feed.md`, `search.md`, `nostr.md`, `folders.md`, `profile.md`
//! and `docs/goal/behavior/family-safety.md` § Errors & edge cases).
//!
//! Each forwards one shared predicate untouched, so Swift, Kotlin and C# paint
//! a form's submit from the same [`Possibility`] tui reads in Rust — disabled
//! with the reason within eyeshot, the submit asking the same function first
//! and refusing with the same reason — instead of re-deriving "the field is
//! blank" per app. The enum crosses the boundary whole, like
//! [`crate::offline::offline_possibility`]'s, so an app can fold the two with
//! one `and`.
//!
//! Gated `value-format` for the same bare-`fauna_core`-type reason as
//! `offline_possibility`: the Go mail-bridge's `--no-default-features` build has
//! no form to paint. The feed faces also need `feed-badge` (the `fauna-feed`
//! dep) and the search face `search` (the `fauna-client-search` dep), both of
//! which the kids flavor drops along with the pages that hold those forms.

use fauna_core::possibility::Possibility;

/// `post-submit-button`: [`fauna_feed::forms::post_submit_possibility`].
#[cfg(feature = "feed-badge")]
#[uniffi::export]
pub fn feed_post_submit_possibility(text: String) -> Possibility {
    fauna_feed::forms::post_submit_possibility(&text)
}

/// `feed-reply-submit-button`: [`fauna_feed::forms::reply_submit_possibility`].
#[cfg(feature = "feed-badge")]
#[uniffi::export]
pub fn feed_reply_submit_possibility(text: String) -> Possibility {
    fauna_feed::forms::reply_submit_possibility(&text)
}

/// `create-feed`: [`fauna_feed::forms::create_feed_possibility`].
#[cfg(feature = "feed-badge")]
#[uniffi::export]
pub fn feed_create_possibility(name: String) -> Possibility {
    fauna_feed::forms::create_feed_possibility(&name)
}

/// `bridge-form-subscribe-button`:
/// [`fauna_feed::forms::bridge_subscribe_possibility`].
#[cfg(feature = "feed-badge")]
#[uniffi::export]
pub fn feed_bridge_subscribe_possibility(kind: String, uri: String) -> Possibility {
    fauna_feed::forms::bridge_subscribe_possibility(&kind, &uri)
}

/// `search-submit-button`: [`fauna_client_search::query_possibility`].
#[cfg(feature = "search")]
#[uniffi::export]
pub fn search_query_possibility(query: String) -> Possibility {
    fauna_client_search::query_possibility(&query)
}

/// `nostr-link-button`: [`fauna_client_bridges::link_possibility`]. `mode` is
/// the link-request mode the picker holds (`generate` / `import` / `remote`).
#[uniffi::export]
pub fn nostr_link_possibility(mode: String, nsec: String, bunker_url: String) -> Possibility {
    fauna_client_bridges::link_possibility(&mode, &nsec, &bunker_url)
}

/// `nostr-add-follow`: [`fauna_client_bridges::add_follow_possibility`].
#[uniffi::export]
pub fn nostr_add_follow_possibility(pubkey: String) -> Possibility {
    fauna_client_bridges::add_follow_possibility(&pubkey)
}

/// `nostr-zap-signer-add-btn`'s draft half:
/// [`fauna_client_bridges::zap_signer_add_possibility`].
#[uniffi::export]
pub fn nostr_zap_signer_add_possibility(pubkey: String) -> Possibility {
    fauna_client_bridges::zap_signer_add_possibility(&pubkey)
}

/// The *Connected apps* section's gate:
/// [`fauna_client_bridges::connected_apps_possibility`] over the stored signing
/// mode (`None` with no linked account). Never an app-local mode check.
#[uniffi::export]
pub fn nostr_connected_apps_possibility(signing_mode: Option<String>) -> Possibility {
    fauna_client_bridges::connected_apps_possibility(signing_mode.as_deref())
}

/// `nostr-add-relay`: [`fauna_protocol::nostr_relay::add_relay_possibility`]
/// over the relays already listed and the typed draft.
#[uniffi::export]
pub fn nostr_add_relay_possibility(existing: Vec<String>, input: String) -> Possibility {
    fauna_protocol::nostr_relay::add_relay_possibility(&existing, &input)
}

/// `family-contact-add-button`: [`fauna_client_family::forms::contact_add_possibility`].
#[uniffi::export]
pub fn family_contact_add_possibility(input: String) -> Possibility {
    fauna_client_family::forms::contact_add_possibility(&input)
}

/// `family-transfer-button`: [`fauna_client_family::forms::transfer_possibility`].
#[uniffi::export]
pub fn family_transfer_possibility(input: String) -> Possibility {
    fauna_client_family::forms::transfer_possibility(&input)
}

/// `family-policy-save-button` and the feature-limit editor's save:
/// [`fauna_client_family::forms::policy_save_possibility`] over the three
/// screen-time cells as typed (empty = unset).
#[uniffi::export]
pub fn family_policy_save_possibility(
    window_start: String,
    window_end: String,
    daily: String,
) -> Possibility {
    fauna_client_family::forms::policy_save_possibility(&window_start, &window_end, &daily)
}

/// The Family tab gate: [`fauna_client_family::forms::has_family_surface`] over
/// the status read — never an app-local re-derivation.
#[uniffi::export]
pub fn family_has_surface(supervised: bool, wards: u32, incoming_transfers: u32) -> bool {
    fauna_client_family::forms::has_family_surface(
        supervised,
        wards as usize,
        incoming_transfers as usize,
    )
}

/// `folder-follow-confirm`: [`fauna_client_folders::forms::follow_possibility`].
/// `ready` = machine, nest and identity secret all held.
#[uniffi::export]
pub fn folder_follow_possibility(handle: String, name: String, ready: bool) -> Possibility {
    fauna_client_folders::forms::follow_possibility(&handle, &name, ready)
}

/// `folder-share-confirm`: [`fauna_client_folders::forms::share_possibility`].
/// `ready` = machine, nest, identity secret and the live conversations session.
#[uniffi::export]
pub fn folder_share_possibility(recipient: String, ready: bool) -> Possibility {
    fauna_client_folders::forms::share_possibility(&recipient, ready)
}

/// `subscription-tier-form-save`:
/// [`fauna_client_subscriptions::forms::tier_form_possibility`].
#[uniffi::export]
pub fn subscription_tier_form_possibility(name: String) -> Possibility {
    fauna_client_subscriptions::forms::tier_form_possibility(&name)
}

/// `subscription-claim-redeem-button`'s draft half:
/// [`fauna_client_subscriptions::forms::redeem_code_possibility`], folded with
/// the payments gate by the caller.
#[uniffi::export]
pub fn subscription_redeem_code_possibility(code: String) -> Possibility {
    fauna_client_subscriptions::forms::redeem_code_possibility(&code)
}

// The own-tier and mail gates (`docs/goal/ui/folders.md` § Errors & edge
// cases, *Own-tier and mail gates*; `docs/goal/ui/profile.md` § Errors & edge
// cases, *A tier before what sells it*): each forwards the shared answer over
// the read the page tracks, so a pending or failed read is never painted as
// "create a tier" / "set up mail". One face per control, each fixing its
// `OwnTierUse`, so the use never crosses the boundary as a loose word.

/// `folder-paywall-tier-select`:
/// [`fauna_client_subscriptions::own_tiers::own_tiers_possibility`], use
/// `Paywall`, over the page's own-tier read.
#[uniffi::export]
pub fn folder_paywall_possibility(
    read: fauna_core::possibility::ReadState,
    own_tiers: u32,
) -> Possibility {
    use fauna_client_subscriptions::own_tiers::{OwnTierUse, own_tiers_possibility};
    own_tiers_possibility(read, own_tiers as usize, OwnTierUse::Paywall)
}

/// `subscription-claim-mint-button`: the same answer, use `ClaimCode`, over
/// the Tiers tab's tier read.
#[uniffi::export]
pub fn subscription_claim_mint_possibility(
    read: fauna_core::possibility::ReadState,
    own_tiers: u32,
) -> Possibility {
    use fauna_client_subscriptions::own_tiers::{OwnTierUse, own_tiers_possibility};
    own_tiers_possibility(read, own_tiers as usize, OwnTierUse::ClaimCode)
}

/// `subscription-provider-add-button` (the provider form's opener): the same
/// answer, use `PaymentProvider`, over the Tiers tab's tier read.
#[uniffi::export]
pub fn subscription_provider_add_possibility(
    read: fauna_core::possibility::ReadState,
    own_tiers: u32,
) -> Possibility {
    use fauna_client_subscriptions::own_tiers::{OwnTierUse, own_tiers_possibility};
    own_tiers_possibility(read, own_tiers as usize, OwnTierUse::PaymentProvider)
}

/// `folder-webdav-toggle`:
/// [`fauna_client_folders::webdav_provision::webdav_serve_possibility`] over
/// the mail-custody read behind `owner_can_serve_webdav` (`holds_msek` is that
/// read's answer, ignored unless it resolved).
#[uniffi::export]
pub fn folder_webdav_serve_possibility(
    read: fauna_core::possibility::ReadState,
    holds_msek: bool,
) -> Possibility {
    fauna_client_folders::webdav_provision::webdav_serve_possibility(read, holds_msek)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_tier_and_webdav_faces_forward() {
        use fauna_core::possibility::ReadState;
        assert_eq!(
            key(folder_paywall_possibility(ReadState::Pending, 0)).as_deref(),
            Some("common.still_loading")
        );
        assert_eq!(
            key(folder_paywall_possibility(ReadState::Resolved, 0)).as_deref(),
            Some("devices.paywall_tier_needs_tier")
        );
        assert_eq!(
            key(subscription_claim_mint_possibility(ReadState::Resolved, 0)).as_deref(),
            Some("subscriptions.claim_needs_tier")
        );
        assert_eq!(
            key(subscription_provider_add_possibility(ReadState::Failed, 3)).as_deref(),
            Some("common.load_failed")
        );
        assert!(subscription_provider_add_possibility(ReadState::Resolved, 1).is_possible());
        assert_eq!(
            key(folder_webdav_serve_possibility(ReadState::Resolved, false)).as_deref(),
            Some("devices.serve_webdav_needs_mail")
        );
        assert!(folder_webdav_serve_possibility(ReadState::Resolved, true).is_possible());
    }

    fn key(p: Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[cfg(feature = "feed-badge")]
    #[test]
    fn feed_faces_forward() {
        assert_eq!(
            key(feed_post_submit_possibility(" ".into())).as_deref(),
            Some("feed.compose_empty")
        );
        assert!(feed_post_submit_possibility("hi".into()).is_possible());
        assert_eq!(
            key(feed_reply_submit_possibility("".into())).as_deref(),
            Some("feed.reply_empty")
        );
        assert_eq!(
            key(feed_create_possibility("".into())).as_deref(),
            Some("feed.create.name_required")
        );
        assert_eq!(
            key(feed_bridge_subscribe_possibility("rss".into(), "".into())).as_deref(),
            Some("feed.bridge_form.uri_required")
        );
        assert!(feed_bridge_subscribe_possibility("rss".into(), "u".into()).is_possible());
    }

    #[cfg(feature = "search")]
    #[test]
    fn search_face_forwards() {
        assert_eq!(
            key(search_query_possibility("  ".into())).as_deref(),
            Some("search_page.query_required")
        );
        assert!(search_query_possibility("cats".into()).is_possible());
    }

    #[test]
    fn nostr_faces_forward() {
        assert_eq!(
            key(nostr_link_possibility(
                "import".into(),
                "".into(),
                "".into()
            ))
            .as_deref(),
            Some("nostr.link_account.enter_nsec")
        );
        assert!(nostr_link_possibility("generate".into(), "".into(), "".into()).is_possible());
        assert_eq!(
            key(nostr_add_follow_possibility(" ".into())).as_deref(),
            Some("nostr.follows.pubkey_required")
        );
        assert_eq!(
            key(nostr_zap_signer_add_possibility("abc".into())).as_deref(),
            Some("nostr.zap_signers.invalid_pubkey")
        );
        assert_eq!(
            nostr_connected_apps_possibility(Some("remote".into())),
            Possibility::Absent
        );
        assert!(nostr_connected_apps_possibility(Some("imported".into())).is_possible());
        assert_eq!(
            key(nostr_add_relay_possibility(vec![], "".into())).as_deref(),
            Some("nostr.relays.url_required")
        );
        assert_eq!(
            key(nostr_add_relay_possibility(
                vec!["wss://relay.damus.io".into()],
                "wss://relay.damus.io".into()
            ))
            .as_deref(),
            Some("nostr.relays.already_added")
        );
    }

    #[test]
    fn family_faces_forward() {
        assert_eq!(
            key(family_contact_add_possibility(" ".into())).as_deref(),
            Some("family.contact_add_required")
        );
        assert_eq!(
            key(family_contact_add_possibility("zz".into())).as_deref(),
            Some("family.contact_add_invalid_actor_id")
        );
        assert!(family_contact_add_possibility("ab".repeat(32)).is_possible());
        assert_eq!(
            key(family_transfer_possibility("".into())).as_deref(),
            Some("family.transfer_required")
        );
        assert_eq!(
            key(family_transfer_possibility("nothex".into())).as_deref(),
            Some("family.transfer_invalid_actor_id")
        );
        assert!(family_policy_save_possibility("".into(), "".into(), "".into()).is_possible());
        assert_eq!(
            key(family_policy_save_possibility(
                "22:00".into(),
                "".into(),
                "".into()
            ))
            .as_deref(),
            Some("family.policy_screen_time_invalid")
        );
        assert!(!family_has_surface(false, 0, 0));
        assert!(family_has_surface(false, 0, 1));
    }

    #[test]
    fn folder_faces_forward() {
        assert_eq!(
            key(folder_follow_possibility("".into(), "x".into(), true)).as_deref(),
            Some("folders.follow_handle_required")
        );
        assert_eq!(
            key(folder_follow_possibility("a".into(), "x".into(), false)).as_deref(),
            Some("folders.follow_not_ready")
        );
        assert!(folder_follow_possibility("a".into(), "x".into(), true).is_possible());
        assert_eq!(
            key(folder_share_possibility(" ".into(), true)).as_deref(),
            Some("folders.share_recipient_required")
        );
        assert_eq!(
            key(folder_share_possibility("bob".into(), false)).as_deref(),
            Some("folders.share_not_ready")
        );
    }

    #[test]
    fn subscription_faces_forward() {
        assert_eq!(
            key(subscription_tier_form_possibility(" ".into())).as_deref(),
            Some("subscriptions.tier_name_required")
        );
        assert!(subscription_tier_form_possibility("Gold".into()).is_possible());
        assert_eq!(
            key(subscription_redeem_code_possibility("".into())).as_deref(),
            Some("subscriptions.redeem_code_required")
        );
    }
}
