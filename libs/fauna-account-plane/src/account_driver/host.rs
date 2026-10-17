//! The **install/teardown lifecycle** every host of the account driver
//! repeats around its assembly — the slot the started handle lives in, the
//! generation guard that makes spawning the assembly safe, and the in-flight
//! seam a teardown waits on (`account-client-lifecycle.md` § The client-side
//! lifecycle → *The trigger fired*, ruling (4)).
//!
//! Here rather than in the native assembly crate
//! (`fauna-client-account-runtime`, which re-exports every item at its old
//! path) because the browser tab hosts the driver too, and an account switch
//! lands mid-assembly there exactly as on a native host: a tab whose start
//! had no claim to lose installed the departing account's runtime after the
//! switch, or let the departing account's stop take the incoming one's.
//! Pure state transitions over `tokio::sync` — no runtime, no clock — so the
//! one shape builds for wasm32 as well.

use super::{AccountStoreHandle, StopReason, stop_one};

/// The **assembly side** of the in-flight seam: publishes the started runtime
/// the instant the host's assembly settles, so a teardown can reach a store
/// that is open but not yet installed.
///
/// Dropping one without calling either method reads as [`Self::failed`] — the
/// assembly task died without answering, and there is nothing to stop.
pub struct AssemblySettle(tokio::sync::oneshot::Sender<Option<AccountStoreHandle>>);

impl AssemblySettle {
    /// The assembly started a runtime. Publishes a clone for the teardown;
    /// a cheap no-op when no teardown is waiting.
    pub fn started(self, handle: &AccountStoreHandle) {
        let _ = self.0.send(Some(handle.clone()));
    }

    /// The assembly failed. Resolves the wait **at once**, so a failed
    /// assembly never spends the teardown budget.
    pub fn failed(self) {
        let _ = self.0.send(None);
    }
}

/// Hand-written rather than derived: [`InstallClaim`] is `Debug` and carries
/// one of these, and neither the channel nor an `AccountStoreHandle` owes a
/// `Debug` for that to hold.
impl std::fmt::Debug for AssemblySettle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AssemblySettle")
    }
}

/// The **teardown side**: an assembly that may still be in flight.
///
/// # Why a handle slot is not enough, and this is not belt-and-braces
///
/// `AccountStoreRuntime::start` opens `account-store.db` **on its own OS
/// thread before returning**, and the started handle then has to reach the
/// app — through [`AccountRuntimeHost::finish`], or across a UI queue. Between
/// those two moments a live connection holds the database while the host's own
/// handle slot honestly reads `None`, and a teardown that only *checks* that
/// slot stops nothing and erases anyway. Measured `--app tui` on Windows,
/// 2026-09-04: sign-out landed 475 ms after the assembly minted this machine's
/// writer key, and the erase failed with `os error 32` on a root nothing had
/// torn down (`apps/account-scoping.md` § Erasure follows scope, the ⚠ *A
/// store that is still ASSEMBLING has no handle to release* note).
///
/// POSIX `unlink` removes an open file, which is why this hid on linux for as
/// long as it existed — so closing it is a **convergence** obligation on every
/// host, not a windows patch.
pub struct PendingAssembly(tokio::sync::oneshot::Receiver<Option<AccountStoreHandle>>);

impl PendingAssembly {
    /// Wait for the assembly to settle. `None` means there is nothing to stop
    /// — the assembly failed, or its task died without answering.
    pub async fn settled(self) -> Option<AccountStoreHandle> {
        self.0.await.ok().flatten()
    }
}

/// Mint the in-flight seam. The host — or the app itself, for a deliberately
/// host-less one like tui (`account-data-plane.md` § The account store → *The
/// client-side lifecycle*, the ⚠ *tui is a deliberate NON-consumer* note) —
/// keeps the [`PendingAssembly`]; the assembly task carries the
/// [`AssemblySettle`].
pub fn assembly_channel() -> (AssemblySettle, PendingAssembly) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (AssemblySettle(tx), PendingAssembly(rx))
}

