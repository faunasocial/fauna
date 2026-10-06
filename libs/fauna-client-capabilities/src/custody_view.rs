//! The T16 custody facet's **boundary rows** — one projection of
//! [`crate::view_model`]'s fold, wearing both faces (`uniffi::Record` for the
//! native apps, serde for the web SPA).
//!
//! ## Why these live here, and why there is exactly ONE set of them
//!
//! [`crate::view_model`]'s own types stay pure: `CustodyRowView` and friends
//! carry `[u8; 32]` fixed arrays and an `Option<CustodyScopeSet>`, neither of
//! which crosses UniFFI, and they carry no derives beyond the structural ones.
//! Something has to project them for a boundary — and the mistake worth
//! avoiding is projecting them *per boundary*, once in `fauna-ffi` for the
//! UniFFI apps and again in a `fauna-wasm-*` crate for web. Two projections of
//! one fold is exactly the divergence priority #2 forbids: they would drift on
//! which receipt state reads which way, or on whether a nest-anchored custody
//! is skipped, and the four native apps would quietly render different facts
//! than web.
//!
//! So this module mirrors `fauna-client-pair`'s `trust.rs`: **one row type,
//! `Serialize`/`Deserialize` always, `uniffi::Record` behind an off-by-default
//! `uniffi` feature.** `fauna-ffi` turns that feature on and re-exports these
//! for Swift/Kotlin/C#; the web SPA's wasm crate consumes the very same types
//! through serde with the feature off — which is what keeps this crate
//! wasm-clean, since the UniFFI scaffolding simply is not compiled there.
//! `fauna-core` (`LocalizedText`) already works exactly this way.
//!
//! The fold's own types are untouched by all of this, which is what the
//! "`fauna-client-capabilities` stays free of UniFFI scaffolding" rule in
//! `fauna-client-pair/src/trust.rs` is actually protecting: the *computation*
//! types stay pure, and the boundary layer is separate and explicit.
//!
//! ## What a consumer gets, and what it must still do
//!
//! [`CustodyFacetView::from_snapshot`] folds a [`CustodyFacetSnapshot`] into
//! rows that already carry **both shared label decisions** —
//! [`crate::view_model::custody_receipt_status_display`] and
//! [`crate::view_model::custody_held_bytes_display`] — as
//! [`LocalizedText`] plus epoch seconds. A consumer resolves those through its
//! own i18n runtime and formats the timestamp itself; it re-assembles no
//! strings and re-derives no state→key mapping. The timestamp is deliberately
//! not rendered here: `fauna_core::format::format_unix_local` needs the OS
//! timezone database, which wasm32 lacks, so rendering it would cost this crate
//! the wasm-cleanliness the web leg depends on.

use serde::{Deserialize, Serialize};

use fauna_core::custody_grant::CustodyScopeSet;
use fauna_core::localized::LocalizedText;

use crate::view_model::{
    self, CustodyFacetSnapshot, CustodyOfferView, CustodyReceiptView, CustodyRowView,
    GrantLiveness, HeldCustodyView, ReceiptState,
};

/// A grant's liveness relative to now — boundary projection of
/// [`GrantLiveness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CustodyLiveness {
    Active,
    ExpiringSoon,
    Expired,
    AutoRenewing,
}

impl From<GrantLiveness> for CustodyLiveness {
    fn from(l: GrantLiveness) -> Self {
        match l {
            GrantLiveness::Active => Self::Active,
            GrantLiveness::ExpiringSoon => Self::ExpiringSoon,
            GrantLiveness::Expired => Self::Expired,
            GrantLiveness::AutoRenewing => Self::AutoRenewing,
        }
    }
}

/// The three receipt-freshness states — boundary projection of
/// [`ReceiptState`]. Crosses as the raw state *as well as* its rendered label,
/// because a consumer needs the fact for its own render decisions (a degraded
/// style, a sort order) and the label so it never re-derives which state reads
/// which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CustodyReceiptStateView {
    Fresh,
    Stale,
    NoReceiptYet,
}

impl From<ReceiptState> for CustodyReceiptStateView {
    fn from(s: ReceiptState) -> Self {
        match s {
            ReceiptState::Fresh => Self::Fresh,
            ReceiptState::Stale => Self::Stale,
            ReceiptState::NoReceiptYet => Self::NoReceiptYet,
        }
    }
}

