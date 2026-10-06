//! The linux app's one seam for **in-memory** actor-scoped state.
//!
//! `account-scoping.md` § The scoping taxonomy binds this: the switch/sign-out
//! isolation contract forbids account B rendering or modifying account A's local
//! state, and its in-memory corollary makes a live cache, a manager singleton or
//! a running timer account-scoped by class 1/4 exactly as its on-disk twin would
//! be. Linux never exits on an actor change — `main.rs::launch_authenticated`
//! builds a **fresh `FaunaClient`** in-process on all three re-entry paths (the
//! account switcher, the post-onboarding launch a sign-out lands on, and the
//! relaunch path) — so nothing here is dropped for us by the window teardown
//! that surrounds them.
//!
//! ── Why one list rather than a list per teardown site ────────────────────────
//!
//! Before this module the drop was **hand-listed at six sites in `main.rs`**
//! (sign-out, identity-invalid teardown, factory reset, account switch, and the
//! two test-agent paths), and the six had already drifted apart — which is the
//! failure mode, not an accident of maintenance:
//!
//!   * `content_policy`'s three thread-locals were on **none** of the six, so a
//!     switch kept the outgoing ward's guardian content floor and their pending
//!     Guardian Notify counts.
//!   * `feed::host` / `search::host` were cleared on the **e2e actor-switch path
//!     only**, so production's own account switch left both manager slots
//!     holding the outgoing actor's manager.
//!   * the four `start_*_poll` timers were latched by a process-global
//!     `AtomicBool` that no path ever reset, so after any actor change the
//!     incoming actor's ticks never armed and the outgoing actor's kept firing
//!     against a client that is no longer signed in.
//!
//! Web solved the same problem with a registration registry (`actorScope.ts`),
//! which works there because importing a module runs its `registerActorScopedReset`
//! call as a side effect. Rust has no module-init side effect, so a registry here
//! would still need a hand-written registration list — just relocated, and with a
//! *silent* failure mode when one is forgotten. So the shape that survives a cold
//! read is the opposite one: **one explicit function, statically greppable, that
//! every teardown site calls**. Adding actor-scoped state means adding one line to
//! [`reset_actor_scoped_state`] and nowhere else.
//!
//! ── The generation counter ───────────────────────────────────────────────────
//!
//! Dropping state is only half of an actor change; the other half is retiring the
//! **timers** that write it. A `glib::timeout_add_local` tick captures its
//! `Rc<FaunaClient>` and runs forever, so a poll armed by the outgoing actor
//! keeps calling the outgoing actor's client. Clearing the arm-latch alone would
//! be worse than leaving it: the incoming actor would arm a *second* tick beside
//! the first. So every actor change bumps a generation, each poll captures the
//! generation it was armed for, and a tick whose generation is stale returns
//! `ControlFlow::Break` — retiring itself and releasing the stale client — before
//! it does any work.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped by [`reset_actor_scoped_state`], i.e. once per actor change. Timers
/// armed under an older generation retire themselves on their next tick.
static ACTOR_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Claim the arm-slot for a once-per-actor periodic poll.
///
/// Returns the generation to capture in the tick when the caller should arm, and
/// `None` when this actor already armed it (the idempotence the four
/// `start_*_poll` callers rely on — every post-auth hook calls them).
pub fn claim_poll_slot(armed_for: &AtomicU64) -> Option<u64> {
    claim_poll_slot_in(&ACTOR_GENERATION, armed_for)
}

/// Whether a poll armed for `armed_generation` still belongs to the signed-in
/// actor. A tick that gets `false` must return `glib::ControlFlow::Break`
/// **before** touching its captured client.
pub fn poll_still_current(armed_generation: u64) -> bool {
    poll_still_current_in(&ACTOR_GENERATION, armed_generation)
}

