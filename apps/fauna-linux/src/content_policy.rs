//! Client render-enforcement state + verdict, shared across the social surfaces
//! (`family-safety.md` § Content policy). The shared engine
//! [`fauna_core::obligation::render_verdict_composed`] is the pure computation;
//! this module holds the GTK-main-thread render *state* the verdict composes —
//! the viewer's own spam/phishing thresholds and, for a supervised account, the
//! guardian floor — and exposes the one [`verdict_for`] both the feed
//! (`views/feed/post_list.rs`) and conversations
//! (`views/conversations/message_bubble.rs`) call, so the two surfaces can never
//! drift on how a content floor is enforced (priority #2/#4).
//!
//! **Every decision is shared Rust — including the orchestration, not just the
//! composition.** [`fauna_core::obligation::ContentPolicyState`] bundles the
//! three-field wiring (own thresholds, guardian floor, Notify counter) around
//! [`render_verdict_composed`]/[`guardian_enforced_categories`]/
//! [`NotifyAccumulator`] — lifted from an identical private copy this module
//! and tui's `content_policy.rs` each carried (the same shape
//! [`crate::screen_lock`] states for the screen-time pillar, lifted the same
//! session). This module now holds only the `thread_local!` storage strategy
//! (tui threads an ordinary `&mut App` field behind its own `RefCell`
//! instead, since GTK hands this module's callbacks no context to thread one
//! through) plus the wire-type mapping at the edges.

use std::cell::RefCell;

use fauna_client_family::family::FamilyContentNotice;
use fauna_client_spam::spam::SpamPreferences;
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{ComposedVerdict, ContentPolicy, ViewerThresholds};

thread_local! {
    /// The viewer's content render-enforcement state — own thresholds,
    /// guardian floor, and the Guardian Notify counter, all in one instance.
    static CORE: RefCell<fauna_core::obligation::ContentPolicyState> =
        RefCell::new(fauna_core::obligation::ContentPolicyState::default());
}

/// The current epoch seconds and the device's local UTC offset in minutes.
///
/// The offset comes from the app's single device-offset door
/// ([`crate::logs_view::local_offset_secs`]) — glib's timezone-aware local
/// clock, so no timezone dep is added, and an unreadable zone degrades to `0`
/// (UTC), the nest's documented additive fallback. Reading it here rather than
/// re-deriving it keeps one glib offset call in the app: this door used to
/// format `%z` and re-parse the string, which was a second, strictly more
/// fragile way to compute a value the app already had.
pub(crate) fn now_and_offset() -> (i64, i32) {
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    (now, crate::logs_view::local_offset_secs() / 60)
}

/// Cache the viewer's own spam/phishing thresholds (from the post-auth
/// `fauna.spam.get_preferences` read). A later render composes them via
/// [`verdict_for`]. `None` clears them.
pub fn set_spam_preferences(prefs: Option<SpamPreferences>) {
    let thresholds = prefs.map(|p| ViewerThresholds {
        spam_permille: p.spam_threshold,
        phishing_permille: p.phishing_threshold,
    });
    CORE.with(|c| c.borrow_mut().set_spam_preferences(thresholds));
}

/// Set the supervised viewer's guardian content policy (from the post-auth
/// `fauna.family.status` read). A later render — feed rebuild, conversation
/// bubble build, notify, or test inject — enforces it via [`verdict_for`].
/// `None` clears the floor (unsupervised, or a graduated ward).
pub fn set_ward_content_policy(policy: Option<ContentPolicy>) {
    CORE.with(|c| c.borrow_mut().set_ward_content_policy(policy));
}

/// The content-policy render verdict for a piece of content, keyed on the
/// lightweight [`ContentLabelEntry`] list every app already holds per
/// post/message (`family-safety.md` § Content policy).
///
/// [`RenderVerdict::Show`](fauna_core::obligation::RenderVerdict::Show) when
/// no source fires (a present-but-unescalated label renders
/// [`RenderVerdict::Badge`](fauna_core::obligation::RenderVerdict::Badge), which
/// the surfaces treat like `Show`).
///
/// Returns the driving **source** beside the verdict so a placeholder can name
/// whose rule blocked the item — a region and its authority, or the family
/// policy (`region-blocking.md` § Where it composes). A surface that only needs
/// the verb reads `.verdict`.
pub fn verdict_for(labels: &[ContentLabelEntry]) -> ComposedVerdict {
    CORE.with(|c| c.borrow().verdict_for(labels))
}

/// Set the region content policies in force — every region on the device's
/// declared chain, most specific first (`crate::region::apply_rule_sets`).
/// Replaces, never merges.
pub fn set_region_policies(policies: Vec<fauna_core::region_policy::RegionRuleSet>) {
    CORE.with(|c| c.borrow_mut().set_region_policies(policies));
}

/// Set whether the guardian's **Guardian Notify** knob (`content_notify`) is on,
/// from the post-auth `fauna.family.status` read (`family-safety.md` § Guardian
/// Notify). Ward-side counting runs only while on; turning it off drops future
/// counting, pending counts included.
pub fn set_ward_content_notify(on: bool) {
    CORE.with(|c| c.borrow_mut().set_ward_content_notify(on));
}

/// Record any **guardian-floor** render-enforcement on `item_id` for Guardian
/// Notify (`family-safety.md` § Guardian Notify). Called from the feed + conversations
/// render sites for every item; a no-op unless the ward's `content_notify` knob is on
/// AND the guardian floor bites on one of this item's labels (never the ward's
/// own-threshold collapses). Deduped per item per local day, so a re-render
/// never re-counts.
pub fn note_enforcement(item_id: &str, labels: &[ContentLabelEntry]) {
    let (now, offset) = now_and_offset();
    CORE.with(|c| {
        c.borrow_mut()
            .note_enforcement(item_id, labels, now, offset)
    });
}