/// The granted coverage — boundary projection of [`CustodyScopeSet`], which is
/// a pure `fauna-core` enum carrying no boundary derives. The two fields are
/// never both populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyScopesView {
    /// `true` = the owner's whole single-principal scope set, current and
    /// future. The v1 mint always produces this.
    pub whole_account: bool,
    /// The named scope strings, when this is the subset form. Empty when
    /// [`Self::whole_account`] is set.
    pub scopes: Vec<String>,
}

impl From<&CustodyScopeSet> for CustodyScopesView {
    fn from(s: &CustodyScopeSet) -> Self {
        match s {
            CustodyScopeSet::Account => Self {
                whole_account: true,
                scopes: Vec::new(),
            },
            CustodyScopeSet::Scopes(list) => Self {
                whole_account: false,
                scopes: list.clone(),
            },
            // A set a newer build minted covers no scope this build can name.
            CustodyScopeSet::Unknown(_) => Self {
                whole_account: false,
                scopes: Vec::new(),
            },
        }
    }
}

/// The receipt facts a custody row renders, with both shared label decisions
/// already folded in.
///
/// The labels ride the row rather than being separate calls because the join is
/// where the mistakes live: which state maps to which key (the A7 three-state
/// honesty rule), and that `degraded` is **orthogonal to freshness** — a FRESH
/// receipt can truthfully report dropped payload, so a consumer renders the
/// degraded marker *alongside* the status line, never instead of it. Joined
/// once here rather than once per consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyReceiptRowView {
    /// The status line's key, carrying a `{when}` placeholder for the two
    /// timestamped states. Resolve it, then substitute
    /// [`Self::attested_at_secs`].
    pub status_label: LocalizedText,
    /// Epoch SECONDS for the consumer to format and substitute into `{when}`,
    /// or `None` when there is no receipt to date.
    pub attested_at_secs: Option<i64>,
    /// The held-bytes line's key, carrying `{held}` and `{cap}` placeholders
    /// filled from the two resolved fields below.
    pub held_bytes_label: LocalizedText,
    /// Held bytes, or the shared em-dash placeholder when no receipt has landed
    /// — "nothing confirmed yet" and "confirmed zero bytes" are different facts
    /// and must not look alike. Resolve before substituting: a `LocalizedText`
    /// argument is a flat string.
    pub held: LocalizedText,
    /// The budget in force, same placeholder rule.
    pub cap: LocalizedText,
    /// The receipt honestly reports evicted or capped-short coverage. Render
    /// [`crate::view_model::CUSTODY_DEGRADED_BADGE_KEY`] alongside the status
    /// line when set.
    pub degraded: bool,
    /// The raw held-bytes figure, for a consumer that needs the number itself
    /// (a progress bar against the budget). `None` with no receipt yet.
    pub held_bytes: Option<u64>,
    /// The cap the receipt attested to — distinct from the *budget in force*,
    /// which a held row carries directly. `None` with no receipt yet.
    pub attested_cap: Option<u64>,
}

/// Fold a receipt (or its absence) into the boundary row, applying both shared
/// label decisions. `state` is always meaningful — `NoReceiptYet` is a real
/// state the row must render, never an empty line.
fn project_receipt(
    state: ReceiptState,
    receipt: Option<&CustodyReceiptView>,
) -> CustodyReceiptRowView {
    let status =
        view_model::custody_receipt_status_display(state, receipt.map(|r| r.attested_at_micros));
    let bytes = view_model::custody_held_bytes_display(receipt);
    CustodyReceiptRowView {
        status_label: status.label,
        attested_at_secs: status.attested_at_secs,
        held_bytes_label: bytes.label,
        held: bytes.held,
        cap: bytes.cap,
        degraded: bytes.degraded,
        held_bytes: receipt.map(|r| r.held_bytes),
        attested_cap: receipt.map(|r| r.retained_bytes_cap),
    }
}