/// The current actor generation — for a one-shot detached read (not a
/// recurring poll, so [`claim_poll_slot`]'s arm-latch semantics don't apply).
/// Capture this before spawning the read; check it against [`poll_still_current`]
/// before the landing writes anything actor-scoped
/// (`account-scoping.md` § The scoping taxonomy → the in-memory corollary).
pub fn current_generation() -> u64 {
    ACTOR_GENERATION.load(Ordering::SeqCst)
}

// The three primitives below take their generation counter by reference so the
// tests can drive a *local* one. Sharing the process-global across parallel
// tests would make them order-dependent on each other — the wall-clock-adjacent
// brittleness `testing.md` convention 14 exists to keep out of this repo.

/// `armed_for` stores `generation + 1` rather than the generation itself so that
/// its zero-initialised state means "never armed" and generation 0 is still a
/// real generation.
fn claim_poll_slot_in(generation_counter: &AtomicU64, armed_for: &AtomicU64) -> Option<u64> {
    let generation = generation_counter.load(Ordering::SeqCst);
    if armed_for.swap(generation + 1, Ordering::SeqCst) == generation + 1 {
        return None; // this actor already armed it
    }
    Some(generation)
}

fn poll_still_current_in(generation_counter: &AtomicU64, armed_generation: u64) -> bool {
    generation_counter.load(Ordering::SeqCst) == armed_generation
}

/// Retire the current generation. Split out from [`reset_actor_scoped_state`] so
/// the generation primitives are testable without a GTK main loop.
fn bump_actor_generation_in(generation_counter: &AtomicU64) {
    generation_counter.fetch_add(1, Ordering::SeqCst);
}