/// The **install/teardown lifecycle** every seed-holding host repeats around
/// the host's assembly — the slot the started handle lives in, the
/// generation guard that makes spawning the assembly safe, and the in-flight
/// seam a teardown waits on.
///
/// # Why this is shared and not four copies of twenty lines
///
/// Assembly does real I/O, so every host spawns it — and a sign-out or account
/// switch can therefore land *while it runs*. What must happen then is not
/// obvious and is silent when wrong: the freshly-started runtime has to be shut
/// down rather than installed, because installing it means **the signed-out
/// account is still being served** (the shape `apps/sync-agent.md` § Control
/// plane split forbids). linux and the `fauna-ffi` seat each wrote that guard
/// by hand and the second was a near-verbatim copy of the first, comments
/// included — which is exactly the drift this crate's module docs predict for
/// the eleven params, applied to the lifecycle instead. windows, macOS and iOS
/// are about to become the third, fourth and fifth.
///
/// # The payload, and why it is captured at CLAIM rather than at install
///
/// A host often needs one thing *beside* the handle, needed at teardown, and it
/// differs per host: linux stores the tokio [`Handle`] its shutdown must be
/// driven on, because its teardown is called from the GTK main thread and must
/// never build a runtime there. Hosts with nothing to carry use the default
/// `()`.
///
/// It is handed to [`Self::begin`], not to [`Self::finish`], and that moved
/// (2026-09-06) for a reason the earlier shape could not express: **the
/// teardown that most needs the payload is the one that finds an EMPTY slot.**
/// A sign-out landing mid-assembly has to wait for a store that is open but not
/// yet installed — and on linux that wait must be driven on the runtime handle,
/// which under install-time capture does not exist yet. Claim time is when the
/// host first knows both.
///
/// # What it deliberately does NOT do
///
/// [`Self::take`] returns what to stop rather than stopping it, because *how*
/// to await that shutdown is the host's own constraint — linux spawns it and
/// runs the synchronous erase that follows as a GTK main-loop continuation of
/// its completion (that thread cannot await, and must not be held for the
/// stop's budget), the FFI seat awaits it (its caller is already async and a
/// sign-out should not race the drain). Deciding that here would force one of
/// them into the wrong shape. the host's stop is the shared *body* of
/// that stop; the choice of how to drive it stays at the call site.
///
/// [`Handle`]: https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html
pub struct AccountRuntimeHost<T = ()> {
    state: std::sync::Mutex<HostState<AccountStoreHandle, T>>,
}

/// The host's whole mutable state, behind **one** lock.
///
/// # Why one lock, and not a mutex per field
///
/// An install is three decisions — is my claim still current, remove the
/// in-flight registration, write the slot — and a teardown is two: advance the
/// generation, take whatever is there. Under a lock per field, a teardown could
/// land *between* an install's second and third decision and see both halves
/// empty: the registration just removed, the slot not yet written. It then
/// reported **nothing to stop**, the erase ran, and the install completed
/// immediately afterwards — leaving the signed-out account's runtime installed
/// and pumping against a store being erased (the `os error 32` shape on
/// windows, deleted-inode writes on linux). No `.await` sat in that gap, which
/// is what made it look safe; the teardown runs on a different thread (the GTK
/// main thread on linux, the FFI caller's thread in the `fauna-ffi` seat), so
/// it never needed one.
///
/// So both sequences are single transitions over this struct, and the
/// in-between state is unrepresentable rather than merely unlikely: while a
/// claim is current, this state always holds either the registration or the
/// slot for a teardown to find.
///
/// Generic over the handle type so the transitions can be tested without a
/// started [`AccountStoreHandle`], whose fields are private to its own crate
/// and which no test can construct.
struct HostState<H, T> {
    /// The installed runtime and this host's teardown payload.
    slot: Option<(H, T)>,
    /// The assembly claimed but not yet settled, and the payload its teardown
    /// will need. Present between [`AccountRuntimeHost::begin`] and whichever
    /// comes first: [`AccountRuntimeHost::finish`] or a teardown's
    /// [`AccountRuntimeHost::take`].
    pending: Option<(PendingAssembly, T)>,
    /// The newest install claim's number. `begin` and `take` both advance it;
    /// [`AccountRuntimeHost::finish`] installs only under the newest.
    generation: u64,
    /// The newest generation a **sign-out** teardown invalidated. Every claim
    /// at or below it belongs to a sign-in whose credential slot that sign-out
    /// erased, so an assembly of theirs that settles late is stopped the
    /// sign-out way ([`Self::settle_install`]). `0` — below every claim — until
    /// the first sign-out.
    signed_out_through: u64,
}

