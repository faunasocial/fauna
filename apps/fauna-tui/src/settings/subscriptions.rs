//! The consumer-side **`subscription-settings`** page (subscriptions Slice B) —
//! `docs/goal/behavior/monetization.md` § Pillar 1 UX surface 3. Reached from
//! the Settings rail as "Subscriptions" (sibling of Web / Mail & Calendar);
//! shows **this user's own subscriptions across every creator** with per-row
//! unsubscribe, plus the Pillar-3 claim-code redemption input.
//!
//! Distinct from the profile Tiers-tab SELF author management
//! (`crate::profile::tiers`): that is what the user *offers*, this is what the
//! user *consumes*. The reference implementation is
//! `apps/fauna-linux/src/settings/subscriptions.rs` — same ui.yaml ids, same
//! shared-crate calls (priority #1).
//!
//! **All data comes from shared Rust** (priority #2): the list is one
//! `SubscriptionsClient::mine_list` read (`fauna.subscriptions.mine.list`, the
//! caller-scoped enumeration), unsubscribe is the thin
//! `SubscriptionsClient::unsubscribe`, redemption the thin
//! `PaymentsClient::claims_redeem`, and the creator column's handle-or-hex
//! choice is `fauna_core::format::author_display_label`. This module owns no
//! business rule.
//!
//! **The row does not vanish on unsubscribe, and that is the product rule, not
//! a limitation.** For a client-minted tier `unsubscribe` answers `Queued` — the
//! subscriber stays a member until the author's client commits the removal
//! rotation (`monetization.md` § Flows → Unsubscribe). An app that optimistically
//! drops the row renders a leave as done while the `KeyBlob` still covers the
//! leaver. So every mutation here re-reads `mine.list` and paints the nest's
//! answer; nothing is painted from the click.
//!
//! Observer-free like the sibling Web page: the nav edge re-reads on every visit
//! (`route_subpage`), and each mutation re-reads in the same op — which is what
//! lets the agent's awaited click return with the settled list already folded.
//!
//! Errors ride `app.errors[Page::Settings]`, the canonical tui error surface the
//! global `error-message` registration and the state protocol's `messages.error`
//! both read (`crate::ui::register_frame`) — this page paints no `error-message`
//! element of its own, the `muted-words`/`account`/`privacy` precedent.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_core::identity::ActorId;
use fauna_i18n::strings::subscriptions as s;
use fauna_protocol::subscriptions::MineSubscription;

#[cfg(feature = "payments")]
use super::SettingsField;
use super::{Action, SettingsState};
#[cfg(feature = "payments")]
use crate::element::Field;
use crate::element::{Element, Gesture};

/// The consumer page's state — the last `mine.list` read plus the one claim-code
/// buffer.
///
/// Per-actor by construction (it is *this* user's subscriptions), so
/// `SettingsState::clear_session` drops it: a stale row would name the
/// signed-out actor's creators under the next account.
#[derive(Default)]
pub(crate) struct SubscriptionsState {
    /// The caller's subscriptions across all creators — active rows and queued
    /// subscribe requests, deduped nest-side (`MineListReply`).
    pub(crate) mine: Vec<MineSubscription>,
    /// `subscription-claim-redeem-input` — a local buffer committed only on
    /// `subscription-claim-redeem-button` (the `muted-word-input` shape).
    /// Cleared only when the nest *accepts* the code, so a rejected paste is
    /// still there to correct.
    pub(crate) claim_input: String,
}

impl SubscriptionsState {
    /// Read the claim buffer (`SettingsField::ClaimRedeemInput`).
    #[cfg(feature = "payments")]
    pub(super) fn field(&self) -> String {
        self.claim_input.clone()
    }

    /// The creator this row unsubscribes from, or `None` if the list moved under
    /// the click (a re-read between paint and gesture).
    pub(super) fn author_at(&self, index: usize) -> Option<ActorId> {
        self.mine.get(index).map(|sub| sub.author_id)
    }

