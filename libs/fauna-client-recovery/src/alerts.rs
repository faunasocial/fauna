//! The pending-replacement critical alert — the "loud on every surface" half
//! of the 30-day window.
//!
//! `identity-succession.md` § The RecoveryKey → *Replacement* requires a
//! seed-alone replacement to be loud **on every device and client surface for
//! the whole window**, and that requirement is why this module exists rather
//! than a Settings-page element: a user who never opens Settings would never
//! see it, which satisfies the letter and misses the point.
//!
//! The surface it posts to is the one already built for exactly this class of
//! condition — the cross-page `critical-alerts` banner
//! (`critical-alerts.md`): present on every authenticated page,
//! destructive-styled, **non-dismissable while active**. Its severity bar is
//! "possible compromise of identity/keys, or a condition that will destroy
//! user-irrecoverable data if unaddressed", and a seed-alone replacement the
//! user did not request clears it twice over — the request is authorized by
//! the identity seed *alone*, so an unrecognized window means someone else
//! holds the seed; and if it closes uncontested the attacker's key becomes the
//! account's recovery root while the user's own kit stops working, permanently.
//!
//! **The alert is a projection of the nest's answer, never a stored flag.**
//! `critical-alerts.md` § Mechanism → *Lifetime* makes the detector the source
//! of truth precisely so a stale alarm cannot outlive its condition, so every
//! entry point here takes the freshly-read status and posts *or clears* from
//! it — there is deliberately no "post" without a matching "clear" path.

use fauna_core::identity::ActorId;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::error::Result;
use crate::nest::RecoveryClient;
use crate::replacement::{PendingReplacement, pending_replacement};

/// The key, the copy and the per-source sync live below the account runtime's
/// secondary leg, which feeds the same alert from every linked nest
/// (`fauna_client_core::recovery_pending`, whose module note says how the
/// readings merge).
pub use fauna_client_core::recovery_pending::{
    BOUND_SOURCE, LinkedReading, alert_key, linked_source, linked_windows,
    pending_replacement_alert_lines, record_linked_readings, retain_linked_sources,
    sync_linked_readings, sync_pending_replacement_from,
};
#[cfg(test)]
use fauna_client_core::recovery_pending::{PENDING_ALARM_DETAIL_KEY, PENDING_ALARM_KEY};

/// Post or clear **the bound nest's** reading of this account's
/// pending-replacement alert from an already-read status.
///
/// `pending: None` clears the bound nest's reading — and with it the alert,
/// unless a linked nest's reading still holds it: a window a linked nest
/// raised clears only when that nest reads nothing pending
/// ([`sync_pending_replacement_from`]). A vetoed or landed window must take
/// its banner with it, and a caller that only knew how to post would leave a
/// non-dismissable alarm about a window that no longer exists.
pub fn sync_pending_replacement_alert(
    alerts: &fauna_client_alerts::CriticalAlerts,
    actor_id: &ActorId,
    pending: Option<&PendingReplacement>,
    now: i64,
) {
    if let Some(p) = pending {
        tracing::warn!(
            lands_at = p.lands_at,
            remaining_secs = p.remaining_secs(now),
            "a seed-alone RecoveryKey replacement is pending — raising the critical alert"
        );
    }
    sync_pending_replacement_from(alerts, actor_id, BOUND_SOURCE, pending, now);
}

