//! Client render-enforcement state + verdict, shared across tui's two social
//! surfaces (`docs/goal/behavior/family-safety.md` § Content policy). The tui
//! twin of `apps/fauna-linux/src/content_policy.rs` and web's
//! `contentPolicy.svelte.ts`.
//!
//! **Every decision is shared Rust — including the orchestration, not just the
//! composition.** [`ContentPolicyState`] wraps
//! [`fauna_core::obligation::ContentPolicyState`]: the three-field wiring
//! (own thresholds, guardian floor, Notify counter) around
//! [`fauna_core::obligation::render_verdict_composed`]/
//! [`fauna_core::obligation::guardian_enforced_categories`]/
//! [`fauna_core::obligation::NotifyAccumulator`] was hand-rolled once per app
//! (tui, linux), byte-identically save for storage — the same shape
//! [`crate::screen_lock`] states for the screen-time pillar (and was lifted
//! the same session, for the same reason). So tui cannot drift from linux,
//! web or the native apps on how a content floor is enforced or counted, nor
//! on the wiring around it (priority #2/#4).
//!
//! **The engine is general, not guardian-only.** § Content policy is explicit
//! that the own-threshold collapse is built "once, for every user" — the
//! un-darking of `moderation.md` § Categories & enforcement item 1 — and the
//! guardian floor composes *on top* for a supervised account. Both halves are
//! hydrated here, so an unsupervised tui user gets their own thresholds honoured
//! exactly as a ward does.
//!
//! **The `RefCell` is tui's own, not the shared type's.** § Guardian Notify's
//! counting must happen at the *render* call site — "counting and rendering
//! can never diverge" is load-bearing, which holds precisely because
//! [`Self::note_enforcement`] sits beside [`Self::verdict_for`] on the same
//! item. tui's paint path is `elements(app: &App)` (immutable, and rightly
//! so: a repaint must not be able to mutate page state), so the whole shared
//! state lives behind one `RefCell` here — linux reaches the same shape from
//! the other direction, with its own instance in a `thread_local!`. Every
//! borrow is taken and released within one method and never spans a call
//! back into paint, so it cannot re-enter.

use fauna_client_family::family::FamilyContentNotice;
use fauna_client_spam::spam::SpamPreferences;
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{ComposedVerdict, ContentPolicy, ViewerThresholds};

/// The viewer's content render-enforcement state: their own thresholds, any
/// guardian floor, and the Guardian Notify counter — a `RefCell` around the
/// shared [`fauna_core::obligation::ContentPolicyState`] (see the module
/// docs for why the interior mutability lives here rather than there).
///
/// Default = no thresholds, no floor, counting off = every item renders normally,
/// which is also what an identity change resets to
/// ([`crate::app::App::drop_authenticated_state`]).
#[derive(Debug, Default)]
pub struct ContentPolicyState(std::cell::RefCell<fauna_core::obligation::ContentPolicyState>);

impl ContentPolicyState {
    /// Epoch seconds and the device's local UTC offset in minutes.
    ///
    /// The offset comes from the app's single device-offset door
    /// ([`crate::settings::logs::local_offset_secs`]) rather than a second
    /// `chrono::Local::now()` read — the same shape linux's twin routes
    /// through its own door (`crate::logs_view::local_offset_secs`), so
    /// tui cannot drift from its own documented "ONE device-offset door"
    /// the way linux once drifted between two independent offset reads.
    fn now_and_offset() -> (i64, i32) {
        let now = chrono::Utc::now().timestamp();
        let offset = crate::settings::logs::local_offset_secs() / 60;
        (now, offset)
    }

    /// Cache the viewer's own spam/phishing thresholds (from the post-auth
    /// `fauna.spam.get_preferences` read). A later render composes them via
    /// [`Self::verdict_for`]. `None` clears them.
    pub fn set_spam_preferences(&self, prefs: Option<&SpamPreferences>) {
        let thresholds = prefs.map(|p| ViewerThresholds {
            spam_permille: p.spam_threshold,
            phishing_permille: p.phishing_threshold,
        });
        self.0.borrow_mut().set_spam_preferences(thresholds);
    }