    /// The trimmed claim code, or `None` when the buffer holds nothing to
    /// redeem — a blank submit is a no-op, never a nest round-trip.
    #[cfg(feature = "payments")]
    pub(super) fn claim_code(&self) -> Option<String> {
        let code = self.claim_input.trim();
        (!code.is_empty()).then(|| code.to_string())
    }
}

/// What the page asks the nest for. One op family because all three end the
/// same way — a fresh `mine.list` — which is what makes the awaited click return
/// with the settled list rather than the pre-click frame.
#[derive(Debug)]
pub(crate) enum ConsumerOp {
    /// The nav-edge read (`route_subpage`).
    Hydrate,
    /// `subscription-mine-unsubscribe-button` — `fauna.subscriptions.unsubscribe`.
    Unsubscribe(ActorId),
    /// `subscription-claim-redeem-button` — `fauna.payments.claims.redeem`.
    #[cfg(feature = "payments")]
    RedeemClaim(String),
}

/// The page's whole outcome: the fresh list (or the message to surface), plus
/// whether a claim code was actually spent.
///
/// The flag rides the outcome rather than being inferred at the fold because
/// only the op knows it: clearing the input on *dispatch* would throw away a
/// code the nest went on to reject.
#[derive(Debug)]
pub(crate) struct ConsumerRead {
    pub(crate) result: Result<Vec<MineSubscription>, String>,
    pub(crate) claim_redeemed: bool,
}

/// Run one consumer op: the mutation (if any), then the re-read.
///
/// A failed mutation reports the failure and **skips** the re-read — the last
/// nest-confirmed list stays painted, so a transport blip never blanks the page
/// (the `WebView` fold's discipline).
pub(crate) async fn run(nest: Arc<NestClient>, op: ConsumerOp) -> ConsumerRead {
    let subs = SubscriptionsClient::new(Arc::clone(&nest));
    #[allow(unused_mut)]
    let mut claim_redeemed = false;
    let mutation = match op {
        ConsumerOp::Hydrate => Ok(()),
        ConsumerOp::Unsubscribe(author_id) => subs
            .unsubscribe(author_id)
            .await
            .map(|_| ())
            .map_err(|e| format!("unsubscribe: {e}")),
        #[cfg(feature = "payments")]
        ConsumerOp::RedeemClaim(code) => PaymentsClient::new(Arc::clone(&nest))
            .claims_redeem(code)
            .await
            .map(|_| claim_redeemed = true)
            .map_err(|e| format!("redeem claim: {e}")),
    };
    match mutation {
        Ok(()) => ConsumerRead {
            result: subs
                .mine_list()
                .await
                .map_err(|e| format!("load subscriptions: {e}")),
            claim_redeemed,
        },
        Err(message) => ConsumerRead {
            result: Err(message),
            claim_redeemed: false,
        },
    }
}