/// Drop every field of the shared content-policy state on an actor change
/// (`crate::actor_scope`), registered on the canonical drop list.
///
/// All three are one account's state — the viewer's own thresholds, their
/// guardian's content floor, and the ward-side Notify counter — so
/// `account-scoping.md`'s in-memory corollary makes them class-1 exactly as
/// their on-disk twins would be. Linux tears down and re-launches **in
/// process** on sign-out, factory reset and account switch
/// (`main.rs::launch_authenticated` builds a fresh `FaunaClient` on each), so
/// nothing here is dropped by the window teardown that surrounds those paths.
///
/// Each of the three leaks differently, which is why the drop is unconditional:
///
///   * **the pending Notify counts** — a Notify report carries no identity of
///     its own, so counts accrued by the outgoing ward are attributed to
///     whoever is signed in when they drain: a cross-actor leak *and* a
///     misattribution.
///   * **the Notify dedup set** — the sharp edge, because it fails
///     *silently*: a per-item/per-local-day dedup carried across a switch
///     makes the incoming ward's first enforcement on an item the outgoing
///     one already saw go uncounted, an under-report to their guardian that
///     looks exactly like "nothing happened".
///   * **the guardian floor / own thresholds** — the outgoing account's
///     guardian floor keeps binding the incoming one until its own post-auth
///     read lands, so supervised→unsupervised over-enforces and
///     unsupervised→supervised under-enforces (a family-safety failure, not a
///     cosmetic one).
///
/// Pending counts are dropped rather than flushed, matching web's
/// `familyNotify.ts::resetForActorChange`: they were accrued under the outgoing
/// actor, and Guardian Notify is explicitly coarse and best-effort
/// (`family-safety.md` § Guardian Notify trust bound), so losing a partial
/// bucket at a switch is within its contract — attributing it to the wrong
/// actor would not be.
pub fn clear_for_identity_change() {
    CORE.with(|c| c.borrow_mut().clear_for_identity_change());
}

/// Drain the batched Guardian Notify report if a flush is due (≤ hourly, the
/// accumulator's own gate). The app's notify-flush tick calls this on the GTK main
/// thread and, on `Some`, sends `fauna.family.notify_report(entries, offset)`.
pub fn take_notify_report() -> Option<(Vec<FamilyContentNotice>, i32)> {
    let (now, _offset) = now_and_offset();
    let (entries, offset) = CORE.with(|c| c.borrow_mut().take_notify_report(now))?;
    let entries = entries
        .into_iter()
        .map(|(category, count)| FamilyContentNotice {
            category: category.to_string(),
            count,
            ..Default::default()
        })
        .collect();
    Some((entries, offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::obligation::{ContentFloor, RenderVerdict};

    fn entry(category: &str, per_mille: u16) -> ContentLabelEntry {
        ContentLabelEntry {
            category: category.into(),
            confidence_per_mille: per_mille,
        }
    }

    fn prefs(spam: u16, phishing: u16) -> SpamPreferences {
        SpamPreferences {
            spam_threshold: spam,
            phishing_threshold: phishing,
            extra: Default::default(),
        }
    }

    // Each test runs on its own thread, so the thread-local starts clean;
    // clear it at the end anyway so a future same-thread test never inherits
    // state.
    fn reset() {
        clear_for_identity_change();
    }

    /// The offset this door produces rides `fauna.family.usage_report` and
    /// `fauna.family.notify_report`, where the nest clamps it to
    /// `[-720, 840]` before persisting it as the ward's
    /// `guardianships.ward_utc_offset_minutes`
    /// (`bins/fauna-nest/src/family_handlers.rs` → `clamp_utc_offset`). A
    /// producer that could outrun that range would have the ward's local day
    /// moved by the clamp instead of by the device — silently shifting when a
    /// screen-time budget resets and when a usage window applies. Pin that
    /// this producer cannot: the nest's clamp is a no-op on what it sends —
    /// tui's twin (`content_policy.rs::ContentPolicyState::now_and_offset`)
    /// carries the same pin over its own door.
    #[test]
    fn the_device_offset_is_one_the_nest_accepts_unchanged() {
        let (_, offset) = now_and_offset();
        assert_eq!(fauna_core::day_bucket::clamp_utc_offset(offset), offset);
    }

    // The composition/counting rules themselves (own-threshold-for-every-user,
    // guardian-wins, fail-closed-on-Unknown, the Notify knob/dedup/flush gate,
    // the identity-change reset) are
    // `fauna_core::obligation::tests::content_policy_state_*`'s own coverage
    // now. What's left here is linux's own wiring: the real `SpamPreferences`
    // wire mapping, the `thread_local!` delegation, and the
    // `FamilyContentNotice` mapping on the way out.
    #[test]
    fn the_thread_local_wiring_reaches_the_shared_core() {
        reset();

        // The wire mapping: SpamPreferences' threshold fields land in the
        // right ViewerThresholds slots, not swapped.
        set_spam_preferences(Some(prefs(500, 300)));
        assert_eq!(
            verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Collapse,
            "the spam threshold must gate the spam category"
        );
        assert_eq!(
            verdict_for(&[entry("phishing", 900)]).verdict,
            RenderVerdict::Collapse,
            "the phishing threshold must gate the phishing category"
        );

        // A guardian floor composes on top and is what Notify counts.
        set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        set_ward_content_notify(true);
        note_enforcement("p1", &[entry("spam", 900)]);
        let (entries, _offset) = take_notify_report().expect("a due report after one enforcement");
        // The FamilyContentNotice mapping: category/count carried through,
        // not left as the shared core's raw (&'static str, u32) tuple.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, "spam");
        assert_eq!(entries[0].count, 1);

        reset();
    }
}
