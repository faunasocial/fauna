pub mod crypto;
pub mod preview;
pub mod types;

/// The reserved name of the free, always-present default subscription tier.
///
/// "Following" a creator *is* subscribing to this tier (`monetization.md`
/// § The unifying model; `feed.md` § Encryption at rest — "the always-present
/// default tier is the user's *followers*"). It is free (no `price_hint` /
/// `payment_url`), `auto_approve`, and sits at the reserved rank
/// [`FOLLOWERS_TIER_RANK`] *below* every author-created paid tier (which are
/// `rank >= 1`), so the auto-approve cascade enrolls every paid subscriber as a
/// follower too. The nest auto-provisions it on first follow; an author may not
/// create a tier with this name/rank (`tiers.create` rejects `rank < 1`).
///
/// Single source of truth for nest + clients. The per-app `FOLLOWERS_TIER`
/// literals in the five apps that reach this crate only across a boundary
/// (web `profile/[[actorId]]`, windows `ProfileViewModel.cs`, android + apple
/// `ProfileOffersVM`)
/// **intentionally stay as compile-time constants and are NOT lifted over
/// FFI/WASM** — decided 2026-07-08 (marathon test). ⚠ **The exemption is the
/// boundary, not the app count:** it is bought entirely by the FFI/WASM call
/// overhead argument below, which a **Rust-native** shell does not pay — tui
/// (`profile/mod.rs`) and linux (`views/profile/offers.rs`) both `use` this
/// constant directly and keep every bit of the compile-time semantics the
/// ruling protects. linux was corrected to that shape 2026-08-22; it had been
/// listed here as an intentional literal, which is how a rationale that never
/// applied to it went unexamined. `"followers"` is an *immutable
/// reserved wire name* (rank-0 free tier; changing it is a wire-compat break),
/// so it can never silently drift — any client that changed its literal would
/// fail immediately against the nest, making the wire contract itself the
/// enforced source of truth. UniFFI/wasm-bindgen expose *functions*, not
/// consts, so a lift would replace a zero-cost compile-time constant with a
/// runtime (async) FFI/WASM call on all 7 apps — a downgrade (call
/// overhead plus failure modes plus a permanent new FFI surface) to dedupe a
/// fixed 9-char string. A codegen-time lift would keep compile-time semantics, but no such
/// pipeline exists for wire constants (i18n is for translatable UI strings) and
/// building one for a single constant is over-engineering. Treat the per-app
/// literals as intentional, not tech debt.
pub const FOLLOWERS_TIER: &str = "followers";

/// The reserved `rank` of the free [`FOLLOWERS_TIER`] — `0`, below every
/// author-created paid tier. `tiers.create` rejects `rank < 1`, reserving this
/// slot for the system-provisioned followers tier (`monetization.md` § Pillar 1).
/// Typed `i64` to match the nest DB rank column (`SubscriptionTierRow.rank`).
pub const FOLLOWERS_TIER_RANK: i64 = 0;

/// The reserved name of the **owner-only** tier — the tier whose only reader
/// is its author. Minted once, on first need, by the archive-import machine
/// for imported content whose original audience was custom, only-me or
/// unknown (`behavior/archive-import.md` § What each category becomes →
/// *Audience mapping*); created `hidden`, so no offer surface ever lists it
/// (`behavior/monetization.md` § The unifying model — *A tier may be
/// hidden*). The author decrypts own gated posts with their own period key,
/// so the tier needs no subscriber and never gets one.
///
/// Like [`FOLLOWERS_TIER`], an immutable reserved wire name: the nest stores
/// the row under it and a client finds an existing one by it.
pub const OWNER_ONLY_TIER: &str = "only-me";