/// The `subscription-settings` page's ordered element list — ui.yaml's declared
/// order (`page-heading`, `subscription-mine-section` + the
/// `subscription-mine-list` rows, the claim redeem pair), with
/// `settings-nav-back` in the rail-exit slot every other sub-page puts it.
///
/// **Row shape.** Each subscription is a `subscription-mine-row` marker plus its
/// four leaves, every one `.within(ids::SUBSCRIPTION_MINE_ROW, i)` — the
/// `crate::profile::tiers` idiom for this feature's other four list components,
/// which keeps a leaf addressable both flat (`get_text(id, index=i)`, what
/// `actions/subscriptions.py` uses) and scoped (`scope="subscription-mine-row[0]"`).
/// The leaves are **not** `inline`: one element per painted line is what makes a
/// long creator hex readable in a narrow terminal, and it is the shape every
/// non-grid tui page uses (`Element::starts_row` tells the story of the one that
/// didn't).
pub(super) fn subscriptions_elements(app: &crate::app::App) -> Vec<Element> {
    let state = &app.settings;
    let page = &state.subscriptions;
    let overlay = crate::contacts::overlay_projection(app);
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, s::TITLE),
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
        Element::label(ids::SUBSCRIPTION_MINE_SECTION, s::MY_SUBSCRIPTIONS),
        Element::label(ids::SUBSCRIPTION_MINE_LIST, " "),
    ];
    // Untagged chrome (ui.yaml scopes no empty-state id here), so the human is
    // never shown a blank where a list belongs and no id assertion can see it.
    if page.mine.is_empty() {
        els.push(Element::chrome(s::NO_SUBSCRIPTIONS));
    }
    for (i, sub) in page.mine.iter().enumerate() {
        els.push(
            Element::label(ids::SUBSCRIPTION_MINE_ROW, " ").within(ids::SUBSCRIPTION_MINE_ROW, i),
        );
        els.push(
            // The viewer's own nickname for the creator, else their handle when
            // the nest resolved one, else the hex actor id — the one resolver
            // over the shared chooser, so all 7 apps make the same call
            // (`value-formatting.md` § Subscription author label, § Peer
            // display label).
            Element::label(
                ids::SUBSCRIPTION_MINE_AUTHOR,
                match overlay.as_ref() {
                    Some(p) => p.subscription_author_label(sub.handle.as_deref(), &sub.author_id.0),
                    None => fauna_core::format::author_display_label(
                        sub.handle.as_deref(),
                        &sub.author_id.0,
                    ),
                },
            )
            .within(ids::SUBSCRIPTION_MINE_ROW, i),
        );
        els.push(
            Element::label(ids::SUBSCRIPTION_MINE_TIER, sub.tier.clone())
                .within(ids::SUBSCRIPTION_MINE_ROW, i),
        );
        els.push(
            // The raw wire status ("active" | "pending"), rendered verbatim —
            // uniform with §2's `subscription-request-kind` and with linux.
            Element::label(ids::SUBSCRIPTION_MINE_STATUS, sub.status.clone())
                .within(ids::SUBSCRIPTION_MINE_ROW, i),
        );
        els.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_MINE_UNSUBSCRIBE_BUTTON,
                s::UNSUBSCRIBE,
                true,
                Gesture::Settings(Action::UnsubscribeMine(i)),
            )
            .within(ids::SUBSCRIPTION_MINE_ROW, i),
        );
    }
    // Claim redemption (`monetization.md` § Pillar 3 Q4's universal fallback
    // binding): the buyer pastes a post-payment code, the entitlement binds to
    // this actor and lands through Pillar 1's grant queue — so a success renders
    // above exactly like a queued subscribe (a "pending" row).
    //
    // `payments`-gated with the plane it spends into (`dynamic-features.md`
    // § Platform-family surface excision). Note the two layers do NOT collapse
    // into one: `redeem_gate` below is the DYNAMIC courtesy layer (a nest whose
    // policy says no), while this `#[cfg]` is COMPILE-TIME excision (an app
    // artifact that carries no money plane at all). A disabled button is still
    // a painted id, and criterion 1 is a `strings`-grep for ids.
    #[cfg(feature = "payments")]
    {
        els.push(Element::chrome(s::REDEEM_CLAIM_TITLE));
        els.push(
            Element::input(
                ids::SUBSCRIPTION_CLAIM_REDEEM_INPUT,
                page.claim_input.clone(),
                Field::Settings(SettingsField::ClaimRedeemInput),
            )
            .labelled(s::CLAIM_CODE),
        );
        // The Dim-3 courtesy layer (`dynamic-features.md` § Evaluation points item
        // 2). `payments.claim.redeem` is a WIRED gate surface — the nest refuses
        // this call with a typed `fauna.features.denied` / `.over_quota` when the
        // plane says no — so offering a live button that is guaranteed to fail is
        // the "letting the user hit refusals" the courtesy layer exists to prevent.
        //
        // The decision is READ, never re-derived: `affordance` is the shared crate's
        // and already accounts for the subset edge (a `payments` deny reaches
        // `zaps`) and for the newness-delta rule (a spent `counterparties` cell
        // does NOT disable, because a redemption toward a creator already in the
        // records is still admitted). A page that decided this itself would get
        // both wrong in the direction that disables working buttons.
        let claim_gate = redeem_gate(state);
        els.push(Element::gesture_button(
            ids::SUBSCRIPTION_CLAIM_REDEEM_BUTTON,
            s::REDEEM,
            claim_gate.is_none(),
            Gesture::Settings(Action::RedeemClaim),
        ));
        // Rule 5 — a disabled control states its reason within eyeshot, and here the
        // reason is the honest why-line the shared crate already composed. Nothing
        // is painted while the read is un-hydrated: a settled claim with no basis is
        // worse than a blank (the un-hydrated-paint finding), and the button stays
        // live in that window because the nest, not the app, is the enforcement floor.
        if let Some(reason) = claim_gate {
            els.push(Element::chrome(reason));
        }
    }
    els
}