    /// Set the supervised viewer's guardian content policy (from the
    /// `fauna.family.status` read). A later render enforces it via
    /// [`Self::verdict_for`]. `None` clears the floor (unsupervised, or a
    /// graduated ward).
    pub fn set_ward_content_policy(&self, policy: Option<ContentPolicy>) {
        self.0.borrow_mut().set_ward_content_policy(policy);
    }

    /// Set whether the guardian's **Guardian Notify** knob (`content_notify`) is
    /// on, from the `fauna.family.status` read (§ Guardian Notify). Ward-side
    /// counting runs only while on; turning it off drops future counting,
    /// pending counts included.
    pub fn set_ward_content_notify(&self, on: bool) {
        self.0.borrow_mut().set_ward_content_notify(on);
    }

    /// Set the region content policies in force on the declared region's
    /// chain, most specific first — the engine's third source
    /// (`region-blocking.md` § Where it composes). Fed from
    /// `crate::region::apply_rule_sets`; replaces, never merges.
    pub fn set_region_policies(&self, policies: Vec<fauna_core::region_policy::RegionRuleSet>) {
        self.0.borrow_mut().set_region_policies(policies);
    }

    /// The content-policy render verdict for a piece of content, keyed on the
    /// lightweight [`ContentLabelEntry`] list both surfaces already hold per
    /// post/message (§ Content policy).
    ///
    /// [`RenderVerdict::Show`](fauna_core::obligation::RenderVerdict::Show) when
    /// no source is set; a present-but-unescalated label renders
    /// [`RenderVerdict::Badge`](fauna_core::obligation::RenderVerdict::Badge), which the surfaces treat like
    /// `Show` (the `content-label-badge` they already paint).
    ///
    /// Returns the driving **source** beside the verdict so a placeholder can
    /// name whose rule blocked the item — a region and its authority, or the
    /// family policy (`region-blocking.md` § Where it composes). A surface that
    /// only needs the verb reads `.verdict`.
    pub fn verdict_for(&self, labels: &[ContentLabelEntry]) -> ComposedVerdict {
        self.0.borrow().verdict_for(labels)
    }

    /// [`Self::verdict_for`] for one identified item, plus the viewer's own
    /// reports (`moderation.md` § Corollary): an item the viewer reported, or
    /// whose author they reported, is a `Block` attributed `Reported`.
    pub fn verdict_for_item(
        &self,
        item_id: &str,
        author_id: Option<&str>,
        labels: &[ContentLabelEntry],
    ) -> ComposedVerdict {
        self.0.borrow().verdict_for_item(item_id, author_id, labels)
    }

    /// Replace the viewer's reported-and-hidden ids — the stored list the
    /// login read, or a report's hide, returns (`crate::report`).
    pub fn set_hidden_content(&self, ids: Vec<String>) {
        self.0.borrow_mut().set_hidden_content(ids);
    }

    /// Record any **guardian-floor** render-enforcement on `item_id` for Guardian
    /// Notify (§ Guardian Notify). Called from both render sites for every item;
    /// a no-op unless the ward's `content_notify` knob is on AND the guardian
    /// floor bites on one of this item's labels (never the ward's own-threshold
    /// collapses). Deduped per item per local day, so a re-render never
    /// re-counts.
    pub fn note_enforcement(&self, item_id: &str, labels: &[ContentLabelEntry]) {
        let (now, offset) = Self::now_and_offset();
        self.0
            .borrow_mut()
            .note_enforcement(item_id, labels, now, offset);
    }