/// Drop every piece of in-memory actor-scoped state, then retire the outgoing
/// actor's timers. **The** canonical list — every teardown path calls this and
/// hand-lists nothing of its own.
///
/// Ordering is part of the contract: state is dropped **before** the generation
/// bump, so a tick that wakes between the two finds its own generation still
/// current and merely operates on already-cleared state, rather than reading a
/// half-dropped mix.
///
/// Callers that additionally tear down e2e-only state (the test-agent paths'
/// `clear_for_test`, which is absent from a release build) keep those calls at
/// their own site; everything a *production* actor change must drop belongs here.
///
/// `reason` is the one fact the account runtime's stop cannot infer: whether
/// the credential erase follows (`StopReason::SignOut` — the machine's
/// enrollment is retired nest-side on the way out) or the slot survives
/// (`StopReason::AccountSwitch` — the machine stays enrolled). Every caller
/// states it; the shared stop owns why it matters
/// (`fauna_client_account_runtime::StopReason`).
///
/// `then` is the caller's continuation, and the one contract here that is
/// about TIME: everything above runs before this returns, but the account
/// runtime's stop does not — the GTK thread is handed back while it runs, and
/// `then` runs on the main loop once it has finished (inline when there was
/// nothing to stop). So a caller puts inside `then` everything that must follow
/// the stop: the erase above all, which must never meet a store still open
/// (`apps/account-scoping.md` § Erasure follows scope), and anything that shuts
/// down the client runtime the stop is driven on.
pub fn reset_actor_scoped_state(
    reason: fauna_client_account_runtime::StopReason,
    then: impl FnOnce() + 'static,
) {
    crate::content_policy::clear_for_identity_change();
    // …but the region source is the DEVICE's, not the departing identity's:
    // re-arm it on the fresh engine, and forget only the session's refresh
    // clock (tui's `App::reset_for_identity_change` does the same).
    crate::region::clear_session();
    crate::region::apply_rule_sets();
    crate::critical_alerts::clear_for_identity_change();
    crate::screen_lock::clear_for_identity_change();
    // The outgoing ward's own pending asks — a stale one would paint "asked —
    // waiting for your guardian" on the incoming account's contacts/profile/
    // bridge surfaces until its first status read lands.
    crate::ward_asks::clear_for_identity_change();
    // The Folders page's co-present offline-share panel — window-owned, not
    // process-global, so the reset goes through the live window's own
    // registration (`crate::offline_share::register_reset_hook`) rather than
    // a bare clear call (`p2p.md` § Cross-user shared-set transfer → *Built —
    // the tui app leg*; mirrors tui's `sign_out`,
    // `apps/fauna-tui/src/session.rs`). Runs HERE, synchronously, because
    // sign-out stops the message pump around the same point
    // (`settings::trigger_pump_shutdown`) — a reset queued as a `DataMessage`
    // could be left unhandled.
    #[cfg(feature = "p2p-share")]
    crate::offline_share::reset_for_actor_change();
    // Both manager slots are process-wide accessors with no actor key, so a
    // reader between teardown and the next `init` serializes the previous
    // identity's posts / search index.
    crate::feed::host::clear();
    crate::search::host::clear();
    // The per-key image caches can hold a gated post's image OPENED under the
    // outgoing reader's key, or a remote image only that reader revealed. The
    // feed's are already keyed to the manager cleared above, so the next reader
    // could not read them; dropping them here also stops them outliving the
    // session in memory, and retires the epoch the conversations page's cache
    // is keyed to (that page's manager is a process singleton).
    crate::media_loads::clear_for_identity_change();
    // What the outgoing identity's aftermath pass reported: each sign-in runs
    // its own, so the incoming identity must not read the outgoing one's lines.
    crate::settings::recovery_kit::clear_session_aftermath();
    // The web-publish origin cache the Settings → Web page shares with the
    // feed ⋯-menu's copy-link verbs — undropped until 2026-09-21 left an
    // account switch reading the outgoing actor's origin, paywall link
    // included, until a fresh visit to Settings → Web overwrote it.
    crate::settings::web::clear_for_actor_change();
    // The outgoing window's gated controls. Its widget tree outlives
    // `destroy()`, so without the retire every later link flip would still
    // re-decide them (`offline_gate::retire_outgoing_window`).
    crate::offline_gate::retire_outgoing_window();
    bump_actor_generation_in(&ACTOR_GENERATION);
    // LAST: the W3 (account-data-plane.md § Workstreams) account-store runtime
    // this app hosts. Actor-scoped by class 1: it holds an open store for one
    // account and pumps the plane AS that account, so a handle surviving an
    // actor change is the outgoing account still being written by this
    // process. The stop is deterministic (`shutdown()` drains the in-flight
    // pass) and runs OFF this thread; `then` — everything the caller must not
    // do until the account has stopped (its writers-down, its erase, its next
    // window) — runs on the main loop once it has
    // (`account_runtime::teardown`).
    crate::account_runtime::teardown(reason, then);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_slot_is_claimed_once_per_actor() {
        let generations = AtomicU64::new(0);
        let armed = AtomicU64::new(0);
        let generation = claim_poll_slot_in(&generations, &armed).expect("the first claim arms");
        // Every post-auth hook calls the start_* helpers; only the first arms,
        // or the actor would run two ticks of the same poll.
        assert!(claim_poll_slot_in(&generations, &armed).is_none());
        assert!(claim_poll_slot_in(&generations, &armed).is_none());
        assert!(poll_still_current_in(&generations, generation));
    }

    #[test]
    fn an_actor_change_retires_the_outgoing_poll_and_lets_the_incoming_actor_arm() {
        let generations = AtomicU64::new(0);
        let armed = AtomicU64::new(0);
        let outgoing = claim_poll_slot_in(&generations, &armed).expect("the outgoing actor arms");

        bump_actor_generation_in(&generations);

        // 1. The outgoing actor's tick must retire itself rather than keep
        //    firing against a client that is no longer signed in — the half a
        //    bare latch-reset would miss, leaving two ticks running at once.
        assert!(
            !poll_still_current_in(&generations, outgoing),
            "a poll armed by the outgoing actor still reports itself current"
        );
        // 2. ...and the incoming actor must be able to arm its own tick, which
        //    the never-reset `AtomicBool` latch forbade.
        let incoming = claim_poll_slot_in(&generations, &armed)
            .expect("the incoming actor must arm its own tick");
        assert_ne!(incoming, outgoing);
        assert!(poll_still_current_in(&generations, incoming));
    }
}
