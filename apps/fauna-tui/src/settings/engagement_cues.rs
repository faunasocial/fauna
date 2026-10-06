//! The Personalization home's **engagement-cue** facet — the Layer-A erase
//! affordance and the Layer-B opt-in toggle + transparency pane
//! (`docs/goal/behavior/engagement-cues.md` §§ Layer A / Layer B / At rest).
//!
//! **Scope: authoring controls only — tui builds no capture shell.** The
//! capture-shell boundary revision (ratified 2026-07-29) is a different track,
//! and `engagement-cues.md` § Implementation status records a tui capture shell
//! as unbuilt and unowned. Nothing here derives, buckets or reports a cue; this
//! facet only lets the user *erase* what was captured and *choose* whether
//! coarse verdicts join the k-anonymized aggregate plane.
//!
//! **Shape: a bool plus a read-only list, not a state machine** — the same
//! reasoning (and very nearly the same code) as the report-sharing pane in
//! [`super::mail_spam`], which this mirrors deliberately: `signal_share.status`
//! returns the identical `ReportShareEntry` rows, and the shared
//! `actions/personalization.py` readers mirror mail_spam's too.
//!
//! **Everything routes through the LIVE [`crate::feed::CliFeedManager`], never a
//! fresh `ModerationClient`.** That is not incidental plumbing — it is the
//! difference between correct and quietly wrong:
//!
//! * [`FeedManager::set_signal_sharing`] and `signal_share_status` **cache the
//!   nest-confirmed `share`** for the Layer-B producer (`manager.rs`,
//!   `share_signals.store(...)`). A raw `ModerationClient::signal_share_set`
//!   would flip the persisted row while leaving the live manager's cache stale —
//!   invisible today (tui has no producer) and a silent privacy bug the moment a
//!   tui capture shell lands.
//! * [`FeedManager::delete_cue_rollup`] deletes the sealed `cues:v1` row **and**
//!   resets the in-memory `CueEngine`. A second manager would delete the row and
//!   leave the live engine populated, so the next put would resurrect it.
//!
//! This also keeps tui uniform with linux/windows/apple, all of which drive
//! these three affordances through the shared `FeedManager` faces (priority #3).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_i18n::strings::personalization as p;
use fauna_protocol::moderation::{ModerationSignalShareStatusReply, ReportShareEntry};

use crate::element::{Element, Gesture};
use crate::feed::CliFeedManager;

/// The facet's whole render state: the nest-confirmed opt-in and the ≥k
/// aggregates this nest exports.
///
/// Both are **nest-confirmed, never optimistic** — the toggle renders what the
/// reply said, so a refused or lost set can never leave the UI claiming an
/// opt-in the nest does not hold. (`engagement-cues.md` § Layer B: "Caches the
/// nest-confirmed `share` for the producer — never optimistically.")
#[derive(Debug, Default, Clone)]
pub(crate) struct EngagementCuesState {
    /// `spam_preferences.share_signals`, as last confirmed by the nest.
    pub(crate) share_signals: bool,
    /// The ≥k published aggregates (`signal:*` **and** `report:*` — the export
    /// view is one list; `signal_published_description` says so to the user).
    pub(crate) published: Vec<ReportShareEntry>,
}

impl EngagementCuesState {
    /// Fold a nest-confirmed `fauna.moderation.signal_share.status` reply.
    pub(super) fn apply(&mut self, reply: ModerationSignalShareStatusReply) {
        self.share_signals = reply.share;
        self.published = reply.published;
    }
}

/// Read `fauna.moderation.signal_share.status` over the **live** manager — the
/// caller's opt-in plus the ≥k aggregates this nest exports. Stringified here so
/// the `Outcome` stays `Send` and the fold has exactly one thing to bridge.
pub(super) async fn read_signal_share(
    manager: Arc<CliFeedManager>,
) -> Result<ModerationSignalShareStatusReply, String> {
    manager.signal_share_status().await
}