/// One owner-side custody row — "who holds my data" (`custody-holder-card`),
/// boundary projection of [`CustodyRowView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyHolderRowView {
    /// The handle every gesture names. Acts carry the **grant id, never the row
    /// index** — a refold re-orders rows, so an index captured at paint time can
    /// address a different custody by the time the act runs.
    pub grant_id: Vec<u8>,
    /// The counterpart account holding custody (32 bytes).
    pub host: Vec<u8>,
    /// The serving principal key the ceremony's accept bound, or `None` while
    /// the ceremony is still pending the host's consent. This is the holder a
    /// revoke names.
    pub custodian_key: Option<Vec<u8>>,
    /// `Some` = the bound custodian is the host's NEST, so this row belongs to
    /// the **Nests** page's `nest-trust-custody-*` family and a Devices surface
    /// must skip it. One custody never renders in both places.
    pub custodian_nest_url: Option<String>,
    pub scopes: Option<CustodyScopesView>,
    /// The minted grant's `lasts-until` (epoch seconds); `None` while pending.
    pub lasts_until: Option<i64>,
    /// `None` while pending — nothing is minted to be live or lapsed yet.
    pub liveness: Option<CustodyLiveness>,
    pub receipt_state: CustodyReceiptStateView,
    pub receipt: CustodyReceiptRowView,
    /// The ceremony has not completed (offer out, or accept captured but the
    /// mint/deliver still owed) — the row renders as pending, and the revoke
    /// control is disabled because there is nothing minted to revoke.
    pub pending: bool,
}

impl From<&CustodyRowView> for CustodyHolderRowView {
    fn from(r: &CustodyRowView) -> Self {
        Self {
            grant_id: r.grant_id.clone(),
            host: r.host.to_vec(),
            custodian_key: r.custodian_key.map(|k| k.to_vec()),
            custodian_nest_url: r.custodian_nest_url.clone(),
            scopes: r.scopes.as_ref().map(CustodyScopesView::from),
            lasts_until: r.lasts_until.map(|s| s as i64),
            liveness: r.liveness.map(CustodyLiveness::from),
            receipt_state: r.receipt_state.into(),
            receipt: project_receipt(r.receipt_state, r.receipt.as_ref()),
            pending: r.pending,
        }
    }
}

/// One host-side held custody — "what I hold for others"
/// (`custody-held-card`), boundary projection of [`HeldCustodyView`].
///
/// ⚠ A consumer whose app cannot reach the W3 (account-data-plane.md § Workstreams) account store must NOT render
/// this family: `ui/devices.md` § Custody facet defines the host-side card *as*
/// its budget input and stop control, and neither can succeed without a store
/// handle. The row still crosses so the boundary reports what the fold found
/// rather than lying by omission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyHeldRowView {
    pub grant_id: Vec<u8>,
    /// Whose data this device holds (32 bytes).
    pub owner: Vec<u8>,
    pub scopes: Option<CustodyScopesView>,
    /// The budget in force. Seeded from the accept; a caller with registry
    /// reach overlays the row's live value before projecting.
    pub retained_bytes_cap: u64,
    /// The `custody-held-budget-input` seed text for this row, on the shared
    /// 1024-unit byte scale. Resolve through the consumer's own string lookup.
    pub budget_draft: LocalizedText,
    pub receipt: CustodyReceiptRowView,
    /// The host stopped holding — a registry-row fact, so `false` for any
    /// consumer that could not apply the overlay.
    pub stopped: bool,
}

/// One incoming custody offer awaiting consent (`custody-offer-card` —
/// indexed: two owners can have offers pending at once), boundary projection of
/// [`CustodyOfferView`].
///
/// ⚠ Same caveat as [`CustodyHeldRowView`]: accepting records fine but leaves
/// the registry row owed forever without a store handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyOfferRowView {
    pub grant_id: Vec<u8>,
    /// The account asking this device to hold (32 bytes).
    pub owner: Vec<u8>,
    pub scopes: CustodyScopesView,
    /// When the offer arrived (epoch seconds).
    pub offered_at_secs: i64,
    /// A NEST can hold this custody (the owner named its own nest to pull
    /// from) — the fold's half of the consent card's target
    /// select (`custody-offer-target-select`). The other half is a pinned nest
    /// identity, which is process state, so the select renders only when a
    /// shell also holds that pin: the UniFFI face answers the whole question in
    /// `custody_offer_shows_target_select` so no leg re-derives it.
    #[serde(default)]
    pub nest_can_hold: bool,
}

