//! The yield's nudge: an app runtime that hands the engine role to its
//! co-located sync agent asks the agent for its pass at once, so the freed
//! role is taken now rather than at the agent's next backstop tick
//! (`account-runtime.md` § Multi-instance concurrency → *The agent holds the
//! role when present*, part 4).
//!
//! [`crate::resolve_and_start`] spawns one watch per started runtime, so every
//! desktop host (tui, linux, the `fauna-ffi` seat) runs it with no app glue.
//! It asks through the seat the participation switch already uses
//! ([`EngineHolderNudgeSlot::seat`] → the agent's `ReconcileAccountRuntime`,
//! whose arm runs a non-holder's pass too: that pass is the re-election, and
//! the kernel arbitrates). No seat, or an agent not yet answering → the
//! agent's own tick, as before.
//!
//! The same seat carries every owed-pass ask
//! ([`spawn_holder_pass_owed_nudge`]): a generation key this runtime recovers
//! from escrow beside the agent opens rows only at the agent's next full
//! reconcile, and a `Removed` row a devices-page removal writes here severs
//! the removed device's peer admission only at the agent's next withdrawal
//! snapshot refresh — both asked for at once.

use tokio::sync::watch;

use crate::p2p_participation::EngineHolderNudgeSlot;

/// Watch `role` (an [`AccountStoreHandle::subscribe_engine_role`]) and ask
/// the engine holder published in `slot` for its pass on every
/// holder→non-holder edge; a non-holder→holder edge asks nothing. The task
/// ends when the watch closes — with the runtime.
///
/// [`AccountStoreHandle::subscribe_engine_role`]:
///     fauna_sync_engine::account_runtime::AccountStoreHandle::subscribe_engine_role
pub fn spawn_holder_yield_nudge(
    mut role: watch::Receiver<bool>,
    slot: EngineHolderNudgeSlot,
) -> tokio::task::JoinHandle<()> {
    // Read before the spawn, not inside it: a yield that lands before the
    // task is first polled must read as the edge it is, not as the start.
    let mut held = *role.borrow_and_update();
    tokio::spawn(async move {
        while role.changed().await.is_ok() {
            let now = *role.borrow_and_update();
            if held
                && !now
                && let Some(holder) = slot.current()
            {
                tracing::debug!("engine role yielded: asking the agent for its pass");
                holder.nudge_engine_holder().await;
            }
            held = now;
        }
    })
}

/// Watch `owed` (an [`AccountStoreHandle::subscribe_holder_pass_owed`]) and
/// ask the engine holder published in `slot` for its pass on every move. Two
/// writers move it: this runtime's seed pass recovered a generation key from
/// escrow, and the rows it opens are re-presented by the holder's next full
/// reconcile alone (`account-runtime.md` § Multi-instance concurrency → *The
/// seed-leg role*, part 4, *Escrow recovery*) — without the ask a re-signed-in
/// app beside its agent held every tip-sealed read until the agent's backstop
/// tick; and a removal wrote a `Removed` row, which the serving agent's
/// admission honours only from its next pass (`account-sync-plane.md` § The
/// admission seam → *Validity and severance*) — without the ask the removed
/// device kept pulling chunks for up to a backstop tick. The task ends when
/// the watch closes — with the runtime.
///
/// [`AccountStoreHandle::subscribe_holder_pass_owed`]:
///     fauna_sync_engine::account_runtime::AccountStoreHandle::subscribe_holder_pass_owed
pub fn spawn_holder_pass_owed_nudge(
    mut owed: watch::Receiver<u64>,
    slot: EngineHolderNudgeSlot,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while owed.changed().await.is_ok() {
            owed.borrow_and_update();
            if let Some(holder) = slot.current() {
                tracing::debug!("this runtime owes the agent a pass: asking for it");
                holder.nudge_engine_holder().await;
            }
        }
    })
}