/// Why `subscription-claim-redeem-button` is dead, or `None` when it works.
///
/// `payments` is the member that gates claim redemption. A `hidden` affordance
/// means the nest build carries no payments plane at all — the button is
/// disabled with the same honest line rather than vanishing, because the page
/// is reachable and a button that silently disappears is the silent gate
/// boundary 4 forbids; the *excision* story is the orthogonal COMPILE-TIME
/// axis, landed 2026-08-14 as this crate's `payments` feature — that one makes
/// the whole section (this gate included) absent from the artifact, where this
/// one only greys the button in a build that HAS the plane.
#[cfg(feature = "payments")]
fn redeem_gate(state: &SettingsState) -> Option<String> {
    fauna_client_features::gate_reason(
        state.features.as_deref().unwrap_or(&[]),
        "payments",
        fauna_i18n::strings::lookup,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_core::data::Timestamp;
    use fauna_core::identity::ActorId;

    fn sub(author: [u8; 32], tier: &str, status: &str, handle: Option<&str>) -> MineSubscription {
        MineSubscription {
            author_id: ActorId(author),
            tier: tier.to_string(),
            status: status.to_string(),
            handle: handle.map(str::to_string),
            since: Timestamp(0),
            extra: Default::default(),
        }
    }

    fn app_with(mine: Vec<MineSubscription>) -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        app.settings.sub = SubPage::Subscriptions;
        app.settings.subscriptions.mine = mine;
        app
    }

    fn ids(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    // ── The Dim-3 gate on claim redemption ──────────────────────────────

    fn payments_row(
        authored: &[(
            fauna_client_features::RuleTier,
            fauna_core::feature_gate::FeaturePolicy,
        )],
        capabilities: &[String],
    ) -> fauna_client_features::FeatureRow {
        let feature = fauna_client_features::GatedFeature::Payments;
        fauna_client_features::feature_row(
            &fauna_protocol::features::FeatureStatusItem {
                feature,
                policy: fauna_core::feature_gate::effective_policy(feature, authored, &[]),
                usage: fauna_core::feature_gate::UsageCounters::default(),
                extra: Default::default(),
            },
            capabilities,
        )
    }

    fn subscriptions_with(rows: Option<Vec<fauna_client_features::FeatureRow>>) -> crate::app::App {
        let mut app = app_with(Vec::new());
        app.settings.features = rows;
        app
    }

    fn redeem_button(els: &[Element]) -> &Element {
        els.iter()
            .find(|e| e.id == "subscription-claim-redeem-button")
            .expect("the redeem button always registers")
    }

    /// The nest refuses `fauna.payments.claims.redeem` under a payments deny
    /// (it is a wired gate surface), so a live button is a guaranteed refusal —
    /// exactly what the courtesy layer exists to prevent. And it must say WHY:
    /// a control that is merely dead is the silent gate boundary 4 forbids.
    #[test]
    fn a_denied_payments_plane_disables_redeem_and_states_the_reason() {
        let denied = fauna_core::feature_gate::FeaturePolicy {
            availability: fauna_core::feature_gate::Availability::Deny,
            ..Default::default()
        };
        let app = subscriptions_with(Some(vec![payments_row(
            &[(fauna_client_features::RuleTier::Admin, denied)],
            &[fauna_protocol::discovery::capability::SUBSCRIPTIONS.to_string()],
        )]));
        let els = subscriptions_elements(&app);

        let button = redeem_button(&els);
        assert!(!button.enabled, "a guaranteed refusal must not look live");
        assert!(
            els.iter()
                .any(|e| e.text == "Turned off by your nest admin."),
            "rule 5: the disabled control states its reason within eyeshot"
        );
    }

    /// The other direction, and the one a re-derivation gets wrong: a member
    /// that is *bounded but unspent* is fully usable. A page that disabled on
    /// `availability == "limit"` would kill a working button for every user on
    /// a nest with any policy at all.
    #[test]
    fn a_bounded_but_unspent_plane_leaves_redeem_live() {
        let app = subscriptions_with(Some(vec![payments_row(
            &[],
            &[fauna_protocol::discovery::capability::SUBSCRIPTIONS.to_string()],
        )]));
        let els = subscriptions_elements(&app);

        let row = &app.settings.features.as_ref().unwrap()[0];
        assert_eq!(row.availability, "limit", "tier 1 bounds it");
        assert!(
            redeem_button(&els).enabled,
            "bounded is not blocked — the button works"
        );
    }

    /// Before the read lands there is no basis for a verdict, and the nest is
    /// the enforcement floor regardless — so the button stays live rather than
    /// being disabled on a guess.
    #[test]
    fn an_unhydrated_read_leaves_redeem_live_and_says_nothing() {
        let els = subscriptions_elements(&subscriptions_with(None));
        assert!(redeem_button(&els).enabled);
        assert!(
            !els.iter().any(|e| e.text.contains("Turned off")),
            "no verdict may be painted before its data lands"
        );
    }

    /// The empty page still registers every ui.yaml id the driver waits on —
    /// `navigate_settings` blocks on `subscription-mine-section`, so a page that
    /// only grew its landmark once rows arrived would hang every consumer test.
    #[test]
    fn empty_page_paints_the_section_and_the_claim_pair() {
        let app = app_with(Vec::new());
        let els = subscriptions_elements(&app);
        assert_eq!(
            ids(&els),
            vec![
                "page-heading",
                "settings-nav-back",
                "subscription-mine-section",
                "subscription-mine-list",
                "subscription-claim-redeem-input",
                "subscription-claim-redeem-button",
            ]
        );
        // The empty state is real UI, but untagged — so it can never be
        // mistaken for a row.
        assert!(
            els.iter()
                .any(|e| e.id.is_empty() && e.text == s::NO_SUBSCRIPTIONS)
        );
    }

    /// One row per subscription, four leaves each, all scoped to their row.
    #[test]
    fn a_row_paints_its_four_leaves_scoped_to_the_row() {
        let app = app_with(vec![sub([0xab; 32], "gold", "active", Some("alice"))]);
        let els = subscriptions_elements(&app);
        assert_eq!(
            ids(&els),
            vec![
                "page-heading",
                "settings-nav-back",
                "subscription-mine-section",
                "subscription-mine-list",
                "subscription-mine-row",
                "subscription-mine-author",
                "subscription-mine-tier",
                "subscription-mine-status",
                "subscription-mine-unsubscribe-button",
                "subscription-claim-redeem-input",
                "subscription-claim-redeem-button",
            ]
        );
        for id in [
            "subscription-mine-row",
            "subscription-mine-author",
            "subscription-mine-tier",
            "subscription-mine-status",
            "subscription-mine-unsubscribe-button",
        ] {
            let el = els.iter().find(|e| e.id == id).expect("leaf painted");
            assert_eq!(
                el.path
                    .first()
                    .map(|(container, index)| (container.as_str(), *index)),
                Some(("subscription-mine-row", 0)),
                "{id} must be scoped to its row so scope=\"subscription-mine-row[0]\" resolves"
            );
        }
    }

    /// The creator column is the shared handle-or-hex chooser: a resolved handle
    /// renders verbatim, an absent one falls back to the full hex actor id. The
    /// consumer e2e asserts the hex arm specifically (its author is admitted
    /// handle-less), so both arms are pinned here.
    #[test]
    fn creator_column_prefers_the_handle_and_falls_back_to_hex() {
        let app = app_with(vec![
            sub([0x01; 32], "gold", "active", Some("alice")),
            sub([0x02; 32], "silver", "pending", None),
        ]);
        let els = subscriptions_elements(&app);
        let authors: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "subscription-mine-author")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(authors[0], "alice");
        assert_eq!(authors[1], hex::encode([0x02u8; 32]));
        // The status is the raw wire string, not a localized re-label.
        let statuses: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "subscription-mine-status")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(statuses, vec!["active", "pending"]);
    }

    /// The creator column names the author through the one resolver
    /// (`contacts.md` § The private overlay → *Where the nickname paints*): the
    /// viewer's own nickname for them wins over the handle-or-hex chooser, which
    /// stays the fallback.
    #[test]
    fn creator_column_paints_the_viewers_nickname_for_the_author() {
        use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
        let mut app = app_with(vec![
            sub([0x01; 32], "gold", "active", Some("alice")),
            sub([0x02; 32], "silver", "pending", None),
        ]);
        let manager = fauna_conversations::ConversationsManager::new();
        let generation = manager.register_contact_overlays(None);
        manager.apply_contact_overlays(
            generation,
            [(
                hex::encode([0x02u8; 32]),
                ContactOverlay {
                    nickname: Register {
                        stamp: Stamp::new(1, [1; 32]),
                        value: Some("Aunt Bea".into()),
                    },
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        app.conversations.manager = Some(manager);
        let els = subscriptions_elements(&app);
        let authors: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "subscription-mine-author")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(authors, vec!["alice", "Aunt Bea"]);
    }

    /// **The paint assertion.** Every leaf is its own painted line, so a row can
    /// never join an inline run and clip past the terminal's edge — the month-grid
    /// class of bug the registry is blind to (`Element::starts_row`). Registry
    /// presence is asserted above; this asserts the pixels exist too.
    #[test]
    fn every_row_leaf_paints_on_its_own_line() {
        let app = app_with(vec![
            sub([0x01; 32], "gold", "active", Some("alice")),
            sub([0x02; 32], "silver", "pending", Some("bob")),
        ]);
        let els = subscriptions_elements(&app);
        assert!(
            els.iter().all(|e| !e.inline),
            "no element on this page is inline — a row of leaves that joined one \
             run would paint past the terminal edge while every id still resolved"
        );
        let painted = crate::ui::painted_line_texts(&els);
        for needle in ["alice", "gold", "active", "bob", "silver", "pending"] {
            assert_eq!(
                painted.iter().filter(|l| l.contains(needle)).count(),
                1,
                "{needle:?} should occupy exactly one painted line of its own"
            );
        }
    }

    /// A blank claim buffer produces no op at all — a submit with nothing pasted
    /// must not spend a nest round-trip (nor clear an error the user is reading).
    #[test]
    #[cfg(feature = "payments")]
    fn a_blank_claim_code_is_a_no_op() {
        let mut app = app_with(Vec::new());
        assert_eq!(app.settings.subscriptions.claim_code(), None);
        app.settings.subscriptions.claim_input = "   ".to_string();
        assert_eq!(app.settings.subscriptions.claim_code(), None);
        app.settings.subscriptions.claim_input = "  CODE-1  ".to_string();
        assert_eq!(
            app.settings.subscriptions.claim_code().as_deref(),
            Some("CODE-1"),
            "the code is trimmed before it reaches the nest"
        );
    }

    /// The unsubscribe gesture addresses the row's creator by INDEX into the
    /// painted list, so a list that moved under the click resolves to nothing
    /// rather than to the wrong creator.
    #[test]
    fn unsubscribe_resolves_the_row_index_to_its_creator() {
        let app = app_with(vec![
            sub([0x01; 32], "gold", "active", None),
            sub([0x02; 32], "silver", "active", None),
        ]);
        assert_eq!(
            app.settings.subscriptions.author_at(1),
            Some(ActorId([0x02; 32]))
        );
        assert_eq!(app.settings.subscriptions.author_at(2), None);
    }
}
