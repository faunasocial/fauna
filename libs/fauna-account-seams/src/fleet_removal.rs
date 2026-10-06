//! The account runtime's impl of the devices page's fleet-scope removal door
//! ([`fauna_devices_machine::FleetRemoval`]) — the remove-device action's
//! second leg beside `fauna.sync.devices.delete`
//! (`docs/goal/behavior/devices.md` § Removing a Device;
//! `docs/goal/architecture/account-data-taxonomy.md` § The generation
//! machinery → *Fleet-scope reclamation*, clause (4)).
//!
//! # Why it lives here and not in each app
//!
//! `fauna-devices-machine` stays wasm-clean and cannot name the account
//! plane, so the door is injected — and until this module every
//! runtime-hosting seat (tui, linux, the `fauna-ffi` seam windows/macOS/iOS/
//! android funnel through) carried its own near-verbatim adapter. The one
//! decision inside it is a trust-boundary rule, so it is stated once: **a
//! runtime that is absent refuses the removal** (below). It lives in this
//! wasm-capable crate, not the native assembly crate (which re-exports it at
//! its old path), because web serves the same door: `fauna-wasm`'s
//! `account_port` answers the folders chunk's forwarder
//! (`fauna_devices_machine::port`) through this adapter
//! (`account-client-lifecycle.md` § The client-side lifecycle → *The account
//! port*, decision (d)) — so the rule is one body of code on all seven apps.
//!
//! # An absent runtime refuses — at every step
//!
//! The handle is read FRESH on every call through the seat's own `source`
//! (its contract is "`None` before the assembly completes, after a sign-out,
//! or whenever it failed", none of which may be assumed to stay false for a
//! built `DevicesMachine`'s lifetime). Every seat starts the runtime
//! fire-and-forget at login and best-effort, so absence is reachable on all of
//! them: for the seconds the assembly runs, and for a whole session when it
//! failed. Resolving *nothing* then would let the nest deletion proceed alone
//! — the row gone, nothing to retry from, the device a verified member and a
//! wrap target for ever, the user told it was removed: the exact leak clause
//! (4)'s completion rule exists to close. So absence answers
//! [`FleetRemovalRefusal::Unavailable`]: nothing is deleted, the page says the
//! device was not removed, and the row is still there to retry from once the
//! runtime is up. On web the source also answers `None` when the tab's
//! runtime serves another account than the port was minted for (decision
//! (e)), so a machine that outlived an account switch is refused the same way.

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_core::fleet_removal::PendingFleetRemoval;
use fauna_devices_machine::{FleetMembersView, FleetRemoval, FleetRemovalRefusal, NestDeletion};

/// What an absent runtime answers, at every step of the removal.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// [`FleetRemoval`] over a seat's account runtime. `source` is the seat's own
/// fresh read of its live handle — `AccountRuntimeHost::handle` on linux and
/// the `fauna-ffi` seat, the `App`-owned slot on tui, and on web the core
/// chunk's `account_runtime::handle_for` the port's account.
pub struct RuntimeFleetRemoval<S> {
    source: S,
}

impl<S> RuntimeFleetRemoval<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }

    fn handle(&self) -> Result<AccountStoreHandle, String> {
        (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> FleetRemoval for RuntimeFleetRemoval<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn resolve_removal(
        &self,
        row_device_id: &str,
        claimed_principal: Option<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
        self.handle()
            .map_err(FleetRemovalRefusal::Unavailable)?
            .resolve_fleet_removal(row_device_id, claimed_principal)
            .await
    }

    async fn stage_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
    ) -> Result<(), String> {
        // Targets were resolved through a live handle a moment ago; one that
        // has gone since cannot promise to finish, so the page must not delete.
        self.handle()?
            .stage_fleet_removal(PendingFleetRemoval {
                row: row_device_id.to_string(),
                targets,
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn settle_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
        outcome: NestDeletion,
    ) -> Result<(), String> {
        // The intent is already staged, so an error here loses nothing: the
        // runtime's reconcile finishes it from the roster with no gesture.
        self.handle()?
            .settle_fleet_removal(
                PendingFleetRemoval {
                    row: row_device_id.to_string(),
                    targets,
                },
                outcome,
            )
            .await
            .map_err(|e| e.to_string())
    }

    async fn fleet_members(
        &self,
        roster: Vec<(String, Option<[u8; 32]>)>,
    ) -> Result<FleetMembersView, String> {
        // An absent runtime cannot list anyone: the page keeps what it last
        // painted (the machine's keep-on-error rule) rather than blanking a
        // group whose members are still there.
        self.handle()?
            .unaccounted_fleet_members(roster)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn remove_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal> {
        // The same rule as resolve: absence refuses, never "nothing to do".
        self.handle()
            .map_err(FleetRemovalRefusal::Unavailable)?
            .remove_fleet_member(member)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absent() -> RuntimeFleetRemoval<impl Fn() -> Option<AccountStoreHandle> + Send + Sync> {
        RuntimeFleetRemoval::new(|| None)
    }

    /// The trust-boundary rule this module exists to state once: with no
    /// runtime to resolve through, the removal is refused — never resolved to
    /// "no fleet member", which the machine reads as licence to delete the
    /// nest row alone.
    #[tokio::test]
    async fn an_absent_runtime_refuses_the_removal_rather_than_resolving_nothing() {
        let refusal = absent()
            .resolve_removal("aa", Some([0x11; 32]))
            .await
            .expect_err("an absent runtime must refuse");
        assert_eq!(
            refusal,
            FleetRemovalRefusal::Unavailable(RUNTIME_ABSENT.to_string())
        );
    }

    /// The member door's two halves follow the same rule: no runtime, no
    /// list (an error the page reads as "keep what is painted") and no
    /// removal by key.
    #[tokio::test]
    async fn an_absent_runtime_neither_lists_nor_removes_members() {
        let door = absent();
        assert_eq!(
            door.fleet_members(Vec::new()).await,
            Err(RUNTIME_ABSENT.to_string())
        );
        assert_eq!(
            door.remove_member([0x11; 32]).await,
            Err(FleetRemovalRefusal::Unavailable(RUNTIME_ABSENT.to_string()))
        );
    }

    #[tokio::test]
    async fn an_absent_runtime_refuses_to_stage_or_settle() {
        let door = absent();
        assert_eq!(
            door.stage_removal("aa", vec![[0x11; 32]]).await,
            Err(RUNTIME_ABSENT.to_string())
        );
        assert_eq!(
            door.settle_removal("aa", vec![[0x11; 32]], NestDeletion::Gone)
                .await,
            Err(RUNTIME_ABSENT.to_string())
        );
    }
}
