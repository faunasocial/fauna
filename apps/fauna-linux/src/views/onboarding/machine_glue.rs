//! GTK observer + factory for `OnboardingMachine`.
//!
//! The machine notifies on every state mutation via `OnboardingObserver::on_changed`.
//! This implementation forwards each notification to a GTK main-loop receiver
//! through an `async-channel`. The orchestrator (mod.rs) owns the receiver
//! end and runs `handle_change` on every tick to re-read snapshots and update
//! the visible page + active refresh closure.
//!
//! `glib::MainContext::channel` was removed in glib 0.20, so we use
//! `async-channel` (unbounded, MPMC-safe) with `glib::MainContext::default().spawn_local`.
use std::sync::Arc;

use async_channel::Sender;

use fauna_onboarding_machine::{OnboardingMachine, OnboardingObserver};

pub struct GtkObserver {
    pub tx: Sender<()>,
}

impl OnboardingObserver for GtkObserver {
    fn on_changed(&self) {
        // try_send so the mutator path never blocks; a full channel is a wake
        // already owed (`async_helper::snapshot_wake_channel`).
        let _ = self.tx.try_send(());
    }
}

/// Build a machine. The wizard is in-memory only; identity persistence
/// happens at the identity-confirmation view layer via
/// [`super::commit_confirmed_identity`] (and final all-three save at
/// complete-login time).
pub fn make_machine(tx: Sender<()>) -> Arc<OnboardingMachine> {
    let observer = Arc::new(GtkObserver { tx });
    // `new_with_persistence`, never bare `new`: the wizard writes the
    // pending-provision slot before it builds a box, so a quit or crash mid-run
    // resumes from the app alone (`onboarding.md` § 6 *The pending-provision
    // slot*). A machine built without a store provisions exactly as before and
    // silently loses that resume — which is why
    // `provision_slot_is_wired_on_every_production_app` pins this line.
    let machine = OnboardingMachine::new_with_persistence(
        observer,
        Arc::new(crate::account_registry().pending_provision_store()),
    );
    // linux renders the `trust_prompt` interstitial (`onboarding.md` § 3b-ter,
    // built 2026-08-14 after tui led it), so the machine routes the NAT step's
    // exits through the one-tap trust offer. This capability flag is the ONLY
    // thing that makes the step reachable — an app that leaves it undeclared
    // exits straight to `Done`, which is why the other apps' flows were
    // byte-for-byte unchanged until each trickle-down landed.
    //
    // ⚠ Declaring it and NOT rendering the page would strand the wizard on a
    // step with no stack child: the two land together, here and in
    // `mod.rs`'s `STEP_TRUST_PROMPT` registration.
    machine.set_renders_trust_prompt(true);
    // linux renders the `recovery_kit` offer and the `recovery_entry` restore
    // (`onboarding.md` § 1 Identity; tui led), so the machine routes a created
    // identity through the kit screen. Same pairing rule as the trust prompt:
    // the declaration and `mod.rs`'s `STEP_RECOVERY_KIT` registration land
    // together, or the wizard strands on a step with no stack child.
    machine.set_renders_recovery_kit(true);
    machine
}