/// Set the opt-in over the live manager, which itself re-reads status so the
/// toggle + published list reflect the **persisted** value. Opting out withdraws
/// this actor's `signal:*` rows, which can shrink the list — so the re-read is
/// not a nicety, it is how the page stops showing aggregates that no longer
/// exist (`engagement-cues.md` § Layer B, withdrawal semantics).
pub(super) async fn set_signal_share(
    manager: Arc<CliFeedManager>,
    share: bool,
) -> Result<ModerationSignalShareStatusReply, String> {
    manager.set_signal_sharing(share).await
}

/// Delete the sealed `cues:v1` rollup and reset the live engine
/// (`engagement-cues.md` § At rest — "Deleting the row is the user's own delete
/// affordance (Personalization home), user-revocable by construction").
pub(super) async fn clear_engagement_data(manager: Arc<CliFeedManager>) -> Result<(), String> {
    manager.delete_cue_rollup().await
}

/// The facet's elements, appended to the Personalization home.
///
/// Row shape is **FLAT-indexed**, matching `actions/personalization.py`'s
/// `signal_published_*` readers, which use plain `get_text(id, index=i)` — the
/// same shape `mail_spam.rs`'s report-share pane paints for the same reason.
/// (There is no house default here: this page's trained-factor engagement
/// toggle is read with `scope="…-item[i]"`. Read the shared action every time.)
pub(super) fn engagement_cue_elements(state: &EngagementCuesState) -> Vec<Element> {
    let mut els = vec![
        // ── Layer A: the user-revocable erase (engagement-cues.md § At rest) ──
        // A single click, no confirm gate: ui.yaml scopes no confirm id here,
        // and the data is re-derivable by simply using the app again — unlike
        // the account-delete gate, this destroys no irrecoverable user content.
        Element::gesture_button(
            ids::PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON,
            p::CLEAR_ENGAGEMENT_DATA,
            true,
            Gesture::Settings(super::Action::ClearEngagementData),
        ),
        // ── Layer B: the opt-in (default off — the frame's D6 rule) ──
        Element::chrome(p::SHARE_SIGNALS_TITLE),
        Element::checkbox_gesture(
            ids::PERSONALIZATION_SHARE_SIGNALS_TOGGLE,
            p::SHARE_SIGNALS_LABEL,
            state.share_signals,
            Gesture::Settings(super::Action::ToggleShareSignals),
        )
        // The shared action reads `state`, not the checkbox glyph — the
        // "on"/"off" wire contract apple/windows already answer.
        .attr("state", if state.share_signals { "on" } else { "off" }),
        Element::chrome(p::SHARE_SIGNALS_SUBTITLE),
    ];

    // ── The transparency pane (report-sharing.md § transparency surface) ──
    // The container is a real element, not decoration: the driver counts it.
    els.push(Element::label(
        ids::SIGNAL_SHARE_PUBLISHED_LIST,
        p::SIGNAL_PUBLISHED_TITLE,
    ));
    els.push(Element::chrome(p::SIGNAL_PUBLISHED_DESCRIPTION));
    if state.published.is_empty() {
        els.push(Element::chrome(p::SIGNAL_PUBLISHED_EMPTY));
    }
    for entry in &state.published {
        els.extend(published_row_elements(entry));
    }
    els
}