    /// Drain the batched Guardian Notify report if a flush is due (≤ hourly, the
    /// accumulator's own gate). The one-minute tick calls this and, on `Some`,
    /// sends `fauna.family.notify_report(entries, offset)`.
    pub fn take_notify_report(&self) -> Option<(Vec<FamilyContentNotice>, i32)> {
        let (now, _offset) = Self::now_and_offset();
        let (entries, offset) = self.0.borrow_mut().take_notify_report(now)?;
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
}

/// Fire-and-forget the post-auth `fauna.spam.get_preferences` read that hydrates
/// the viewer's OWN thresholds (`family-safety.md` § Content policy — the
/// every-user own-threshold collapse that un-darks `moderation.md` § Categories &
/// enforcement item 1).
///
/// Fetched at login rather than on a nav edge because it gates a *render* on two
/// pages, not a page's own data: deferring it would let the first feed paint of
/// every session miss the viewer's thresholds, which fails **open** — content
/// they asked to have collapsed would flash before the read landed. The family
/// half rides `fauna.family.status`, which is fetched here for the same reason.
///
/// A failure leaves the thresholds unset, which composes no own-threshold rule.
/// That is the honest degrade (the same one an account that never set thresholds
/// gets) and is logged rather than surfaced: the user did not ask for this read,
/// and any guardian floor still binds independently.
pub fn spawn_spam_preferences_check(
    nest: std::sync::Arc<fauna_client::NestClient>,
    tx: &tokio::sync::mpsc::UnboundedSender<crate::app::UiMessage>,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let client = fauna_client_spam::SpamClient::new(nest);
        let req = fauna_protocol::spam::SpamGetPreferencesRequest {
            extra: Default::default(),
        };
        match client.get_preferences(req).await {
            Ok(prefs) => {
                let _ = tx.send(crate::app::UiMessage::Data(
                    crate::app::DataMessage::SpamPreferencesLoaded(prefs),
                ));
            }
            Err(e) => {
                tracing::warn!(
                    "fauna.spam.get_preferences failed; own-threshold content \
                     collapse is inactive this session: {e}"
                );
            }
        }
    });
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

    /// The offset this door produces rides `fauna.family.notify_report`,
    /// where the nest clamps it to `[-720, 840]` before persisting it as
    /// the ward's `guardianships.ward_utc_offset_minutes`
    /// (`bins/fauna-nest/src/family_handlers.rs` → `clamp_utc_offset`). A
    /// producer that could outrun that range would have the ward's local
    /// day moved by the clamp instead of by the device. Pin that this
    /// producer cannot: the nest's clamp is a no-op on what it sends —
    /// linux's twin (`content_policy.rs::now_and_offset`) carries the same
    /// pin over its own door.
    #[test]
    fn the_device_offset_is_one_the_nest_accepts_unchanged() {
        let (_, offset) = ContentPolicyState::now_and_offset();
        assert_eq!(fauna_core::day_bucket::clamp_utc_offset(offset), offset);
    }

    // The composition/counting rules themselves (own-threshold-for-every-user,
    // guardian-wins, fail-closed-on-Unknown, the Notify knob/dedup/flush gate)
    // are `fauna_core::obligation::tests::content_policy_state_*`'s own
    // coverage now. What's left here is tui's own wiring: the real
    // `SpamPreferences` wire mapping, the `&self`/`RefCell` interior
    // mutability the render path depends on, and the `FamilyContentNotice`
    // mapping on the way out.
    #[test]
    fn the_wrapper_reaches_the_shared_core_through_a_shared_reference() {
        let state = ContentPolicyState::default();
        // `&self` throughout — this is what makes it callable from
        // `elements(app: &App)`. If any method here needed `&mut self`, this
        // line would not compile.
        let state: &ContentPolicyState = &state;

        // The wire mapping: SpamPreferences' threshold fields land in the
        // right ViewerThresholds slots, not swapped.
        state.set_spam_preferences(Some(&prefs(500, 300)));
        assert_eq!(
            state.verdict_for(&[entry("spam", 900)]).verdict,
            RenderVerdict::Collapse,
            "the spam threshold must gate the spam category"
        );
        assert_eq!(
            state.verdict_for(&[entry("phishing", 900)]).verdict,
            RenderVerdict::Collapse,
            "the phishing threshold must gate the phishing category"
        );

        // A guardian floor composes on top and is what Notify counts.
        state.set_ward_content_policy(Some(ContentPolicy {
            spam: ContentFloor::Block,
            ..Default::default()
        }));
        state.set_ward_content_notify(true);
        state.note_enforcement("p1", &[entry("spam", 900)]);
        let (entries, _offset) = state
            .take_notify_report()
            .expect("a due report after one enforcement");
        // The FamilyContentNotice mapping: category/count carried through,
        // not left as the shared core's raw (&'static str, u32) tuple.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, "spam");
        assert_eq!(entries[0].count, 1);
    }
}