/// One place the owner could send a custody offer — boundary projection of
/// `fauna_client_conversations::CustodyMintCandidate`, the options behind
/// `custody-mint-host-select`.
///
/// The row lives here with its siblings rather than at either boundary, for
/// the module's own reason: one row type, one set of field names, no chance of
/// the four native apps drifting on what a candidate *is*. The conversion from
/// the session-derived candidate lives in `fauna-client-custody`, which is
/// where both types are visible — this crate is wasm-clean and does not know
/// the conversations crate.
///
/// ⚠ **Only the UniFFI face carries this today.** The mint is native-only —
/// it needs a conversations session to post the offer on (`ui/devices.md`
/// § Where logic lives) — so the web SPA has no minting path to feed. The row
/// sits here anyway so that a wasm face, if web ever gains a session, reads the
/// same candidate every native app does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyMintCandidateView {
    /// The account the offer would address (32 bytes) — the `host` a mint act
    /// names.
    pub host: Vec<u8>,
    /// The conversation channel (hex) the ceremony would ride. The offer is
    /// posted over an EXISTING conversation: creating the DM is a shipped user
    /// act, not ceremony business.
    pub channel_hex: String,
    /// The option's rendered text — the counterpart as the thread list shows
    /// them, falling back to the channel hex for a thread with no summary yet.
    /// Already resolved, so a leg renders it directly.
    pub label: String,
}

/// The whole custody facet in one crossing — boundary projection of
/// [`CustodyFacetSnapshot`], the same bundle every app renders from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CustodyFacetView {
    /// Owner side — "who holds my data" (`custody-holder-card`).
    pub rows: Vec<CustodyHolderRowView>,
    /// Host side — "what I hold for others" (`custody-held-card`).
    pub held: Vec<CustodyHeldRowView>,
    /// Incoming offers awaiting consent (`custody-offer-card`, indexed).
    pub offers: Vec<CustodyOfferRowView>,
}

impl CustodyFacetView {
    /// Project a folded facet into its boundary rows, applying every shared
    /// label decision on the way out.
    pub fn from_snapshot(f: &CustodyFacetSnapshot) -> Self {
        // The budget seed texts come from the shared `budget_draft_texts`,
        // which returns one per held row in `held` order — zipped here rather
        // than re-derived, so no consumer hand-rolls the loop and drifts on
        // which value seeds the input (it is the row's live cap, never the
        // accept's).
        let drafts = view_model::budget_draft_texts(f);
        Self {
            rows: f.rows.iter().map(CustodyHolderRowView::from).collect(),
            held: f
                .held
                .iter()
                .zip(drafts)
                .map(|(h, draft)| project_held(h, draft))
                .collect(),
            offers: f.offers.iter().map(project_offer).collect(),
        }
    }
}

fn project_held(h: &HeldCustodyView, budget_draft: LocalizedText) -> CustodyHeldRowView {
    CustodyHeldRowView {
        grant_id: h.grant_id.clone(),
        owner: h.owner.to_vec(),
        scopes: h.scopes.as_ref().map(CustodyScopesView::from),
        retained_bytes_cap: h.retained_bytes_cap,
        budget_draft,
        // A held row's own freshness is not folded by the shared side — the
        // three-state honesty rule is an OWNER-side promise about a custodian.
        // The host card renders the held-bytes line, so the state passed here
        // only selects between the numbers and the placeholders.
        receipt: project_receipt(
            match h.receipt {
                Some(_) => ReceiptState::Fresh,
                None => ReceiptState::NoReceiptYet,
            },
            h.receipt.as_ref(),
        ),
        stopped: h.stopped,
    }
}

