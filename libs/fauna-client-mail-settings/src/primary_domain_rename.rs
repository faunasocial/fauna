//! Shared projection + affordance logic for the **primary-domain rename**
//! workflow on the admin `admin-dns` page.
//!
//! Authority for behavior/data/RPCs: `docs/goal/behavior/mail-primary-domain-
//! rename.md`. Authority for UX/IDs: `tests/e2e-unified/ui.yaml` `admin-dns` (the
//! `admin-dns-rename-*` sheet/banner components + the per-row
//! `admin-dns-domain-rename-button` / `-promote-button` / `-rename-state`).
//!
//! Per priority #2 the rename's client state rides the **existing**
//! [`LocalDomainsSnapshot`](crate::local_domains::LocalDomainsSnapshot) (as the
//! optional [`PrimaryDomainRenameView`] + the [`rename_available`] gate) rather
//! than a second machine — every app already builds + renders that one
//! machine, so the in-flight banner is a dumb render of one more snapshot field,
//! exactly like the mail snapshot's pending-DKIM-rotation banner. The rename is
//! intrinsically about the local-domain rows the page already draws (the
//! affordances render *on* those rows), so co-locating the data avoids a
//! per-app two-snapshot merge.
//!
//! This module owns the pure **projection** (wire [`MailDomainRenameRow`] → the
//! render-ready [`PrimaryDomainRenameView`], resolving the row's opaque 16-byte
//! `domain_id`s back to display names against the domain list) and the pure
//! **precondition** ([`rename_available`], the client-side enable/disable hint
//! for the two-step rule). All *validation* stays nest-authoritative: the client
//! never duplicates the single-active-rename / cert-mode / TLS-posture / SAN-cap
//! rules — it dispatches the action and surfaces any 409-class refusal, the same
//! "the guard is the nest's" pattern the remove-primary affordance uses.

use fauna_mail::domain_rename::RenameState;
use fauna_protocol::bridge_routing::MailDomainRenameRow;
use serde::{Deserialize, Serialize};

use crate::local_domains::LocalDomainView;

/// A render-ready view of the single in-flight primary-domain rename
/// (`mail-primary-domain-rename.md` § Lifecycle). Projected from the wire
/// [`MailDomainRenameRow`]; resolves the row's opaque `domain_id`s to display
/// names, parses the `state` string into the shared [`RenameState`] vocabulary
/// (keeping the raw string for forward-compat display of a state a newer nest
/// might introduce), and pre-derives the per-state affordance flags each app
/// renders as buttons — so no client re-implements the state→action mapping.
///
/// The wall-clock **countdown** to `grace_ends_at` is deliberately *not* computed
/// here (there is no clock in this WASM-safe crate): the view surfaces the raw
/// epoch-millis fields and each app renders the remaining time against its own
/// clock, exactly as `LocalDomainView` surfaces raw `added_at`/`removed_at`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PrimaryDomainRenameView {
    /// 16-byte rename id — scopes the `complete` / `extend` / `abort` actions so
    /// a stale rename can't be actioned across an abort-then-restart race.
    pub rename_id: Vec<u8>,
    /// The lifecycle state as the raw wire string (`requested`, `cert_issuance`,
    /// … `completed`, `aborted`) — rendered verbatim so a forward-compat state a
    /// newer nest wrote still displays even when [`RenameState::from_wire`]
    /// can't classify it.
    pub state: String,
    /// The current-primary (being demoted) domain name, resolved from the domain
    /// list; falls back to the hex `domain_id` if the row is absent (defensive —
    /// the old primary stays a local domain throughout the rename, so this
    /// resolves in practice).
    pub old_primary_domain: String,
    /// The promotion-target (new primary) domain name, resolved likewise.
    pub new_primary_domain: String,
    /// Epoch-millis the rename started (per-app localized in UI).
    pub started_at: i64,
    /// The grace-window length in days the admin chose at `start` (`[1, 30]`).
    pub grace_days: i64,
    /// Epoch-millis the peer-cache-flush grace window ends; `None` until the
    /// anchor flip commits. Drives the client-rendered countdown + the
    /// "Complete now" vs "grace not expired" affordance.
    pub grace_ends_at: Option<i64>,
    /// Epoch-millis the grace watcher promoted the row to `ready_to_complete`;
    /// `None` before that.
    pub ready_to_complete_at: Option<i64>,
    /// The rename has committed the `is_primary` anchor flip and is in the grace
    /// window (`anchor_flip` / `grace` / `ready_to_complete`). The in-flight
    /// banner shows the grace controls only in this phase.
    pub is_post_flip_active: bool,
    /// A pre-flip state (`requested` / `cert_issuance` / `cert_ready`): the
    /// anchor is untouched, so an abort is cheap ("nothing to unwind"). The
    /// abort-confirm dialog names the higher post-flip cost only when this is
    /// false.
    pub is_pre_flip: bool,
    /// Offer "Complete now" without `force` — the grace window has elapsed
    /// (`ready_to_complete`).
    pub can_complete: bool,
    /// Offer "Complete now (early)" *with* `force` — still in `grace`; the
    /// confirm dialog must name the early-cache-flush risk.
    pub can_force_complete: bool,
    /// Offer "Extend grace by N days" — valid from `grace` / `ready_to_complete`.
    pub can_extend: bool,
    /// Offer "Abort" — valid from any non-terminal state (the confirm dialog
    /// names the inverse-re-flip cost when `!is_pre_flip`).
    pub can_abort: bool,
}