/// The reserved `rank` of [`OWNER_ONLY_TIER`] — the top of the `u32` wire
/// range, held there by two enforced checks rather than arithmetic alone
/// (`behavior/monetization.md:21`): `tiers.create` refuses any other name at or above this rank
/// (`subscription_handlers::tiers_create_handler`), and every rank-cascade
/// door — the grant-time fan-out and its creation-time and boot-time reconcile
/// twins — additionally excludes `hidden` tiers outright regardless of rank
/// (rule (e), `behavior/monetization.md` § Per-post pay-to-unlock). Typed `u32` because the client
/// mints it over the wire (`TierCreateRequest.rank`); the nest stores
/// `rank as i64`.
pub const OWNER_ONLY_TIER_RANK: u32 = u32::MAX;

/// The `GatedInfo.tier` every **room-restricted** post carries — a hard-coded
/// constant, never a tier its author owns (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the ruling*, ruling 3). Its readers are a room's floor
/// members, reached through [`types::KeyAccess::Room`], never a subscription.
///
/// Why a tier value at all: `GatedInfo.tier` is a required field every shipped
/// app decodes, and a non-empty value is what makes the nest's
/// `content_meta.gated_tier` non-NULL — which is what keeps the post out of the
/// public and trending feeds and off off-box publish with no new code. An
/// older app's card then names a tier it cannot subscribe to, which is the
/// honest degrade.
///
/// Reserved like [`FOLLOWERS_TIER`]: `tiers.create` refuses it as an author's
/// tier name, so it can never name one.
pub const ROOM_POST_TIER: &str = "room";

/// The `GatedInfo.tier_rank` of a room-restricted post — `0`, with no
/// subscription semantics: no subscriber cascade ever reaches a room post,
/// because nothing about it is keyed to a subscription.
pub const ROOM_POST_TIER_RANK: u32 = 0;

/// True iff `name` is one of the three reserved system tiers — a tier no
/// author creates and nobody pays for: the free [`FOLLOWERS_TIER`], the hidden
/// [`OWNER_ONLY_TIER`] and the constant [`ROOM_POST_TIER`]. Every other tier
/// name is an author's own, paid tier (`tiers.create` refuses `rank < 1` and
/// the reserved names — `monetization.md` § The unifying model).
///
/// The one predicate behind "may this tier's period key leave the owner's
/// apps as a capability-grant payload?" — the nest's web-serve holder exists
/// to serve **paid** web-published posts (`monetization.md` § Pillar 2), so a
/// reserved tier's key is never a grant option and never derives
/// (`ui/nests.md` § Trust facet — grants → *Mint* → *Which tiers*, ruled
/// 2026-10-23). Shared so the apps' mint catalog and the mint itself cannot
/// drift on which names are reserved.
pub fn is_reserved_tier(name: &str) -> bool {
    matches!(name, FOLLOWERS_TIER | OWNER_ONLY_TIER | ROOM_POST_TIER)
}

/// True iff `url` is safe to hand to the OS/browser's default opener as a
/// tier's or unlock-offer's `payment_url` — i.e. it starts with `https://`.
///
/// `payment_url` is author-supplied (carried on the wire from a possibly
/// compromised/spoofed nest), so it is untrusted the same way the bridge OAuth
/// `redirect_url` is: opening it unchecked lets a malicious nest redirect the
/// user to an attacker-chosen scheme (the F-CL2 anti-phishing-redirect class; see
/// `docs/goal/architecture/security.md`). This is a plain scheme-prefix check,
/// not full URL parsing: it matches the check web's
/// `#lib/safe-url.ts::isSafeNavUrl` already applies (`parsed.protocol ===
/// 'https:'`), and native callers just need the same "https or refuse" gate
/// before invoking their platform's opener.
///
/// One definition for every app that opens a `payment_url`. Landed 2026-08-10
/// after finding real drift: tui duplicated this exact check at two call
/// sites instead of sharing it (`feed/mod.rs`, `profile/mod.rs` — now both
/// call this fn); linux guarded only the feed-side open
/// (`views/feed/post_list.rs`) and was missing it on the profile-side one
/// (`views/profile/offers.rs` — both now call this fn); windows had the same
/// split (`FeedPage.xaml.cs` guarded, `ProfilePage.xaml.cs` didn't — both now
/// call `uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl`); apple's
/// `ProfileView.swift::openPayment` had no guard at all (now calls
/// `isSafePaymentUrl`). web keeps its own guard (`isSafeNavUrl`) since it
/// isn't a UniFFI/wasm consumer of this fn.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn is_safe_payment_url(url: &str) -> bool {
    url.starts_with("https://")
}