/// Read the pending-replacement status and sync the alert from it.
///
/// The one call a client's session-start and periodic refresh make. It is
/// deliberately **not** tied to the Settings page's lifecycle: the condition
/// this raises must reach a user who never opens Settings, and a poll driven
/// by the page that displays the veto button would only ever fire for users
/// already looking at it.
///
/// A transport failure is returned, **not** swallowed into a cleared alert —
/// unreachable is not the same as resolved, and clearing on error would let a
/// nest that merely went offline silence a live compromise warning. The caller
/// retries on its next pass; the previously-posted alert stands in the
/// meantime, which is the correct fail-safe direction for this severity class.
pub async fn refresh_pending_replacement_alert<R>(
    client: &RecoveryClient<R>,
    alerts: &fauna_client_alerts::CriticalAlerts,
    actor_id: &ActorId,
    now: i64,
) -> Result<Option<PendingReplacement>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let pending = pending_replacement(client).await?;
    sync_pending_replacement_alert(alerts, actor_id, pending.as_ref(), now);
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_alerts::CriticalAlerts;

    fn actor(byte: u8) -> ActorId {
        ActorId([byte; 32])
    }

    fn pending(pubkey_hex: &str, lands_at: i64) -> PendingReplacement {
        PendingReplacement {
            new_recovery_pubkey_hex: pubkey_hex.to_string(),
            requested_at: 0,
            lands_at,
        }
    }

    #[test]
    fn the_key_is_identity_scoped() {
        assert_ne!(alert_key(&actor(1)), alert_key(&actor(2)));
        assert!(alert_key(&actor(0xab)).starts_with("recovery-replacement-pending:"));
    }

    #[test]
    fn the_countdown_rounds_up_so_a_live_window_never_reads_as_lost() {
        let p = pending(&"ab".repeat(32), 100);
        // One second left is still "1 day", never "0 days" — a window in its
        // final hours must not read as already over.
        let lines = pending_replacement_alert_lines(&p, 99);
        assert_eq!(lines[1].args["days"], "1");

        // 47 hours left is "2 days", not "1" — rounding down would tell the
        // user they have less time than they do.
        let p = pending(&"ab".repeat(32), 47 * 3600);
        let lines = pending_replacement_alert_lines(&p, 0);
        assert_eq!(lines[1].args["days"], "2");
    }

    #[test]
    fn an_elapsed_window_saturates_at_zero_rather_than_wrapping() {
        let p = pending(&"ab".repeat(32), 100);
        let lines = pending_replacement_alert_lines(&p, 5_000);
        assert_eq!(lines[1].args["days"], "0");
    }

    #[test]
    fn the_fingerprint_is_the_pending_keys_leading_hex() {
        let p = pending("0123456789abcdef0123456789abcdef", 100);
        let lines = pending_replacement_alert_lines(&p, 0);
        assert_eq!(lines[1].args["fingerprint"], "0123456789abcdef");
    }

    #[test]
    fn both_lines_are_localized_keys_never_composed_prose() {
        let p = pending(&"ab".repeat(32), 100);
        let lines = pending_replacement_alert_lines(&p, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].key, PENDING_ALARM_KEY);
        assert_eq!(lines[1].key, PENDING_ALARM_DETAIL_KEY);
        // The headline carries no args — it must read as a complete sentence
        // on its own, since it is the line a user sees first.
        assert!(lines[0].args.is_empty());
    }

    #[test]
    fn syncing_a_window_posts_and_syncing_none_clears() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);
        let p = pending(&"cd".repeat(32), 10_000);

        sync_pending_replacement_alert(&alerts, &id, Some(&p), 0);
        assert_eq!(alerts.active().len(), 1);
        assert_eq!(alerts.active()[0].key, alert_key(&id));

        // A vetoed or landed window must take its banner with it — the alert
        // is non-dismissable, so nothing else would ever remove it.
        sync_pending_replacement_alert(&alerts, &id, None, 0);
        assert!(alerts.active().is_empty());
    }

    #[test]
    fn re_syncing_the_same_account_updates_rather_than_stacks() {
        let alerts = CriticalAlerts::new();
        let id = actor(7);

        sync_pending_replacement_alert(&alerts, &id, Some(&pending(&"cd".repeat(32), 90_000)), 0);
        sync_pending_replacement_alert(
            &alerts,
            &id,
            Some(&pending(&"cd".repeat(32), 90_000)),
            86_400,
        );
        assert_eq!(alerts.active().len(), 1);
        // ...and the countdown actually moved.
        assert_eq!(alerts.active()[0].lines[1].args["days"], "1");
    }

    #[test]
    fn two_accounts_hold_independent_alerts() {
        let alerts = CriticalAlerts::new();
        let (a, b) = (actor(1), actor(2));

        sync_pending_replacement_alert(&alerts, &a, Some(&pending(&"11".repeat(32), 10_000)), 0);
        sync_pending_replacement_alert(&alerts, &b, Some(&pending(&"22".repeat(32), 10_000)), 0);
        assert_eq!(alerts.active().len(), 2);

        // Clearing one leaves the other standing — the account switch case the
        // Lifetime rule is about clears via `clear_all`, not by collision.
        sync_pending_replacement_alert(&alerts, &a, None, 0);
        assert_eq!(alerts.active().len(), 1);
        assert_eq!(alerts.active()[0].key, alert_key(&b));
    }
}