impl PrimaryDomainRenameView {
    /// Project a wire rename row into the render-ready view, resolving both
    /// `domain_id`s against the active + soft-deleted domain lists.
    pub fn project(
        row: &MailDomainRenameRow,
        active: &[LocalDomainView],
        soft_deleted: &[LocalDomainView],
    ) -> Self {
        let parsed = RenameState::from_wire(&row.state);
        // Unknown (forward-compat) states offer no actions — we never dispatch an
        // action against a state this build can't reason about.
        let is_terminal = parsed.is_some_and(RenameState::is_terminal);
        let is_pre_flip = parsed.is_some_and(RenameState::is_pre_flip);
        let is_post_flip_active = parsed.is_some_and(RenameState::is_post_flip_active);
        Self {
            rename_id: row.rename_id.to_vec(),
            state: row.state.clone(),
            old_primary_domain: resolve_name(&row.old_primary_domain_id, active, soft_deleted),
            new_primary_domain: resolve_name(&row.new_primary_domain_id, active, soft_deleted),
            started_at: row.started_at,
            grace_days: row.grace_days,
            grace_ends_at: row.grace_ends_at,
            ready_to_complete_at: row.ready_to_complete_at,
            is_post_flip_active,
            is_pre_flip,
            can_complete: parsed == Some(RenameState::ReadyToComplete),
            can_force_complete: parsed == Some(RenameState::Grace),
            // `extend` is valid from grace / ready_to_complete (both post-flip
            // and both have a future/near-deadline the admin may push out).
            can_extend: matches!(
                parsed,
                Some(RenameState::Grace | RenameState::ReadyToComplete)
            ),
            // `abort` is valid from any non-terminal state (including an
            // unclassifiable forward-compat one? no — offer it only for states
            // we can classify as non-terminal, to avoid acting blindly).
            can_abort: parsed.is_some() && !is_terminal,
        }
    }
}

/// Resolve a 16-byte `mail_domains.domain_id` to its display name, searching the
/// active then the soft-deleted list; falls back to the hex id (defensive — a
/// rename always references domains that remain local, so this resolves in the
/// normal path).
fn resolve_name(id: &[u8], active: &[LocalDomainView], soft_deleted: &[LocalDomainView]) -> String {
    active
        .iter()
        .chain(soft_deleted.iter())
        .find(|d| d.domain_id == id)
        .map(|d| d.domain.clone())
        .unwrap_or_else(|| hex::encode(id))
}

