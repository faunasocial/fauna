//! **Why** an account's runtime is being stopped, and how long a stop may take
//! — the two facts every host of the driver states the same way, the browser
//! tab included (`account-client-lifecycle.md` § The client-side lifecycle →
//! *Ruling (4), the teardown rider*).
//!
//! Here rather than in the native assembly crate
//! (`fauna-client-account-runtime`, which re-exports all three at their old
//! paths) because web hosts the driver too and that crate is native-only by
//! design: a second enum on web would be a second place the sign-out and the
//! switch could drift apart.

use std::time::Duration;

use super::{AccountStoreHandle, ENROLLMENT_RETIRE_BUDGET, SIGN_OUT_PASS_GRACE};

/// How long a teardown waits for the account runtime to stop before erasing
/// anyway — **one** budget for the whole stop, not one per half.
///
/// The user asked to be signed out, and "up to 5 s" is the promise: waiting
/// for an assembly and then stopping what it produced are two halves of one
/// stop, so they share the clock. It is shared rather than per-app because the
/// number is part of the contract `apps/account-scoping.md` § Erasure follows
/// scope states — "a named bounded budget" — not a per-host taste.
pub const ACCOUNT_RUNTIME_STOP_BUDGET: Duration = Duration::from_secs(5);

// A sign-out's stop is three things on this one clock: the pass it lands in
// (cut after `SIGN_OUT_PASS_GRACE`), the enrollment retirement
// (`ENROLLMENT_RETIRE_BUDGET`), then the store's own shutdown. A retirement
// the budget cannot fit is an erase over a live grant — a device row nobody
// can ever retire — so the sum is checked here, where the budget lives.
const _: () = assert!(
    SIGN_OUT_PASS_GRACE.as_millis() + ENROLLMENT_RETIRE_BUDGET.as_millis()
        < ACCOUNT_RUNTIME_STOP_BUDGET.as_millis()
);

/// **Why** an account's runtime is being stopped — the one fact the stop
/// cannot infer and must not guess, because the two answers differ in what
/// the nest is told (`sync-agent-credentials.md` § Credential model → *The
/// signed-out reconcile*, the nest-side leg, 2026-09-14).
///
/// The machine's store principal — its writer key and the enrollment grant
/// the nest holds for it — lives in the per-actor credential slot. A stop
/// that is followed by the **erase of that slot** must retire the grant
/// nest-side first, while the runtime still holds the key and the app still
/// holds a session: otherwise the nest keeps a live credential nobody holds
/// on the machine's row until the user deletes the device. A
/// stop after which the slot **survives** must do the opposite: the same key
/// will be loaded again at the next sign-in as this account, and tombstoning
/// it would turn an ordinary switch back into a removed-from-account state
/// and a successor mint.
///
/// So the host names what it is doing; the shared stop does the rest. A host
/// unsure which it is should read what follows the stop: an erase of the
/// account's credential namespace is a sign-out, anything that keeps it is a
/// switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The all-accounts erase follows (`account-scoping.md` § Erasure follows
    /// scope): sign-out, and every reset that wipes the credential namespace.
    /// The enrollment is retired nest-side before the store closes.
    SignOut,
    /// The slot survives: an account switch, a factory-reset re-claim that
    /// keeps the identity, a nest-trust escalation that keeps the credentials,
    /// a superseded assembly. The machine stays enrolled.
    AccountSwitch,
}

/// One stop per handle, shaped by the reason: a sign-out retires the
/// enrollment on its way out, a switch leaves the machine enrolled. The one
/// stop every host runs — the native hosts' teardown and superseded arm
/// (which race for one runtime on a mid-assembly teardown, so both must retire
/// first or neither reliably does) and the browser tab's. Two retirements of
/// one key are harmless: the revoke is key-addressed and the second finds
/// nothing to clear.
pub async fn stop_one(store: &AccountStoreHandle, reason: StopReason) {
    match reason {
        StopReason::SignOut => {
            let retirement = store.shutdown_for_sign_out().await;
            tracing::info!(
                ?retirement,
                "[account-runtime] sign-out: the machine's enrollment retirement"
            );
        }
        StopReason::AccountSwitch => store.shutdown().await,
    }
}
