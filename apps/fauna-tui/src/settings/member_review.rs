//! The Settings → **Members To Review** sub-page (`ui.yaml` page
//! `member_review`; nav id `member-review`) — the **permanent** half of the
//! post-succession unattested-member review, and item (iv) of
//! `succession-aftermath.md` § Propagation's two-surface ruling.
//!
//! **What makes it the permanent one.** The ephemeral kit-side pass
//! ([`super::recovery::member_review_elements`]) is *"shown once per recovery,
//! in the ceremony flow that produced it"* — it renders only while
//! `App::succession_sweep` is live and the user has not deferred. Everything
//! that pass left unanswered stays open on the succession ledger (`fauna.state.succession-ledger`), and **this page is where
//! it lives from then on**. That is the whole meaning of *Review The Rest
//! Later*: without a permanent view, deferring would drop the backlog out of
//! sight until the next ceremony raised it again.
//!
//! **One family, two surfaces — the rows here are literally the same rows.**
//! [`review_rows`] below builds them and both pages call it, so a driver
//! reading `member-review-row[i]` is reading the same markup on either, and a
//! reworded verdict cannot come to mean one thing inside the ceremony and
//! another a month later. The pages differ in exactly two ways, both of them
//! deliberate: this one has **no** `member-review-defer-button` (deferring is
//! what sent the backlog here — a second postponement would defer it to
//! itself), and it has `member-review-empty`, which the ephemeral pass has no
//! use for because a pass with nothing to ask should not be on screen at all.
//!
//! **Empty is the ordinary state, and `member-review-empty` is not a safety
//! verdict.** The page holds only a backlog somebody explicitly postponed, so
//! it is empty before any recovery and empty again once the backlog is worked
//! through — which is why it needs no dismiss affordance of its own and never
//! accumulates standing clutter. ⚠ The empty line must never read as reassurance:
//! `succession-aftermath.md` § Implementation status today (the sweep-driver
//! bullet) leaves deliberately no combined "is the user safe" boolean for a
//! surface to round up to, and an empty review list is not one either — it says
//! nothing at all about whether the account is safe.
//!
//! **The verdict is DERIVED.** *Remove* here drives
//! [`fauna_conversations::ConversationsManager::evict_person_everywhere`] over
//! the groups the person is in **now**, re-derived, and records only what
//! `CrossGroupEviction::earned_verdict()` earned. A partial eviction earns
//! none, so the row **stays** and the page's `error-message` says how far it
//! got. This page constructs no verdict of its own — it shares the ephemeral
//! pass's `Action::MemberReviewKeep` / `MemberReviewRemove` ops, which is the
//! structural reason it cannot.

use fauna_i18n::strings::common;
use fauna_i18n::strings::settings::member_review_page as page;
use fauna_i18n::strings::settings::recovery_kit as t;

use fauna_ui_ids as ids;

use super::Action;
use crate::element::{Element, Gesture};

/// One `member-review-row` per still-open **person**, with that person's
/// `member-review-keep-button` / `member-review-remove-button` pair scoped
/// **inside** it.
///
/// **Shared by both review surfaces on purpose** (`settings.md` § Recovery kit
/// → *The ephemeral member-review pass*: "the same four IDs serve the permanent
/// view when it lands — one family, two surfaces"). The ephemeral kit-side pass
/// wraps this in its own gate and appends the defer button; this page wraps it
/// in an empty-state check. Neither owns the row.
///
/// One row per person and never one per item: the collapse already happened in
/// `SuccessionLedger::open_member_reviews`, because the owner's
/// decision (do I trust this person) is singular and two rows for one human
/// reads as being asked the same question twice.
///
/// The `manager` is what resolves a person to the handle they are seated under.
/// `None` — pre-auth, or conversations still coming up — renders the row anyway
/// under the "no longer in any of your groups" wording, for the same reason a
/// person who has since left every group still gets a row: **an item nobody can
/// name is an item nobody can close.**
pub(super) fn review_rows(
    reviews: &[fauna_core::data::MemberReview],
    manager: Option<&std::sync::Arc<fauna_conversations::ConversationsManager>>,
) -> Vec<Element> {
    let mut els = Vec::new();
    for (i, review) in reviews.iter().enumerate() {
        let handle = manager.and_then(|m| m.handle_for_person(&review.person));
        let parts = fauna_core::data::review_row_text(review, handle.as_deref());
        els.push(Element::label(
            ids::MEMBER_REVIEW_ROW,
            t::review_row(
                &parts.who.resolve(fauna_i18n::strings::lookup),
                &fauna_core::data::reason_text(&parts.reasons, fauna_i18n::strings::lookup),
            ),
        ));
        els.push(
            Element::gesture_button(
                ids::MEMBER_REVIEW_KEEP_BUTTON,
                t::REVIEW_KEEP,
                true,
                Gesture::Settings(Action::MemberReviewKeep {
                    person: review.person,
                }),
            )
            .within(ids::MEMBER_REVIEW_ROW, i),
        );
        els.push(
            Element::gesture_button(
                ids::MEMBER_REVIEW_REMOVE_BUTTON,
                t::REVIEW_REMOVE,
                true,
                Gesture::Settings(Action::MemberReviewRemove {
                    person: review.person,
                }),
            )
            .within(ids::MEMBER_REVIEW_ROW, i),
        );
    }
    els
}