/// The two halves one teardown found — the settled runtime with its payload,
/// and the assembly still in flight with its own. [`AccountRuntimeHost::take`]
/// assembles the public [`RuntimeTeardown`] around them.
type TornDown<H, T> = (Option<(H, T)>, Option<(PendingAssembly, T)>);

/// What [`HostState::settle_install`] decided, for the caller to carry out
/// **after** releasing the lock — shutting a runtime down is async, and nothing
/// may `.await` under a `std::sync::Mutex`.
enum InstallStep<H, T> {
    /// The handle is installed. Shut down whatever it replaced, if anything.
    Installed(Option<(H, T)>),
    /// A teardown or a newer install landed first. Shut this handle down —
    /// **the way whatever superseded it would have**: a sign-out's erase took
    /// this runtime's credential slot, so its enrollment is retired on the way
    /// out; anything else left the slot in place and the machine stays
    /// enrolled ([`StopReason`]).
    Superseded(H, StopReason),
}

impl<H, T> HostState<H, T> {
    const fn empty() -> Self {
        Self {
            slot: None,
            pending: None,
            generation: 0,
            signed_out_through: 0,
        }
    }

    /// How a runtime assembled under `claim_generation` must be stopped now
    /// that it will not be installed.
    ///
    /// ⚠ **The superseded arm is a stop like any other, and a stop has a
    /// reason.** The prologue enrolls the machine *before* the assembly
    /// settles, so a sign-out landing mid-assembly supersedes a runtime that
    /// already holds a registered enrollment. Stopped plainly, that enrollment
    /// outlives the key the erase is about to destroy — a live grant nobody
    /// holds, per such sign-out (`sync-agent-credentials.md` § Implementation
    /// status today). The waiting teardown cannot be relied on
    /// for the retirement: this arm's own shutdown is queued the instant the
    /// handle is published, ahead of anything the teardown sends after waking.
    fn superseded_stop_reason(&self, claim_generation: u64) -> StopReason {
        if claim_generation <= self.signed_out_through {
            StopReason::SignOut
        } else {
            StopReason::AccountSwitch
        }
    }

    /// Claim the next install: advance the generation and register the
    /// assembly as in flight, replacing any earlier registration (whose
    /// generation has just moved on, so nothing will be installed under it).
    fn begin(&mut self, pending: PendingAssembly, payload: T) -> u64 {
        self.generation += 1;
        self.pending = Some((pending, payload));
        self.generation
    }

    /// The **whole install decision**, as one transition: the claim check, the
    /// registration's removal and the slot write — see the type docs for why
    /// they may not be three.
    fn settle_install(&mut self, claim_generation: u64, handle: H) -> InstallStep<H, T> {
        let reason = self.superseded_stop_reason(claim_generation);
        if self.generation != claim_generation {
            return InstallStep::Superseded(handle, reason);
        }
        // Unreachable while the claim is current: only `begin` and `take` clear
        // the registration, and both advance the generation in this same
        // transition. Kept as the fail-closed arm — refusing to install beats
        // inventing a payload, because an un-torn-down runtime is the one
        // outcome this type exists to prevent.
        let Some((_, payload)) = self.pending.take() else {
            return InstallStep::Superseded(handle, reason);
        };
        InstallStep::Installed(self.slot.replace((handle, payload)))
    }

    /// Everything a teardown must stop, and the generation advanced so an
    /// assembly still in flight discards itself — one transition, for the same
    /// reason [`Self::settle_install`] is one. A sign-out additionally marks
    /// every claim so far as signed out, **before** the generation moves on, so
    /// the sign-in that follows is not caught by it.
    fn tear_down(&mut self, reason: StopReason) -> TornDown<H, T> {
        if reason == StopReason::SignOut {
            self.signed_out_through = self.generation;
        }
        self.generation += 1;
        (self.slot.take(), self.pending.take())
    }
}

