//! The Settings → Mail → Spam sub-page (`docs/goal/behavior/mail-spam.md`
//! § Reset / § Training-sample retention → § Undo; `tests/e2e-unified/ui.yaml`
//! `mail-spam` page + its `mail-spam-training-history-list` and
//! `report-share-published-list` components).
//!
//! Where a person manages **their own** per-account spam classifier: reset the
//! per-user Bayesian model, opt in/out of the deployment-baseline contribution,
//! opt in/out of distributed k-anonymized report sharing, and review + undo
//! individual training events.
//!
//! **Two independent surfaces on one page, and that is the ratified shape.**
//! The classifier half is the shared `fauna_client_mail_settings::
//! MailSpamMachine` (`mail-spam.md` § Implementation status item 5: every app is
//! "a dumb renderer of the shared `MailSpamMachine` … no per-app logic"). The
//! report-sharing half is deliberately **not** on that machine — per
//! `report-sharing.md` § Implementation status Slice 4 it is driven directly
//! over `fauna_client_moderation::ModerationClient::{report_share_set,
//! report_share_status}`, because it is "a bool + read-only list, not a state
//! machine". linux (`apps/fauna-linux/src/settings/mail_spam.rs`) makes the same
//! split; tui is the seventh and last app to lift the page (priority #1), and
//! the **third direct-Rust consumer** after linux — no FFI hop, no shared-Rust
//! work owed.
//!
//! **The badges go through the shared formatters, never a local map.**
//! `training_label_badge` / `training_source_badge` are the single source of
//! truth for the `TrainingLabel`/`TrainingSource`→i18n-key maps
//! (`mail-spam.md:380`) — the four apps that each hard-coded them were lifted
//! onto these precisely so a seventh would not re-hand-roll them.
//!
//! **Both lists are FLAT-indexed, and the shared action is why.**
//! `tests/e2e-unified/actions/mail_spam.py` reads every row leaf with a plain
//! `get_text(id, index)` / `click(id, index)` — there is no `scope=` path
//! anywhere in it. So the row leaves paint flat, the `mail-aliases` shape, and
//! **not** the `.within("<container>", i)` shape muted-words and task-delegation
//! use. Declaring containment here would be the mirror image of the
//! `restore-history-item` bug: it would put the container first in every leaf's
//! path and leave every read resolving to nothing while the page painted
//! perfectly. Read the shared action before choosing the row shape — neither
//! flat nor scoped is the default.
//!
//! **The share-reports toggle is non-optimistic.** Its `state` attr — the one
//! thing `is_share_reports_on()` reads — is set only from a nest-confirmed
//! `report_share.status` reply, never from the local click. That is what makes
//! the e2e's `toggle → poll until on` a genuine round-trip proof rather than an
//! echo of the gesture (linux's own reasoning, `render_report_share`).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    MailSpamMachine, MailSpamSnapshot, SpamTrainingView, training_label_badge,
    training_source_badge,
};
use fauna_i18n::strings::mail_spam as t;
use fauna_protocol::moderation::ReportShareEntry;