/// The checkout hosts that honour a `client_reference_id` query parameter and
/// hand it back on the payment webhook — Stripe's Payment Link and Checkout
/// hosts. A buyer's actor id is handed to no other host.
pub const CHECKOUT_REFERENCE_HOSTS: &[&str] = &["buy.stripe.com", "checkout.stripe.com"];

/// The checkout URL a buyer's app opens for an author-supplied `payment_url` —
/// the one shared rule behind Q4's primary binding (`monetization.md`
/// § Pillar 3 → *Buyer↔actor binding (Q4)* → *A checkout-bound buyer homed on
/// another nest*, rule (2)).
///
/// `None` when the URL fails [`is_safe_payment_url`] (nothing is opened). When
/// its host is one of [`CHECKOUT_REFERENCE_HOSTS`] the URL comes back with
/// `client_reference_id=<buyer hex>` added to its query (ahead of any
/// fragment, after any existing parameters); any other host — or an authority
/// carrying userinfo, which could make a lookalike read as a listed host —
/// comes back untouched, so that payment stays on the claim code.
pub fn checkout_url(payment_url: &str, buyer: &crate::identity::ActorId) -> Option<String> {
    if !is_safe_payment_url(payment_url) {
        return None;
    }
    let rest = &payment_url["https://".len()..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('@') {
        return Some(payment_url.to_string());
    }
    let host = authority.split(':').next().unwrap_or_default();
    if !CHECKOUT_REFERENCE_HOSTS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(host))
    {
        return Some(payment_url.to_string());
    }
    let (before_fragment, fragment) = match payment_url.find('#') {
        Some(i) => payment_url.split_at(i),
        None => (payment_url, ""),
    };
    let sep = if !before_fragment.contains('?') {
        "?"
    } else if before_fragment.ends_with('?') || before_fragment.ends_with('&') {
        ""
    } else {
        "&"
    };
    Some(format!(
        "{before_fragment}{sep}client_reference_id={}{fragment}",
        buyer.to_hex()
    ))
}