/// A claim on one install attempt, taken by [`AccountRuntimeHost::begin`]
/// **before** the assembly is spawned and presented back to
/// [`AccountRuntimeHost::finish`] after it returns.
///
/// Not `Copy` or constructible outside this crate on purpose: a host cannot
/// finish an install it never began, nor reuse one claim for two assemblies.
///
/// It carries the [`AssemblySettle`] half of the in-flight seam, so an
/// assembly task that **drops** its claim — the failed-assembly path, where
/// `finish` is never called — resolves a waiting teardown at once instead of
/// spending its whole budget.
#[derive(Debug)]
pub struct InstallClaim(u64, Option<AssemblySettle>);

/// What [`AccountRuntimeHost::finish`] did — worth logging, never worth
/// branching on: both arms leave the host in a correct state.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The handle is now this host's live runtime. Any previous one was shut
    /// down first.
    Installed,
    /// A teardown or a newer install landed while this assembly ran, so the
    /// fresh runtime was shut down instead of installed.
    Superseded,
}

impl<T> AccountRuntimeHost<T> {
    /// An empty host. `const` so it can be a `static` — every consumer wants
    /// exactly one per process, since the engine election is per store
    /// and a second host in one process would contend with the first.
    pub const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(HostState::empty()),
        }
    }

    /// The host's state, through a poisoned lock as well as a healthy one.
    ///
    /// Nothing under this lock can panic — the transitions are moves between
    /// `Option`s — so a poisoned lock means a panic *elsewhere* left the state
    /// intact, and the previous shape's "on poison, skip the operation" would
    /// turn that into the very outcome this type prevents: a teardown that
    /// takes nothing while a runtime stays installed.
    fn state(&self) -> std::sync::MutexGuard<'_, HostState<AccountStoreHandle, T>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claim the next install and register the assembly as in flight. Call
    /// this **on the login path, before spawning the assembly** — claiming
    /// after the spawn would leave a window in which a teardown bumps nothing
    /// and the assembly installs anyway.
    ///
    /// `payload` is what this host's teardown will need beside the handle; it
    /// is captured here rather than at [`Self::finish`] because a teardown
    /// landing mid-assembly needs it precisely when nothing is installed yet
    /// (see the type docs).
    pub fn begin(&self, payload: T) -> InstallClaim {
        let (settle, pending) = assembly_channel();
        // A previous claim's registration is replaced, not merged: its
        // generation has just moved on, so `finish` will discard whatever it
        // produces and nothing is left for a teardown to wait on. Dropping its
        // receiver also resolves that assembly's own settle send as "nobody
        // waiting", which is true.
        let generation = self.state().begin(pending, payload);
        InstallClaim(generation, Some(settle))
    }

    /// Install `handle` under `claim`, or shut it down if the claim is stale.
    ///
    /// Shutting down the superseded runtime — and the previous one it replaces
    /// — happens here rather than at the call site precisely because forgetting
    /// either is invisible: the account keeps pumping and nothing reports it.
    pub async fn finish(
        &self,
        mut claim: InstallClaim,
        handle: AccountStoreHandle,
    ) -> InstallOutcome {
        // Publish to a waiting teardown FIRST, before either arm below. The
        // superseded arm's own `shutdown()` is what usually stops this runtime,
        // and `shutdown` is idempotent — so a teardown that also reaches it
        // stops it once and both report success. What the publish actually buys
        // is the *wait*: without it the teardown returns while this store is
        // still open, which on windows is the `os error 32` erase failure.
        if let Some(settle) = claim.1.take() {
            settle.started(&handle);
        }
        // ONE transition, not three: the claim check, the registration's
        // removal and the slot write happen under a single lock, so a teardown
        // cannot land between them and come away with "nothing to stop" while
        // this install completes behind it (`HostState`'s docs). The shutdowns
        // below run after the guard is dropped — nothing may `.await` under a
        // `std::sync::Mutex`.
        let step = self.state().settle_install(claim.0, handle);
        match step {
            InstallStep::Superseded(handle, reason) => {
                // The reason is the superseding teardown's, not a default: see
                // `HostState::superseded_stop_reason`.
                stop_one(&handle, reason).await;
                InstallOutcome::Superseded
            }
            InstallStep::Installed(previous) => {
                if let Some((previous, _)) = previous {
                    previous.shutdown().await;
                }
                InstallOutcome::Installed
            }
        }
    }

    /// The live handle, if an assembly has landed. `None` before it completes,
    /// after a teardown, or whenever it failed.
    pub fn handle(&self) -> Option<AccountStoreHandle> {
        self.state().slot.as_ref().map(|(h, _)| h.clone())
    }

    /// Read the live handle together with its payload, under the lock — for a
    /// host whose readers need what it carries beside the handle (the browser
    /// tab: the account the runtime serves, its store-change level). `None`
    /// exactly when [`Self::handle`] is. `read` must not block or re-enter
    /// the host.
    pub fn with_installed<R>(&self, read: impl FnOnce(&AccountStoreHandle, &T) -> R) -> Option<R> {
        self.state().slot.as_ref().map(|(h, t)| read(h, t))
    }

    /// Take **everything this host has to stop** for a teardown, and advance
    /// the generation so an assembly still in flight discards itself instead of
    /// installing under the next account.
    ///
    /// **The caller must stop what it gets back** — feed it to
    /// the host's stop and decide whether to await or spawn that; see
    /// the type docs for why the decision is not made here. The generation
    /// moves even when the slot is empty, which is the case that matters: a
    /// sign-out landing *during* the very first assembly has nothing to take
    /// and everything to prevent — and, since 2026-09-06, an in-flight
    /// assembly to WAIT for rather than merely to invalidate.
    ///
    /// `reason` is the same one the caller is about to hand
    /// the host's stop. It is recorded here because the in-flight
    /// assembly's own [`Self::finish`] also stops that runtime, usually first,
    /// and must stop it the same way — a sign-out's retirement is otherwise
    /// lost to a plain shutdown queued ahead of it.
    pub fn take(&self, reason: StopReason) -> RuntimeTeardown<T> {
        // One transition, for [`Self::finish`]'s reason: an install that is
        // half-applied is not a state this can observe.
        let (settled, pending) = self.state().tear_down(reason);
        // The payload comes from whichever half exists. Both cannot: `finish`
        // clears the registration as it installs, and `begin` replaces it — so
        // a host is either assembling or settled, never both for one account.
        let (settled, settled_payload) = match settled {
            Some((handle, payload)) => (Some(handle), Some(payload)),
            None => (None, None),
        };
        let (pending, pending_payload) = match pending {
            Some((pending, payload)) => (Some(pending), Some(payload)),
            None => (None, None),
        };
        RuntimeTeardown {
            settled,
            pending,
            payload: settled_payload.or(pending_payload),
        }
    }
}