/// One `signal-share-published-list-item` row. Pure transparency — no action.
/// The count is always ≥ k (= 3) by the nest gate, so "contributors" is always
/// plural.
fn published_row_elements(entry: &ReportShareEntry) -> Vec<Element> {
    vec![
        Element::label(
            ids::SIGNAL_SHARE_PUBLISHED_LIST_ITEM,
            format!("{} {}", entry.count, p::SIGNAL_PUBLISHED_CONTRIBUTORS),
        ),
        Element::label(
            ids::SIGNAL_SHARE_PUBLISHED_LIST_ITEM_HASH,
            entry.content_hash.clone(),
        ),
        Element::label(
            ids::SIGNAL_SHARE_PUBLISHED_LIST_ITEM_FACTOR,
            entry.factor.clone(),
        ),
        Element::label(
            ids::SIGNAL_SHARE_PUBLISHED_LIST_ITEM_COUNT,
            entry.count.to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, factor: &str, count: u32) -> ReportShareEntry {
        ReportShareEntry {
            content_hash: hash.to_string(),
            factor: factor.to_string(),
            count,
            extra: Default::default(),
        }
    }

    fn ids(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str, index: usize) -> &'a str {
        els.iter()
            .filter(|e| e.id == id)
            .nth(index)
            .map(|e| e.text.as_str())
            .unwrap_or_else(|| panic!("no {id} at index {index}"))
    }

    #[test]
    fn the_three_ids_render_with_an_empty_pane() {
        let els = engagement_cue_elements(&EngagementCuesState::default());
        let ids = ids(&els);
        assert!(ids.contains(&"personalization-clear-engagement-data-button"));
        assert!(ids.contains(&"personalization-share-signals-toggle"));
        assert!(ids.contains(&"signal-share-published-list"));
        // Empty is a real state, not an absence: the pane must render (and say
        // so) before any opt-in, which is exactly what the windows/apple e2e
        // asserts "renders without error even before any opt-in".
        assert!(!ids.contains(&"signal-share-published-list-item"));
    }

    #[test]
    fn the_toggle_defaults_off_and_carries_the_state_attr() {
        // Default-off is the frame's D6 privacy rule, not a UI preference —
        // worth pinning so a refactor cannot flip it silently.
        let attr = |state: &EngagementCuesState| {
            engagement_cue_elements(state)
                .into_iter()
                .find(|e| e.id == "personalization-share-signals-toggle")
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.clone())
                })
        };
        assert_eq!(
            attr(&EngagementCuesState::default()).as_deref(),
            Some("off")
        );
        assert_eq!(
            attr(&EngagementCuesState {
                share_signals: true,
                published: vec![],
            })
            .as_deref(),
            Some("on")
        );
    }

    #[test]
    fn published_rows_paint_flat_and_in_order() {
        // FLAT-indexed to match `actions/personalization.py`'s plain
        // `get_text(id, index=i)` readers — the mail_spam precedent. If this
        // ever became `.within(...)`, every scoped read would resolve to
        // nothing while the pane painted perfectly.
        let state = EngagementCuesState {
            share_signals: true,
            published: vec![
                entry("aa11", "signal:watch-complete", 4),
                entry("bb22", "signal:skip", 7),
            ],
        };
        let els = engagement_cue_elements(&state);
        assert_eq!(
            text_of(&els, "signal-share-published-list-item-hash", 0),
            "aa11"
        );
        assert_eq!(
            text_of(&els, "signal-share-published-list-item-hash", 1),
            "bb22"
        );
        assert_eq!(
            text_of(&els, "signal-share-published-list-item-factor", 0),
            "signal:watch-complete"
        );
        assert_eq!(
            text_of(&els, "signal-share-published-list-item-count", 1),
            "7"
        );
    }

    #[test]
    fn apply_takes_the_nest_confirmed_reply_verbatim() {
        // The fold is the ONLY writer of `share_signals` — there is no
        // optimistic set anywhere, which is what makes the rendered toggle
        // evidence of the persisted row rather than an echo of the click.
        let mut state = EngagementCuesState::default();
        state.apply(ModerationSignalShareStatusReply {
            share: true,
            published: vec![entry("cc33", "report:spam", 3)],
            extra: Default::default(),
        });
        assert!(state.share_signals);
        assert_eq!(state.published.len(), 1);

        // Opting out withdraws this actor's rows, so the list can shrink — the
        // fold must replace, never merge.
        state.apply(ModerationSignalShareStatusReply {
            share: false,
            published: vec![],
            extra: Default::default(),
        });
        assert!(!state.share_signals);
        assert!(state.published.is_empty());
    }
}