/// The foreign-language face of [`checkout_url`]: the buyer named by its
/// 64-hex actor id; `None` for an unsafe URL or a malformed id.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn checkout_url_for_buyer(payment_url: &str, buyer_actor_hex: &str) -> Option<String> {
    let buyer = crate::identity::ActorId::from_hex(buyer_actor_hex).ok()?;
    checkout_url(payment_url, &buyer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https() {
        assert!(is_safe_payment_url("https://checkout.example.com/pay/abc"));
    }

    #[test]
    fn rejects_non_https() {
        assert!(!is_safe_payment_url("http://checkout.example.com/pay/abc"));
        assert!(!is_safe_payment_url("javascript:alert(1)"));
        assert!(!is_safe_payment_url("data:text/html,<script>1</script>"));
        assert!(!is_safe_payment_url(""));
        assert!(!is_safe_payment_url("checkout.example.com"));
    }

    fn buyer() -> crate::identity::ActorId {
        crate::identity::ActorId([0xab; 32])
    }

    fn reference() -> String {
        format!("client_reference_id={}", "ab".repeat(32))
    }

    #[test]
    fn checkout_url_refuses_an_unsafe_url() {
        assert_eq!(checkout_url("http://buy.stripe.com/x", &buyer()), None);
        assert_eq!(checkout_url("javascript:alert(1)", &buyer()), None);
    }

    #[test]
    fn checkout_url_appends_the_reference_on_a_stripe_host() {
        assert_eq!(
            checkout_url("https://buy.stripe.com/test_abc", &buyer()).unwrap(),
            format!("https://buy.stripe.com/test_abc?{}", reference())
        );
        assert_eq!(
            checkout_url("https://CHECKOUT.stripe.com:443/c/pay", &buyer()).unwrap(),
            format!("https://CHECKOUT.stripe.com:443/c/pay?{}", reference())
        );
    }

    #[test]
    fn checkout_url_keeps_an_existing_query_and_the_fragment() {
        assert_eq!(
            checkout_url("https://buy.stripe.com/abc?prefilled_email=x", &buyer()).unwrap(),
            format!(
                "https://buy.stripe.com/abc?prefilled_email=x&{}",
                reference()
            )
        );
        assert_eq!(
            checkout_url("https://buy.stripe.com/abc#frag", &buyer()).unwrap(),
            format!("https://buy.stripe.com/abc?{}#frag", reference())
        );
        assert_eq!(
            checkout_url("https://buy.stripe.com/abc?a=1#frag", &buyer()).unwrap(),
            format!("https://buy.stripe.com/abc?a=1&{}#frag", reference())
        );
        assert_eq!(
            checkout_url("https://buy.stripe.com?", &buyer()).unwrap(),
            format!("https://buy.stripe.com?{}", reference())
        );
    }

    #[test]
    fn checkout_url_leaves_any_other_host_untouched() {
        for url in [
            "https://ko-fi.com/author",
            "https://buy.stripe.com.evil.test/abc",
            "https://evil.test/buy.stripe.com",
            "https://buy.stripe.com@evil.test/abc",
            "https://stripe.com/pay",
        ] {
            assert_eq!(checkout_url(url, &buyer()).as_deref(), Some(url), "{url}");
        }
    }

    #[test]
    fn checkout_url_for_buyer_reads_the_hex_id() {
        assert_eq!(
            checkout_url_for_buyer("https://buy.stripe.com/x", &"ab".repeat(32)).unwrap(),
            format!("https://buy.stripe.com/x?{}", reference())
        );
        assert_eq!(
            checkout_url_for_buyer("https://buy.stripe.com/x", "zz"),
            None
        );
    }

    #[test]
    fn the_reserved_tiers_are_exactly_the_three_system_names() {
        assert!(is_reserved_tier(FOLLOWERS_TIER));
        assert!(is_reserved_tier(OWNER_ONLY_TIER));
        assert!(is_reserved_tier(ROOM_POST_TIER));
        // An author's own tier, a designated single-post tier and a near miss
        // are all paid tiers as far as the grant plane is concerned.
        assert!(!is_reserved_tier("gold"));
        assert!(!is_reserved_tier("post-unlock-0123abcd"));
        assert!(!is_reserved_tier("Followers"));
        assert!(!is_reserved_tier(""));
    }

    #[test]
    fn the_owner_only_tier_sits_above_every_rank_a_subscriber_can_reach() {
        assert_eq!(OWNER_ONLY_TIER, "only-me");
        assert_eq!(OWNER_ONLY_TIER_RANK, u32::MAX);
        assert!(i64::from(OWNER_ONLY_TIER_RANK) > FOLLOWERS_TIER_RANK);
        assert_ne!(OWNER_ONLY_TIER, FOLLOWERS_TIER);
        // `tiers.create` refuses this rank for every name but the reserved
        // `OWNER_ONLY_TIER`; the reserved name itself
        // remains creatable at it — a constant fact, so not asserted (clippy
        // denies a constant assertion).
        // `subscription_handlers::tests::tiers_create_persists_the_hidden_flag`
        // is the positive control (creates at this rank through the reserved
        // name);
        // `tiers_create_rejects_rank_at_the_owner_only_ceiling_for_an_ordinary_name`
        // is the negative control.
    }
}