fn project_offer(o: &CustodyOfferView) -> CustodyOfferRowView {
    CustodyOfferRowView {
        grant_id: o.grant_id.clone(),
        owner: o.owner.to_vec(),
        scopes: CustodyScopesView::from(&o.scopes),
        offered_at_secs: (o.offered_at_micros / 1_000_000) as i64,
        nest_can_hold: o.nest_can_hold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(held: u64, cap: u64, degraded: bool) -> CustodyReceiptView {
        CustodyReceiptView {
            held_bytes: held,
            retained_bytes_cap: cap,
            degraded,
            attested_at_micros: 5_000_000,
        }
    }

    fn holder_row() -> CustodyRowView {
        CustodyRowView {
            grant_id: vec![7u8; 16],
            host: [3u8; 32],
            custodian_key: Some([4u8; 32]),
            custodian_nest_url: None,
            scopes: Some(CustodyScopeSet::Account),
            lasts_until: Some(1_800_000_000),
            liveness: Some(GrantLiveness::Active),
            receipt: Some(receipt(1024, 4096, false)),
            receipt_state: ReceiptState::Fresh,
            pending: false,
        }
    }

    /// The projection carries the fold's facts across, ids widened to the
    /// `Vec<u8>` a boundary needs and timestamps to epoch seconds.
    #[test]
    fn holder_row_projection_preserves_the_folds_facts() {
        let r = holder_row();
        let v = CustodyHolderRowView::from(&r);
        assert_eq!(v.grant_id, r.grant_id);
        assert_eq!(v.host, r.host.to_vec());
        assert_eq!(v.custodian_key, Some(vec![4u8; 32]));
        assert_eq!(v.custodian_nest_url, None);
        assert_eq!(v.lasts_until, Some(1_800_000_000));
        assert_eq!(v.liveness, Some(CustodyLiveness::Active));
        assert_eq!(v.receipt_state, CustodyReceiptStateView::Fresh);
        assert!(!v.pending);
        assert_eq!(v.receipt.held_bytes, Some(1024));
        assert_eq!(v.receipt.attested_cap, Some(4096));
        assert_eq!(
            v.scopes,
            Some(CustodyScopesView {
                whole_account: true,
                scopes: Vec::new(),
            })
        );
    }

    /// The A7 three-state honesty rule survives the boundary: three states,
    /// three distinct label keys, and `NoReceiptYet` carries no timestamp even
    /// when one is available — "no confirmation yet" must never render a date.
    #[test]
    fn the_three_receipt_states_project_to_three_distinct_labels() {
        let r = receipt(1, 2, false);
        let fresh = project_receipt(ReceiptState::Fresh, Some(&r));
        let stale = project_receipt(ReceiptState::Stale, Some(&r));
        let none = project_receipt(ReceiptState::NoReceiptYet, Some(&r));

        assert_ne!(fresh.status_label, stale.status_label);
        assert_ne!(stale.status_label, none.status_label);
        assert_ne!(fresh.status_label, none.status_label);

        assert_eq!(fresh.attested_at_secs, Some(5));
        assert_eq!(stale.attested_at_secs, Some(5));
        assert_eq!(
            none.attested_at_secs, None,
            "a no-receipt-yet row must render no timestamp"
        );
    }

    /// A row with no receipt renders the shared placeholder, not zeroes —
    /// "nothing confirmed yet" and "confirmed zero bytes" are different facts.
    /// And `degraded` stays orthogonal to freshness: a FRESH receipt can
    /// truthfully report dropped payload.
    #[test]
    fn no_receipt_renders_placeholders_and_degraded_stays_orthogonal() {
        let empty = project_receipt(ReceiptState::NoReceiptYet, None);
        let dash = LocalizedText::key(view_model::CUSTODY_NO_RECEIPT_PLACEHOLDER);
        assert_eq!(empty.held, dash);
        assert_eq!(empty.cap, dash);
        assert_eq!(empty.held_bytes, None);
        assert!(!empty.degraded);

        let r = receipt(8, 16, true);
        let fresh_but_degraded = project_receipt(ReceiptState::Fresh, Some(&r));
        assert!(fresh_but_degraded.degraded);
        assert_eq!(
            fresh_but_degraded.status_label,
            project_receipt(ReceiptState::Fresh, Some(&receipt(8, 16, false))).status_label,
            "degraded must not change which freshness label the row renders"
        );
    }

    /// The nest-anchored / device split crosses intact, so a Devices surface can
    /// skip the rows that belong to the Nests page — one custody never renders
    /// in both.
    #[test]
    fn the_nest_anchored_split_crosses_intact() {
        let mut nest_bound = holder_row();
        nest_bound.custodian_nest_url = Some("https://nest.example".to_string());
        let facet = CustodyFacetSnapshot {
            rows: vec![holder_row(), nest_bound],
            held: Vec::new(),
            offers: Vec::new(),
        };
        let v = CustodyFacetView::from_snapshot(&facet);
        assert_eq!(v.rows.len(), 2);
        assert!(v.rows[0].custodian_nest_url.is_none());
        assert_eq!(
            v.rows[1].custodian_nest_url.as_deref(),
            Some("https://nest.example")
        );
    }

    /// A pending row exposes no custodian key, which is exactly what disables a
    /// consumer's revoke control — nothing is minted to revoke yet.
    #[test]
    fn a_pending_row_carries_no_holder_to_revoke_against() {
        let mut r = holder_row();
        r.pending = true;
        r.custodian_key = None;
        r.liveness = None;
        r.lasts_until = None;
        let v = CustodyHolderRowView::from(&r);
        assert!(v.pending);
        assert_eq!(v.custodian_key, None);
        assert_eq!(v.liveness, None);
        assert_eq!(v.lasts_until, None);
    }

    /// The held rows' budget seed texts come from the shared
    /// `budget_draft_texts` in `held` order — one per row, zipped rather than
    /// re-derived.
    #[test]
    fn held_rows_carry_the_shared_budget_seed_text_in_order() {
        let facet = CustodyFacetSnapshot {
            rows: Vec::new(),
            held: vec![
                HeldCustodyView {
                    grant_id: vec![1u8; 16],
                    owner: [9u8; 32],
                    scopes: Some(CustodyScopeSet::Scopes(vec!["mail".into()])),
                    retained_bytes_cap: 2048,
                    receipt: None,
                    stopped: false,
                },
                HeldCustodyView {
                    grant_id: vec![2u8; 16],
                    owner: [8u8; 32],
                    scopes: None,
                    retained_bytes_cap: 4096,
                    receipt: Some(receipt(100, 4096, false)),
                    stopped: true,
                },
            ],
            offers: Vec::new(),
        };
        let expected = view_model::budget_draft_texts(&facet);
        let v = CustodyFacetView::from_snapshot(&facet);
        assert_eq!(v.held.len(), 2);
        assert_eq!(v.held[0].budget_draft, expected[0]);
        assert_eq!(v.held[1].budget_draft, expected[1]);
        assert_eq!(
            v.held[0].scopes,
            Some(CustodyScopesView {
                whole_account: false,
                scopes: vec!["mail".to_string()],
            })
        );
        assert_eq!(v.held[1].scopes, None);
        // The row with no receipt renders placeholders, not the cap it was
        // seeded with — the seed lives on `budget_draft`, not on the receipt.
        assert_eq!(v.held[0].receipt.held_bytes, None);
        assert_eq!(v.held[1].receipt.held_bytes, Some(100));
    }

    /// An offer's arrival time crosses as epoch seconds, matching every sibling
    /// timestamp on this boundary.
    #[test]
    fn an_offer_projects_its_arrival_as_epoch_seconds() {
        let o = CustodyOfferView {
            grant_id: vec![5u8; 16],
            owner: [6u8; 32],
            scopes: CustodyScopeSet::Account,
            offered_at_micros: 7_500_000,
            nest_can_hold: false,
        };
        let v = project_offer(&o);
        assert_eq!(v.grant_id, vec![5u8; 16]);
        assert_eq!(v.owner, vec![6u8; 32]);
        assert_eq!(v.offered_at_secs, 7);
        assert!(v.scopes.whole_account);
        assert!(!v.nest_can_hold);
    }

    /// The nest-can-hold fact crosses: without it a UniFFI leg could never
    /// offer the consent card's target select, and would bind every accept
    /// to the device.
    #[test]
    fn an_offers_nest_can_hold_fact_crosses() {
        let o = CustodyOfferView {
            grant_id: vec![5u8; 16],
            owner: [6u8; 32],
            scopes: CustodyScopeSet::Account,
            offered_at_micros: 0,
            nest_can_hold: true,
        };
        assert!(project_offer(&o).nest_can_hold);
    }

    /// The serde face — the one the web SPA reads — round-trips the whole
    /// bundle. This is what makes ONE row type serving both boundaries a
    /// checkable claim rather than an aspiration: if a field stops crossing for
    /// web, this fails here rather than silently in the browser.
    #[test]
    fn the_serde_face_round_trips_the_whole_bundle() {
        let facet = CustodyFacetSnapshot {
            rows: vec![holder_row()],
            held: vec![HeldCustodyView {
                grant_id: vec![1u8; 16],
                owner: [9u8; 32],
                scopes: Some(CustodyScopeSet::Scopes(vec!["mail".into()])),
                retained_bytes_cap: 2048,
                receipt: Some(receipt(10, 2048, true)),
                stopped: false,
            }],
            offers: vec![CustodyOfferView {
                grant_id: vec![5u8; 16],
                owner: [6u8; 32],
                scopes: CustodyScopeSet::Account,
                offered_at_micros: 7_500_000,
                // Non-default on purpose: the round-trip must prove the
                // nest-can-hold flag crosses the web boundary.
                nest_can_hold: true,
            }],
        };
        let view = CustodyFacetView::from_snapshot(&facet);
        let json = serde_json::to_string(&view).expect("serialize");
        let back: CustodyFacetView = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, view);
        // And the degraded bit specifically survives — it is the one field a
        // consumer must render alongside, never instead of, the status line.
        assert!(back.held[0].receipt.degraded);
    }
}