/// The client-side enable/disable hint for the "Rename primary domain"
/// affordance: a rename is offerable only when there is a primary **and** at
/// least one active non-primary domain to promote — the two-step rule surfaced
/// as a greyed-out control (`mail-primary-domain-rename.md` § Two-step admin
/// action: the wizard never bundles `add_local_domain`). This is a UX hint only;
/// the nest re-validates (`new_primary_must_be_additional`), so a stale snapshot
/// can never cause an invalid rename — it can at most briefly offer/withhold the
/// button.
pub fn rename_available(active: &[LocalDomainView]) -> bool {
    active.iter().any(|d| d.is_primary) && active.iter().any(|d| !d.is_primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(domain: &str, domain_id: &[u8], is_primary: bool) -> LocalDomainView {
        LocalDomainView {
            domain_id: domain_id.to_vec(),
            domain: domain.to_string(),
            is_primary,
            mta_sts_mode: "testing".to_string(),
            mta_sts_cert_mode: "expand_primary".to_string(),
            mta_sts_max_age_seconds: 86_400,
            spf_record: "v=spf1 mx ~all".to_string(),
            dkim_selector: None,
            dkim_rotation_due: false,
            dkim_selector_activated_at: None,
            catch_all_actor_id: None,
            catch_all_cleared_by_succession_at: None,
            role_address_overrides: Vec::new(),
            dmarc_policy: crate::local_domains::DomainDmarcPolicy::Reject,
            added_at: 1_700_000_000_000,
            removed_at: None,
        }
    }

    fn rename_row(state: &str, old_id: &[u8], new_id: &[u8]) -> MailDomainRenameRow {
        MailDomainRenameRow {
            rename_id: fauna_protocol::ByteBuf::from(vec![0xAAu8; 16]),
            old_primary_domain_id: fauna_protocol::ByteBuf::from(old_id.to_vec()),
            new_primary_domain_id: fauna_protocol::ByteBuf::from(new_id.to_vec()),
            state: state.to_string(),
            started_at: 1_700_000_100_000,
            grace_days: 7,
            initiated_by_actor_id: fauna_protocol::ByteBuf::from(vec![0x1u8; 32]),
            ..Default::default()
        }
    }

    #[test]
    fn rename_available_needs_a_primary_and_a_non_primary() {
        // Only a primary → nothing to promote to.
        let only_primary = vec![view("a.example", &[1u8; 16], true)];
        assert!(!rename_available(&only_primary));

        // Primary + a non-primary additional → offerable.
        let with_additional = vec![
            view("a.example", &[1u8; 16], true),
            view("b.example", &[2u8; 16], false),
        ];
        assert!(rename_available(&with_additional));

        // No domains at all → not offerable.
        assert!(!rename_available(&[]));
    }

    #[test]
    fn project_resolves_domain_names_from_ids() {
        let old_id = [1u8; 16];
        let new_id = [2u8; 16];
        let active = vec![
            view("old.example", &old_id, true),
            view("new.example", &new_id, false),
        ];
        let row = rename_row("cert_issuance", &old_id, &new_id);
        let v = PrimaryDomainRenameView::project(&row, &active, &[]);
        assert_eq!(v.old_primary_domain, "old.example");
        assert_eq!(v.new_primary_domain, "new.example");
        assert_eq!(v.rename_id, vec![0xAAu8; 16]);
        assert_eq!(v.state, "cert_issuance");
    }

    #[test]
    fn project_falls_back_to_hex_for_unresolvable_id() {
        // A pathological row referencing a domain not in either list.
        let row = rename_row("grace", &[9u8; 16], &[8u8; 16]);
        let v = PrimaryDomainRenameView::project(&row, &[], &[]);
        assert_eq!(v.old_primary_domain, hex::encode([9u8; 16]));
        assert_eq!(v.new_primary_domain, hex::encode([8u8; 16]));
    }

    #[test]
    fn pre_flip_state_offers_abort_only() {
        // `requested` / `cert_issuance` / `cert_ready`: cheap abort, no grace
        // controls, no complete/extend.
        for state in ["requested", "cert_issuance", "cert_ready"] {
            let v = PrimaryDomainRenameView::project(
                &rename_row(state, &[1u8; 16], &[2u8; 16]),
                &[],
                &[],
            );
            assert!(v.is_pre_flip, "{state} is pre-flip");
            assert!(!v.is_post_flip_active, "{state} not post-flip");
            assert!(v.can_abort, "{state} can abort");
            assert!(!v.can_complete, "{state} cannot complete");
            assert!(!v.can_force_complete, "{state} cannot force-complete");
            assert!(!v.can_extend, "{state} cannot extend");
        }
    }

    #[test]
    fn grace_state_offers_force_complete_extend_abort() {
        let v = PrimaryDomainRenameView::project(
            &rename_row("grace", &[1u8; 16], &[2u8; 16]),
            &[],
            &[],
        );
        assert!(v.is_post_flip_active);
        assert!(!v.is_pre_flip);
        assert!(!v.can_complete, "grace: not without force");
        assert!(v.can_force_complete, "grace: force-complete offered");
        assert!(v.can_extend);
        assert!(v.can_abort);
    }

    #[test]
    fn ready_to_complete_offers_complete_extend_abort() {
        let v = PrimaryDomainRenameView::project(
            &rename_row("ready_to_complete", &[1u8; 16], &[2u8; 16]),
            &[],
            &[],
        );
        assert!(v.is_post_flip_active);
        assert!(v.can_complete, "ready: complete without force");
        assert!(
            !v.can_force_complete,
            "ready: no force needed (plain complete)"
        );
        assert!(v.can_extend);
        assert!(v.can_abort);
    }

    #[test]
    fn terminal_states_offer_no_actions() {
        for state in ["completed", "aborted"] {
            let v = PrimaryDomainRenameView::project(
                &rename_row(state, &[1u8; 16], &[2u8; 16]),
                &[],
                &[],
            );
            assert!(!v.can_complete, "{state}");
            assert!(!v.can_force_complete, "{state}");
            assert!(!v.can_extend, "{state}");
            assert!(!v.can_abort, "{state}");
        }
    }

    #[test]
    fn unknown_forward_compat_state_offers_no_actions_but_displays() {
        // A state a newer nest introduced that this build can't classify: render
        // the raw string, but never offer an action we can't reason about.
        let v = PrimaryDomainRenameView::project(
            &rename_row("some_future_state", &[1u8; 16], &[2u8; 16]),
            &[],
            &[],
        );
        assert_eq!(v.state, "some_future_state");
        assert!(!v.is_pre_flip);
        assert!(!v.is_post_flip_active);
        assert!(!v.can_complete);
        assert!(!v.can_force_complete);
        assert!(!v.can_extend);
        assert!(!v.can_abort);
    }
}