use super::{Action, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The Spam sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailState`/`MailAliasesState` shape — construction is sync and cheap (the
/// RPC is `hydrate()`), and holding it as an `Arc` is what lets an `Op` own only
/// `Arc`s and cross a `tokio::spawn`.
#[derive(Default)]
pub(crate) struct MailSpamState {
    /// The shared classifier machine. `None` pre-auth, or when the identity
    /// secret would not decode — the page then paints its static controls and
    /// its nav edge produces no `Op`, the Mail-page degradation.
    pub(super) machine: Option<Arc<MailSpamMachine>>,
    /// The last rendered classifier snapshot. `None` until the first hydrate
    /// resolves.
    pub(super) snapshot: Option<MailSpamSnapshot>,
    /// The **nest-confirmed** report-share opt-in. Never set from a click — see
    /// the module docs.
    pub(super) share_reports: bool,
    /// The ≥k aggregates this nest exports to peers, exactly as
    /// `report_share.status` returned them (byte-identical to the federation
    /// export — the transparency guarantee, `report-sharing.md` § Client wire).
    pub(super) published: Vec<ReportShareEntry>,
    /// Set once the reset button has been armed by a first click. Reset deletes
    /// the model **and all** history and is irreversible (`mail-spam.md`
    /// § Reset), so it gets the same two-click inline gate the destructive
    /// alias controls use — tui has no modal, and ui.yaml scopes no confirm id
    /// on this page.
    pub(super) reset_armed: bool,
    /// The account's own spam-folder threshold override
    /// (`mail-policy-config.md` § Tier 3), or `None` to follow the admin
    /// default. `Some(0)` is a real setting — it turns automatic Junk filing
    /// off for this account — never collapsed into "unset".
    pub(super) spam_threshold_override: Option<u32>,
    /// The threshold input's draft text (`mail-spam-threshold-override-input`).
    /// Seeded from a successful read via [`Self::apply_spam_threshold_override`],
    /// edited by keystroke, committed on Enter/click
    /// (`Element::input_commit` — `Action::MailSpamCommitThreshold`). Not part
    /// of the "every op re-reads" bucket `share`/`published` are in, so a click
    /// on Reset/Contribute/Undo cannot clobber an in-progress, uncommitted edit.
    pub(super) spam_threshold_input: String,
}

/// One awaited read of the whole page — three independent parts, because tui
/// runs one awaited op per nav edge/gesture (see `Op::HydrateMailSpam`).
///
/// Each part carries its own failure: a classifier failure rides
/// `MailSpamSnapshot.error` (the shared machine's contract), a transport failure
/// on the report-share half rides the `Err` arm, same for the threshold
/// override. All three bridge onto `error-message` in the single fold — never
/// dropped, which is the shape testing.md point 11 requires.
#[derive(Debug)]
pub(crate) struct MailSpamRead {
    /// `None` when the op did not touch the classifier (the report-share toggle
    /// and the threshold-override commit).
    pub(super) snapshot: Option<MailSpamSnapshot>,
    /// Unlike `snapshot`/`threshold_override`, refetched on every op — the
    /// module's existing (pre-dating this field) choice, not repeated below.
    pub(super) share: Result<fauna_protocol::moderation::ModerationReportShareStatusReply, String>,
    /// `None` when the op did not touch the threshold override (every op
    /// except the page hydrate and the override's own commit) — so an
    /// unrelated dispatch (Reset/Contribute/Undo) cannot clobber an
    /// in-progress, uncommitted edit in `spam_threshold_input`.
    pub(super) threshold_override: Option<Result<Option<u32>, String>>,
}

impl MailSpamState {
    /// Build the page's shared machine from the session's WS handle + identity
    /// secret. The keypair is load-bearing, not ceremony: undoing a
    /// **client-written** (sealed) training row runs the reseal loop, which
    /// needs the actor's MSEK (`rpc_glue::build_mail_spam_machine`'s own doc).
    /// A server-written (plaintext) row would still undo through the seam's
    /// server path, so a build failure degrades the page rather than breaking
    /// it — hence the `Err` arm only logs.
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        secret_hex: &str,
        mail: Arc<dyn fauna_client_config::MailStore>,
        node_url: &str,
        ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    ) -> Self {
        let machine = match crate::mail_glue::build_mail_spam_machine(
            nest, secret_hex, mail, node_url, ledger,
        ) {
            Ok(machine) => Some(Arc::new(machine)),
            Err(e) => {
                tracing::error!("[settings/mail_spam] build machine: {e}");
                None
            }
        };
        Self {
            machine,
            ..Self::default()
        }
    }

    /// Drop the page-local draft on a fresh visit — today just the armed reset,
    /// so a primed destructive confirm never survives a nav-away and fires
    /// against a later visit (`MailState::reset_form`'s reasoning). The
    /// snapshot survives; the nav-edge hydrate replaces it.
    pub(super) fn reset_form(&mut self) {
        self.reset_armed = false;
    }

    /// Fold a nest-confirmed `report_share.status` reply. The only writer of
    /// `share_reports` / `published` — see the module docs.
    pub(super) fn apply_report_share(
        &mut self,
        reply: fauna_protocol::moderation::ModerationReportShareStatusReply,
    ) {
        self.share_reports = reply.share;
        self.published = reply.published;
    }

    /// Fold a nest-confirmed `get_spam_threshold_override` value. The only
    /// writer of `spam_threshold_override` / `spam_threshold_input` — reseeds
    /// the draft from the persisted value, the `open_edit_form` shape.
    pub(super) fn apply_spam_threshold_override(&mut self, value: Option<u32>) {
        self.spam_threshold_override = value;
        self.spam_threshold_input = value.map(|v| v.to_string()).unwrap_or_default();
    }
}

/// Read `fauna.bridges.get_spam_threshold_override` — the account's own
/// per-account spam-folder threshold, or `None` when it follows the admin
/// default (`mail-policy-config.md` § Tier 3). Replay-safe pure read.
pub(super) async fn read_spam_threshold_override(
    nest: Arc<fauna_client::NestClient>,
) -> Result<Option<u32>, String> {
    fauna_client_bridges::MailAccountClient::new(nest)
        .get_spam_threshold_override()
        .await
        .map_err(|e| e.to_string())
}

/// Set the override, then re-read so the input reflects the **persisted**
/// value — never the local click/keystroke — the `set_report_share` shape.
pub(super) async fn set_spam_threshold_override(
    nest: Arc<fauna_client::NestClient>,
    value: Option<u32>,
) -> Result<Option<u32>, String> {
    fauna_client_bridges::MailAccountClient::new(nest)
        .set_spam_threshold_override_and_reload(value)
        .await
        .map_err(|e| e.to_string())
}

/// Read `fauna.moderation.report_share.status` — the caller's opt-in plus the
/// ≥k aggregates this nest exports. The transport error is stringified here so
/// the `Outcome` stays `Send` and the fold has exactly one thing to bridge.
pub(super) async fn read_report_share(
    nest: Arc<fauna_client::NestClient>,
) -> Result<fauna_protocol::moderation::ModerationReportShareStatusReply, String> {
    fauna_client_moderation::ModerationClient::new(nest)
        .report_share_status()
        .await
        .map_err(|e| e.to_string())
}

/// Set the opt-in, then re-read status so the toggle + published list reflect
/// the **persisted** value. Opting out withdraws this actor's reports, which can
/// shrink the list — so the re-read is not a nicety, it is how the page stops
/// showing aggregates that no longer exist. Owned by
/// `ModerationClient::report_share_set_and_reload`.
pub(super) async fn set_report_share(
    nest: Arc<fauna_client::NestClient>,
    share: bool,
) -> Result<fauna_protocol::moderation::ModerationReportShareStatusReply, String> {
    fauna_client_moderation::ModerationClient::new(nest)
        .report_share_set_and_reload(share)
        .await
        .map_err(|e| e.to_string())
}

/// The Spam sub-page's ordered element list.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the privacy/logs/muted-words precedent), so
/// both a classifier-snapshot error and a report-share transport error bridge
/// onto it through `App::errors` rather than being painted here.
pub(super) fn mail_spam_elements(state: &SettingsState) -> Vec<Element> {
    let s = &state.mail_spam;
    let snapshot = s.snapshot.as_ref();
    let contribute = snapshot.is_some_and(|snap| snap.contribute_baseline);

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
        Element::gesture_button(
            ids::MAIL_SPAM_RESET_MODEL_BUTTON,
            // The armed state relabels in place — the two-click confirm's only
            // visible affordance, since ui.yaml scopes no confirm id here — and
            // the relabel itself says the reset cannot be undone
            // (`mail-spam.md` § Reset step 2).
            if s.reset_armed {
                t::RESET_CONFIRM
            } else {
                t::RESET_BUTTON
            },
            true,
            Gesture::Settings(Action::MailSpamReset),
        ),
        Element::chrome(t::RESET_SUBTITLE),
        Element::checkbox_gesture(
            ids::MAIL_SPAM_CONTRIBUTE_BASELINE_TOGGLE,
            t::CONTRIBUTE_BASELINE_LABEL,
            contribute,
            Gesture::Settings(Action::MailSpamToggleContribute),
        )
        .attr("state", if contribute { "on" } else { "off" }),
        Element::chrome(t::CONTRIBUTE_BASELINE_SUBTITLE),
        Element::checkbox_gesture(
            ids::MAIL_SPAM_SHARE_REPORTS_TOGGLE,
            t::SHARE_REPORTS_LABEL,
            s.share_reports,
            Gesture::Settings(Action::MailSpamToggleShareReports),
        )
        .attr("state", if s.share_reports { "on" } else { "off" }),
        Element::chrome(t::SHARE_REPORTS_SUBTITLE),
        Element::input_commit(
            ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT,
            s.spam_threshold_input.clone(),
            Field::Settings(super::SettingsField::MailSpamThresholdOverride),
            Gesture::Settings(Action::MailSpamCommitThreshold),
        )
        .labelled(t::THRESHOLD_OVERRIDE_LABEL),
        Element::chrome(t::THRESHOLD_OVERRIDE_SUBTITLE),
    ];

    // ── "What this nest publishes" (report-sharing.md § transparency surface) ──
    // The container is a real element, not decoration: the driver counts it.
    els.push(Element::label(
        ids::REPORT_SHARE_PUBLISHED_LIST,
        t::PUBLISHED_TITLE,
    ));
    els.push(Element::chrome(t::PUBLISHED_DESCRIPTION));
    if s.published.is_empty() {
        els.push(Element::chrome(t::PUBLISHED_EMPTY));
    }
    for entry in &s.published {
        els.extend(published_row_elements(entry));
    }

    // ── Training history (mail-spam.md § Undo) ──
    els.push(Element::label(
        ids::MAIL_SPAM_TRAINING_HISTORY_LIST,
        t::HISTORY_TITLE,
    ));
    if snapshot.is_some_and(|snap| snap.events.is_empty()) {
        els.push(Element::chrome(t::EMPTY));
    }
    for view in snapshot.map(|snap| snap.events.as_slice()).unwrap_or(&[]) {
        els.extend(history_row_elements(view));
    }

    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// One `report-share-published-list-item` row. Pure transparency — no action.
/// The count is always ≥ k (= 3) by the nest gate, so "reporters" is always
/// plural.
fn published_row_elements(entry: &ReportShareEntry) -> Vec<Element> {
    vec![
        Element::label(
            ids::REPORT_SHARE_PUBLISHED_LIST_ITEM,
            format!("{} {}", entry.count, t::PUBLISHED_REPORTERS),
        ),
        Element::label(
            ids::REPORT_SHARE_PUBLISHED_LIST_ITEM_HASH,
            entry.content_hash.clone(),
        ),
        Element::label(
            ids::REPORT_SHARE_PUBLISHED_LIST_ITEM_FACTOR,
            entry.factor.clone(),
        ),
        Element::label(
            ids::REPORT_SHARE_PUBLISHED_LIST_ITEM_COUNT,
            entry.count.to_string(),
        ),
    ]
}

/// One `mail-spam-training-history-list-item` row from a [`SpamTrainingView`].
///
/// The undo is a single click, not a two-click gate: it is reversible by
/// retraining, so `mail-spam.md` § Undo asks for no confirmation. It carries the
/// row's `history_id_hex`, never an index — a list that re-orders under a fresh
/// snapshot can then never undo the wrong event.
fn history_row_elements(view: &SpamTrainingView) -> Vec<Element> {
    let label_text = crate::wizard::localized(&training_label_badge(view.label));
    let source_text = crate::wizard::localized(&training_source_badge(view.source));
    vec![
        Element::label(
            ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM,
            view.message.clone(),
        ),
        Element::label(
            ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_MESSAGE,
            view.message.clone(),
        ),
        Element::label(ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_LABEL, label_text),
        Element::label(
            ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_SOURCE,
            source_text,
        ),
        Element::label(
            ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_CREATED_AT,
            // `created_at_ms` is milliseconds; the shared tui formatter takes
            // microseconds and renders the same relative/absolute buckets every
            // other timestamp on this client uses.
            crate::format::format_epoch_us(view.created_at_ms.saturating_mul(1_000)),
        ),
        Element::gesture_button(
            ids::MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_UNDO_BUTTON,
            t::UNDO,
            true,
            Gesture::Settings(Action::MailSpamUndo(view.history_id_hex.clone())),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_client_mail_settings::{SpamStatus, TrainingLabel, TrainingSource};

    fn a_view(message: &str, label: TrainingLabel, source: TrainingSource) -> SpamTrainingView {
        SpamTrainingView {
            history_id_hex: "ab".repeat(16),
            message: message.to_string(),
            label,
            source,
            created_at_ms: 1_760_000_000_000,
            model_delta_applied: Vec::new(),
            sealed_subject: Vec::new(),
            mailbox: "INBOX".to_string(),
        }
    }

    fn state_with(
        events: Vec<SpamTrainingView>,
        published: Vec<ReportShareEntry>,
    ) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::MailSpam,
            ..Default::default()
        };
        state.mail_spam.snapshot = Some(MailSpamSnapshot {
            events,
            contribute_baseline: false,
            status: SpamStatus::Idle,
            error: None,
        });
        state.mail_spam.published = published;
        state
    }

    fn an_entry(hash: &str, count: u32) -> ReportShareEntry {
        ReportShareEntry {
            content_hash: hash.to_string(),
            factor: "report:spam".to_string(),
            count,
            extra: Default::default(),
        }
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    fn text_of(els: &[Element], id: &str, index: usize) -> String {
        els.iter()
            .filter(|e| e.id == id)
            .nth(index)
            .map(|e| e.text.clone())
            .unwrap_or_default()
    }

    /// Every static ui.yaml id for the `mail-spam` page paints — including both
    /// list containers, which the driver counts rather than reads.
    #[test]
    fn the_page_paints_every_static_ui_yaml_id() {
        let els = mail_spam_elements(&state_with(Vec::new(), Vec::new()));
        let ids = ids(&els);
        for id in [
            "page-heading",
            "mail-spam-reset-model-button",
            "mail-spam-contribute-baseline-toggle",
            "mail-spam-share-reports-toggle",
            "mail-spam-threshold-override-input",
            "report-share-published-list",
            "mail-spam-training-history-list",
            "settings-nav-back",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
    }

    /// One row per event and per published aggregate, with every leaf present —
    /// and the leaves paint **flat**, which is what makes the shared action's
    /// plain `get_text(id, index)` resolve (see the module docs).
    #[test]
    fn rows_paint_flat_with_every_leaf() {
        let els = mail_spam_elements(&state_with(
            vec![
                a_view(
                    "Cheap pills now · INBOX",
                    TrainingLabel::Spam,
                    TrainingSource::ImapJunkFlag,
                ),
                a_view(
                    "Lunch tomorrow? · Junk",
                    TrainingLabel::Ham,
                    TrainingSource::ImapJunkMove,
                ),
            ],
            vec![an_entry("aa", 3), an_entry("bb", 5)],
        ));
        for id in [
            "mail-spam-training-history-list-item",
            "mail-spam-training-history-list-item-message",
            "mail-spam-training-history-list-item-label",
            "mail-spam-training-history-list-item-source",
            "mail-spam-training-history-list-item-created-at",
            "mail-spam-training-history-list-item-undo-button",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                2,
                "one {id} per training event"
            );
        }
        for id in [
            "report-share-published-list-item",
            "report-share-published-list-item-hash",
            "report-share-published-list-item-factor",
            "report-share-published-list-item-count",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                2,
                "one {id} per published aggregate"
            );
        }
        assert!(
            els.iter().all(|e| e.path.is_empty()),
            "every leaf on this page paints flat — a container path would break \
             the shared action's plain index reads"
        );
    }

    /// The rendered row order is the snapshot's order, so the index the shared
    /// action reads a message at is the index its undo button sits at.
    #[test]
    fn row_order_follows_the_snapshot_so_index_reads_line_up() {
        let els = mail_spam_elements(&state_with(
            vec![
                a_view("first", TrainingLabel::Spam, TrainingSource::ImapJunkFlag),
                a_view("second", TrainingLabel::Ham, TrainingSource::ImapJunkMove),
            ],
            vec![an_entry("aa", 3), an_entry("bb", 5)],
        ));
        assert_eq!(
            text_of(&els, "mail-spam-training-history-list-item-message", 0),
            "first"
        );
        assert_eq!(
            text_of(&els, "mail-spam-training-history-list-item-message", 1),
            "second"
        );
        assert_eq!(
            text_of(&els, "report-share-published-list-item-hash", 0),
            "aa"
        );
        assert_eq!(
            text_of(&els, "report-share-published-list-item-count", 1),
            "5"
        );
        assert_eq!(
            text_of(&els, "report-share-published-list-item-factor", 0),
            "report:spam"
        );
    }

    /// The badges render through the SHARED formatters, so a seventh app cannot
    /// drift from the six before it (`mail-spam.md:380`).
    #[test]
    fn badges_render_through_the_shared_formatters() {
        let els = mail_spam_elements(&state_with(
            vec![a_view(
                "m",
                TrainingLabel::Spam,
                TrainingSource::ImapJunkFlag,
            )],
            Vec::new(),
        ));
        assert_eq!(
            text_of(&els, "mail-spam-training-history-list-item-label", 0),
            crate::wizard::localized(&training_label_badge(TrainingLabel::Spam)),
        );
        assert_eq!(
            text_of(&els, "mail-spam-training-history-list-item-source", 0),
            crate::wizard::localized(&training_source_badge(TrainingSource::ImapJunkFlag)),
        );
    }

    /// The reset button relabels once armed — the two-click gate's only visible
    /// affordance, and the reason a single stray click cannot delete a model.
    #[test]
    fn the_reset_button_relabels_when_armed() {
        let mut state = state_with(Vec::new(), Vec::new());
        let unarmed = text_of(
            &mail_spam_elements(&state),
            "mail-spam-reset-model-button",
            0,
        );
        state.mail_spam.reset_armed = true;
        let armed = text_of(
            &mail_spam_elements(&state),
            "mail-spam-reset-model-button",
            0,
        );
        assert_ne!(unarmed, armed, "the armed reset must be visibly distinct");
        assert_eq!(armed, t::RESET_CONFIRM);
    }

    /// Both toggles carry the `state` attr the shared action reads
    /// (`get_attr(id, "state")`), and the report-share one reflects only what a
    /// nest reply set — never a local click.
    #[test]
    fn both_toggles_carry_a_state_attr_and_share_reports_is_nest_confirmed() {
        let mut state = state_with(Vec::new(), Vec::new());
        let attr = |state: &SettingsState, id: &str| {
            mail_spam_elements(state)
                .into_iter()
                .find(|e| e.id == id)
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.clone())
                })
        };
        assert_eq!(
            attr(&state, "mail-spam-share-reports-toggle").as_deref(),
            Some("off")
        );
        assert_eq!(
            attr(&state, "mail-spam-contribute-baseline-toggle").as_deref(),
            Some("off")
        );

        state.mail_spam.apply_report_share(
            fauna_protocol::moderation::ModerationReportShareStatusReply {
                share: true,
                published: vec![an_entry("aa", 3)],
                extra: Default::default(),
            },
        );
        assert_eq!(
            attr(&state, "mail-spam-share-reports-toggle").as_deref(),
            Some("on")
        );
        assert_eq!(state.mail_spam.published.len(), 1);

        // The contribute toggle reads the SNAPSHOT, not a local flag — so it too
        // can only be flipped by a nest round-trip.
        state
            .mail_spam
            .snapshot
            .as_mut()
            .unwrap()
            .contribute_baseline = true;
        assert_eq!(
            attr(&state, "mail-spam-contribute-baseline-toggle").as_deref(),
            Some("on")
        );
    }

    /// A fresh visit disarms a primed reset, so an armed confirm never survives
    /// a nav-away to fire against a later visit.
    #[test]
    fn a_fresh_visit_disarms_the_reset() {
        let mut s = MailSpamState {
            reset_armed: true,
            ..MailSpamState::default()
        };
        s.reset_form();
        assert!(!s.reset_armed);
    }

    /// Pre-hydrate the page still paints its controls and both containers, and
    /// shows no empty-state copy for a list it has not read yet — "no rows yet"
    /// and "no rows" are different claims.
    #[test]
    fn pre_hydrate_the_page_paints_controls_without_claiming_an_empty_history() {
        let state = SettingsState {
            sub: SubPage::MailSpam,
            ..Default::default()
        };
        let els = mail_spam_elements(&state);
        let ids = ids(&els);
        assert!(ids.contains(&"mail-spam-training-history-list".to_string()));
        assert!(ids.contains(&"mail-spam-reset-model-button".to_string()));
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "mail-spam-training-history-list-item")
                .count(),
            0
        );
        assert!(
            !els.iter().any(|e| e.text == t::EMPTY),
            "an unread history must not claim to be empty"
        );
    }

    /// `apply_spam_threshold_override` is the only writer of both the
    /// persisted value and its draft — a nest-confirmed `Some(0)` seeds "0",
    /// never collapsed to empty (`mail-policy-config.md` § Tier 3: `Some(0)`
    /// is a real setting, distinct from `None`).
    #[test]
    fn apply_spam_threshold_override_seeds_the_draft_and_keeps_zero_distinct_from_unset() {
        let mut s = MailSpamState::default();
        s.apply_spam_threshold_override(Some(0));
        assert_eq!(s.spam_threshold_override, Some(0));
        assert_eq!(s.spam_threshold_input, "0");

        s.apply_spam_threshold_override(Some(9));
        assert_eq!(s.spam_threshold_input, "9");

        s.apply_spam_threshold_override(None);
        assert_eq!(s.spam_threshold_override, None);
        assert_eq!(s.spam_threshold_input, "");
    }

    /// The threshold input paints the current draft and commits via the
    /// generic `Element::input_commit` role — Enter/click fires
    /// `Action::MailSpamCommitThreshold` directly; this page has no separate
    /// save button for it (`Element::input_commit`'s own doc: "use when the
    /// value has no other commit affordance on its row").
    #[test]
    fn the_threshold_input_renders_the_draft_and_commits_via_input_commit() {
        let mut state = state_with(Vec::new(), Vec::new());
        state.mail_spam.spam_threshold_input = "7".to_string();
        let els = mail_spam_elements(&state);
        let input = els
            .iter()
            .find(|e| e.id == "mail-spam-threshold-override-input")
            .expect("threshold input paints");
        assert_eq!(input.text, "7");
        match input.gesture() {
            Some(Gesture::Settings(Action::MailSpamCommitThreshold)) => {}
            other => panic!("unexpected gesture: {other:?}"),
        }
    }
}