/// What one teardown found: the settled runtime, the assembly still in flight,
/// and the payload whichever of them registered.
///
/// A struct rather than a tuple because the interesting case is the one where
/// `settled` is `None` and `pending` is not — the mid-assembly sign-out — and a
/// tuple's second element is exactly the thing a call site forgets.
pub struct RuntimeTeardown<T> {
    /// The installed runtime, if the assembly had landed.
    pub settled: Option<AccountStoreHandle>,
    /// The assembly claimed but not yet settled — a store that may already be
    /// **open** on its own OS thread while `settled` reads `None`. Hand both to
    /// the host's stop.
    pub pending: Option<PendingAssembly>,
    /// This host's teardown payload — the tokio [`Handle`] on linux, `()`
    /// where there is nothing to carry. `None` only when there was nothing to
    /// stop at all.
    ///
    /// [`Handle`]: https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html
    pub payload: Option<T>,
}

impl<T> RuntimeTeardown<T> {
    /// Is there anything at all to stop? `false` is the common teardown — a
    /// sign-out with no account runtime ever assembled.
    pub fn is_empty(&self) -> bool {
        self.settled.is_none() && self.pending.is_none()
    }
}

impl<T> Default for AccountRuntimeHost<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the install/teardown transitions ────────────────────────────────
    //
    // These drive `HostState` rather than the host, because the install half
    // needs a handle and a started `AccountStoreHandle` cannot be constructed
    // in a test (its fields are private to `fauna-sync-engine`). The state is
    // generic over the handle exactly so this half is reachable; the host is a
    // lock around it and one `.await` per shutdown.

    /// A stand-in for the started runtime. The transitions never call anything
    /// on it — they only move it between the registration, the slot and the
    /// teardown, which is the whole of what the lock protects.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeHandle(u8);

    fn claimed(state: &mut HostState<FakeHandle, ()>) -> (u64, AssemblySettle) {
        let (settle, pending) = assembly_channel();
        (state.begin(pending, ()), settle)
    }

    /// ⚠ **The state a teardown must never observe: nothing to stop while an
    /// install is still going to happen.**
    ///
    /// The defect this pins: with a lock per field, an
    /// install was three
    /// decisions — check the claim, remove the registration, write the slot —
    /// and a teardown landing between the last two found the registration gone
    /// and the slot empty. It reported `NothingToStop`, the erase ran, and the
    /// install completed behind it: the signed-out account's runtime installed
    /// and pumping against a store being erased. No `.await` sat in that gap,
    /// which is what made it read as safe; the teardown runs on another thread.
    ///
    /// The fix makes the install ONE transition, so the gap state is
    /// unrepresentable rather than unlikely. This walks the protocol and
    /// asserts that after every step, while the claim is current, the state
    /// still holds something for a teardown to take.
    #[test]
    fn a_current_claim_always_leaves_a_teardown_something_to_take() {
        let mut state = HostState::<FakeHandle, ()>::empty();
        let has_something_to_stop =
            |st: &HostState<FakeHandle, ()>| st.slot.is_some() || st.pending.is_some();

        let (claim, _settle) = claimed(&mut state);
        assert!(
            has_something_to_stop(&state),
            "the registration is what a mid-assembly teardown waits on"
        );

        let step = state.settle_install(claim, FakeHandle(1));
        assert!(
            matches!(step, InstallStep::Installed(None)),
            "the first install replaces nothing"
        );
        assert!(
            has_something_to_stop(&state),
            "the install moved the account from the registration to the slot in ONE \
             transition — a teardown sees one or the other, never neither"
        );
        assert!(
            state.pending.is_none() && state.slot.is_some(),
            "and it is the slot's turn now: {:?}",
            state.slot.map(|(h, ())| h)
        );
    }

    /// The two orders a teardown and an install can genuinely land in — and
    /// neither leaves a runtime installed behind a teardown that took nothing.
    #[test]
    fn a_teardown_either_takes_the_runtime_or_stops_it_being_installed() {
        // Teardown first: the claim is stale, so the assembly's handle is
        // handed back to be shut down and nothing is installed.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let (settled, pending) = state.tear_down(StopReason::AccountSwitch);
        assert!(settled.is_none(), "nothing had been installed yet");
        assert!(pending.is_some(), "but the assembly was in flight");
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(FakeHandle(1), _)
            ),
            "the signed-out account's runtime must be shut down, not installed"
        );
        assert!(state.slot.is_none(), "and it must not be in the slot");

        // Install first: the teardown takes the installed runtime, so the stop
        // has something to wait on and nothing stays behind.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.settle_install(claim, FakeHandle(2));
        let (settled, pending) = state.tear_down(StopReason::AccountSwitch);
        assert_eq!(
            settled.map(|(h, ())| h),
            Some(FakeHandle(2)),
            "the teardown takes the runtime the install landed"
        );
        assert!(pending.is_none(), "the registration went when it installed");
        assert!(state.slot.is_none());
    }

    /// A live claim survives to its install, and a second `begin` invalidates
    /// the first — the account-switch case, where the older assembly must
    /// discard its runtime rather than install it under the newer login.
    ///
    /// Asserted through the install step itself rather than a predicate beside
    /// it: the check is now part of the one transition, and a mirror of it in
    /// the host could drift from the one the install actually gates on.
    #[test]
    fn a_newer_install_invalidates_the_older_claim() {
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (first, _first_settle) = claimed(&mut state);
        let (second, _second_settle) = claimed(&mut state);

        assert!(
            matches!(
                state.settle_install(first, FakeHandle(1)),
                InstallStep::Superseded(..)
            ),
            "the older assembly must shut its runtime down, not install it under the \
             newer login"
        );
        assert!(
            matches!(
                state.settle_install(second, FakeHandle(2)),
                InstallStep::Installed(_)
            ),
            "the newest claim is the one that installs"
        );
    }

    /// ⚠ **A runtime superseded by a SIGN-OUT is stopped the sign-out way** —
    /// its enrollment retired — and one superseded by anything else is not.
    ///
    /// The defect this pins, measured `--app linux` 2026-09-20: the prologue
    /// registers the machine's enrollment *before* the assembly settles, so a
    /// sign-out landing mid-assembly supersedes a runtime that already holds a
    /// registered grant. The superseded arm shut it down plainly, and that
    /// shutdown is queued the instant the handle is published — ahead of the
    /// waiting teardown's retirement, which then read `account runtime is shut
    /// down`. One stranded `sync_devices` row per such sign-out, to the tier's
    /// device cap.
    #[test]
    fn a_sign_out_superseding_an_assembly_has_its_runtime_retire_on_the_way_out() {
        // The sign-out lands mid-assembly: the late install must retire.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(FakeHandle(1), StopReason::SignOut)
            ),
            "the erase that follows a sign-out destroys this runtime's key, so the \
             enrollment it registered must be retired before it stops"
        );

        // ...and STILL must when the next sign-in has claimed before the old
        // assembly settles: the slot it enrolled under is just as erased.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (old, _old_settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        let (new, _new_settle) = claimed(&mut state);
        assert!(matches!(
            state.settle_install(old, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::SignOut)
        ));
        assert!(
            matches!(
                state.settle_install(new, FakeHandle(2)),
                InstallStep::Installed(None)
            ),
            "the sign-in after the sign-out is not caught by it"
        );
        // Nor is that sign-in's own assembly, when a switch supersedes it: its
        // slot postdates the erase and survives the switch.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let _ = claimed(&mut state);
        let _ = state.tear_down(StopReason::SignOut);
        let (after, _after_settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert!(
            matches!(
                state.settle_install(after, FakeHandle(3)),
                InstallStep::Superseded(_, StopReason::AccountSwitch)
            ),
            "a sign-out marks the claims BEFORE it, never the sign-in after it"
        );

        // A switch keeps the slot: retiring here would turn the next sign-in as
        // this account into a removed-from-account state.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert!(matches!(
            state.settle_install(claim, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::AccountSwitch)
        ));

        // So does a newer login replacing an older assembly with no teardown.
        let mut state = HostState::<FakeHandle, ()>::empty();
        let (first, _first_settle) = claimed(&mut state);
        let (_second, _second_settle) = claimed(&mut state);
        assert!(matches!(
            state.settle_install(first, FakeHandle(1)),
            InstallStep::Superseded(_, StopReason::AccountSwitch)
        ));
    }

    /// ⚠ **A teardown advances the generation even with an EMPTY slot**, and
    /// that is the case the guard exists for.
    ///
    /// A sign-out landing *during the very first assembly* has no handle to
    /// take — so an implementation that only bumped when it took something
    /// would leave this claim current, and the assembly would install the
    /// signed-out account's runtime seconds later, with the app showing a
    /// logged-out UI over a live pump. There is no louder failure downstream:
    /// the runtime is healthy, it is just serving an account nobody is signed
    /// in to.
    #[test]
    fn a_teardown_invalidates_an_in_flight_claim_even_with_nothing_installed() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let _claim = host.begin(());
        assert!(
            host.take(StopReason::AccountSwitch).settled.is_none(),
            "nothing installed yet"
        );

        let mut state = HostState::<FakeHandle, ()>::empty();
        let (claim, _settle) = claimed(&mut state);
        let _ = state.tear_down(StopReason::AccountSwitch);
        assert_ne!(
            state.generation, claim,
            "the teardown must advance the generation itself — asserted here rather \
             than only through the install below, which would refuse this claim \
             anyway on the fail-closed arm (its registration is gone) and so would \
             pass even if the generation had never moved"
        );
        assert!(
            matches!(
                state.settle_install(claim, FakeHandle(1)),
                InstallStep::Superseded(..)
            ),
            "a sign-out during the first assembly must still supersede it"
        );
    }

    /// ⚠ **The same teardown must also come away with something to WAIT on**,
    /// which invalidation alone does not give it.
    ///
    /// Invalidating the claim stops the signed-out account from being *served*;
    /// it does nothing about the store the assembly already has **open on its
    /// own OS thread**, and the erase that follows a sign-out is synchronous.
    /// That gap is the measured `os error 32` (`PendingAssembly`'s docs), so
    /// this pins the half the generation guard structurally cannot cover.
    #[test]
    fn a_teardown_during_the_first_assembly_comes_away_with_the_in_flight_half() {
        let host: AccountRuntimeHost<u8> = AccountRuntimeHost::new();
        let _claim = host.begin(7);
        let teardown = host.take(StopReason::AccountSwitch);
        assert!(teardown.settled.is_none(), "nothing installed yet");
        assert!(
            teardown.pending.is_some(),
            "a sign-out mid-assembly must be able to wait for the store the \
             assembly already opened — an empty slot is not 'nothing to stop'"
        );
        assert!(!teardown.is_empty());
        assert_eq!(
            teardown.payload,
            Some(7),
            "the payload a teardown needs most is the one whose slot is empty: \
             on linux it is the runtime handle the wait is driven on"
        );
    }

    /// A second teardown finds the seam emptied — the wait is handed over once,
    /// not re-served to whoever asks next.
    #[test]
    fn the_in_flight_half_is_handed_over_exactly_once() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        let _claim = host.begin(());
        assert!(host.take(StopReason::AccountSwitch).pending.is_some());
        let second = host.take(StopReason::AccountSwitch);
        assert!(second.pending.is_none());
        assert!(second.is_empty());
    }

    /// An untouched host serves no handle, and a teardown on one is a no-op
    /// rather than a panic — every host's teardown runs on paths where no
    /// runtime was ever assembled (a failed assembly, a sign-out before login
    /// completed).
    #[test]
    fn an_empty_host_serves_nothing_and_tears_down_cleanly() {
        let host: AccountRuntimeHost = AccountRuntimeHost::new();
        assert!(host.handle().is_none());
        assert!(host.take(StopReason::AccountSwitch).is_empty());
        assert!(host.handle().is_none());
    }

    /// The code of one `AccountRuntimeHost` method, comments stripped: from its
    /// signature to the first closing brace at the method's own indentation.
    fn method_code(signature: &str) -> String {
        let source = include_str!("host.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("split is non-empty");
        let start = production
            .find(signature)
            .unwrap_or_else(|| panic!("`{signature}` is gone from host.rs — update this pin"));
        let body = &production[start..];
        let end = body
            .find("\n    }\n")
            .expect("a method ends at its own brace");
        body[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// ⚠ **The one-transition property, pinned by its source shape**.
    ///
    /// The tests above assert the *consequences* of `HostState`'s transitions,
    /// single-threaded; none sees how the host takes the lock around them.
    /// Splitting `finish` back into two acquisitions — the lock-per-field defect `HostState` fixed —
    /// leaves every one of them green, because the lock lives in the host and
    /// the state is generic over the handle precisely so it can be tested
    /// without one. A started `AccountStoreHandle` cannot be constructed here,
    /// so the host's own lock use is unreachable by a behavioural test and
    /// `review-method.md` § Evidence standard sanctions asserting the source
    /// shape instead: each of `finish` and `take` takes `self.state()` exactly
    /// once, and that one acquisition is the whole transition.
    #[test]
    fn install_and_teardown_each_take_the_host_lock_exactly_once() {
        let finish = method_code("pub async fn finish(");
        assert_eq!(
            finish.matches("self.state()").count(),
            1,
            "`finish` must run `settle_install` as ONE transition under one lock; a \
             second `self.state()` reopens the gap a teardown can land in:\n{finish}"
        );
        assert!(
            finish.contains("self.state().settle_install("),
            "the single acquisition in `finish` must be the whole install decision"
        );

        let take = method_code("pub fn take(");
        assert_eq!(
            take.matches("self.state()").count(),
            1,
            "`take` must run `tear_down` as ONE transition under one lock:\n{take}"
        );
        assert!(
            take.contains("self.state().tear_down("),
            "the single acquisition in `take` must be the whole teardown decision"
        );
    }
}