/// The page.
///
/// ⚠ **There is no sweep gate here, and that is the entire point.** The
/// ephemeral pass gates on `App::succession_sweep` so it appears only inside
/// the ceremony; adding the same gate here would leave a deferred backlog
/// unreachable the moment the app restarts, which is precisely the hole (iv)
/// was ratified to close. The only condition this page has is whether there is
/// anything open.
pub(super) fn member_review_elements(
    reviews: &[fauna_core::data::MemberReview],
    manager: Option<&std::sync::Arc<fauna_conversations::ConversationsManager>>,
) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, page::TITLE),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    ];
    if reviews.is_empty() {
        els.push(Element::label(ids::MEMBER_REVIEW_EMPTY, page::EMPTY));
        return els;
    }
    // The lead line — the one thing the ephemeral pass never has to say. A user
    // opening this page weeks later has no ceremony around them to explain why
    // these names are here, so the page says it. Chrome rather than an id: it
    // is prose the user reads, and ui.yaml scopes no id for it (the
    // `review_intro` shape one surface up).
    els.push(Element::chrome(page::INTRO));
    els.extend(review_rows(reviews, manager));
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{MemberReview, MemberUnattestedReason};
    use fauna_core::identity::ActorId;

    fn person(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    fn open_review(b: u8) -> MemberReview {
        MemberReview {
            person: person(b),
            reasons: vec![MemberUnattestedReason::CompromiseWindow],
        }
    }

    fn element_ids(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn an_empty_roster_paints_the_empty_state_and_no_rows() {
        let els = member_review_elements(&[], None);
        assert!(
            element_ids(&els).contains(&"member-review-empty"),
            "ids: {:?}",
            element_ids(&els)
        );
        assert!(
            !element_ids(&els).contains(&"member-review-row"),
            "rows and the empty line are mutually exclusive; ids: {:?}",
            element_ids(&els)
        );
    }

    #[test]
    fn open_items_paint_rows_and_no_empty_state() {
        let els = member_review_elements(&[open_review(1), open_review(2)], None);
        assert_eq!(
            els.iter().filter(|e| e.id == "member-review-row").count(),
            2
        );
        assert!(
            !element_ids(&els).contains(&"member-review-empty"),
            "the empty line must not paint over a live backlog; ids: {:?}",
            element_ids(&els)
        );
    }

    /// The page's whole reason to exist: it renders a backlog with **no
    /// ceremony in sight**. A mutation adding a sweep/deferred gate here — the
    /// ephemeral pass's gate, copied one surface over — must red this.
    #[test]
    fn rows_render_with_no_sweep_and_no_ceremony_state_of_any_kind() {
        // `member_review_elements` takes the roster and the manager and nothing
        // else — there is deliberately no sweep, no `deferred` flag, and no
        // `App` in the signature to reach one through.
        let els = member_review_elements(&[open_review(7)], None);
        assert_eq!(
            els.iter().filter(|e| e.id == "member-review-row").count(),
            1,
            "a deferred backlog must survive the ceremony that raised it"
        );
    }

    #[test]
    fn the_keep_remove_pair_is_scoped_inside_its_own_row() {
        let els = member_review_elements(&[open_review(1), open_review(2)], None);
        for id in ["member-review-keep-button", "member-review-remove-button"] {
            let scopes: Vec<_> = els
                .iter()
                .filter(|e| e.id == id)
                .map(|e| e.path.clone())
                .collect();
            assert_eq!(
                scopes,
                vec![
                    vec![("member-review-row".to_string(), 0)],
                    vec![("member-review-row".to_string(), 1)],
                ],
                "{id} must be scoped in the row it belongs to, in row order"
            );
        }
    }

    /// Deferring is what SENT the backlog here, so offering it again would
    /// defer the page to itself (`ui.yaml` `member-review-defer-button`: "Not
    /// present on the permanent page").
    #[test]
    fn there_is_no_defer_button() {
        let els = member_review_elements(&[open_review(1)], None);
        assert!(!element_ids(&els).contains(&"member-review-defer-button"));
    }

    #[test]
    fn a_person_no_manager_can_name_still_gets_a_closable_row() {
        let els = member_review_elements(&[open_review(3)], None);
        let row = els
            .iter()
            .find(|e| e.id == "member-review-row")
            .expect("the row renders");
        assert!(
            row.text.contains(t::REVIEW_UNKNOWN_PERSON),
            "an item nobody can name is an item nobody can close; text: {}",
            row.text
        );
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "member-review-keep-button"
                    || e.id == "member-review-remove-button")
                .count(),
            2,
            "the unnamed row keeps both answers"
        );
    }

    #[test]
    fn every_page_paints_a_real_heading_and_a_way_back() {
        for roster in [vec![], vec![open_review(1)]] {
            let els = member_review_elements(&roster, None);
            assert_eq!(els[0].id, "page-heading");
            assert_eq!(els[0].text, page::TITLE);
            assert!(element_ids(&els).contains(&"settings-nav-back"));
        }
    }

    /// The empty line states a fact about the review list and claims nothing
    /// about the account's safety (`succession-aftermath.md` § Implementation
    /// status today — there is deliberately no combined "is the user safe"
    /// boolean for a surface to round a count up to).
    #[test]
    fn the_empty_line_is_not_a_safety_verdict() {
        let els = member_review_elements(&[], None);
        let empty = els
            .iter()
            .find(|e| e.id == "member-review-empty")
            .expect("the empty line renders");
        let text = empty.text.to_lowercase();
        for claim in ["safe", "secure", "protected", "all clear", "you're good"] {
            assert!(
                !text.contains(claim),
                "the empty line must not read as reassurance; found {claim:?} in {:?}",
                empty.text
            );
        }
    }

    /// An unnamed reason is still a person the owner must decide about.
    #[test]
    fn an_unknown_reason_is_rendered_not_dropped() {
        let review = MemberReview {
            person: person(9),
            reasons: vec![MemberUnattestedReason::Other("from-a-newer-build".into())],
        };
        let els = member_review_elements(&[review], None);
        let row = els
            .iter()
            .find(|e| e.id == "member-review-row")
            .expect("the row renders");
        assert!(
            row.text.contains(t::REVIEW_REASON_OTHER),
            "text: {}",
            row.text
        );
    }

    /// Both reasons on one person read as one row listing both — not two rows,
    /// and not a silently-dropped second reason.
    #[test]
    fn several_reasons_on_one_person_stay_on_one_row() {
        let review = MemberReview {
            person: person(4),
            reasons: vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("second".into()),
            ],
        };
        let els = member_review_elements(&[review], None);
        assert_eq!(
            els.iter().filter(|e| e.id == "member-review-row").count(),
            1
        );
        let row = els.iter().find(|e| e.id == "member-review-row").unwrap();
        assert!(
            row.text.contains(t::REVIEW_REASON_COMPROMISE),
            "{}",
            row.text
        );
        assert!(row.text.contains(t::REVIEW_REASON_OTHER), "{}", row.text);
    }
}
