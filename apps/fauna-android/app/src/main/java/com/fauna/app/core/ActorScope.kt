package com.fauna.app.core

import com.fauna.app.testing.TestAgent
import javax.inject.Inject
import javax.inject.Singleton

/**
 * **The one canonical actor-scoped drop for android** (`account-scoping.md`
 * § The scoping taxonomy → the in-memory corollary): every teardown site calls
 * [dropActorScopedState] **and nothing else**, with no list of its own.
 *
 * The rule exists because a drop hand-listed at each teardown site rots the
 * first time someone adds state and forgets a site — and *the drift, not the
 * missing piece of the day, is the finding*. android's drift was the inverse of
 * the other apps': not one list copied to N sites, but **two funnels and no rule
 * saying which a new surface joins** — [ApiClient.clearAuth] (the session rails,
 * which every identity teardown did route through) and
 * [AccountStores.closeOpenStores] (the self-registering closers). They did not
 * cover the same call sites: a factory reset and the post-auth
 * `NestIdentityChanged` re-entry called `clearAuth()` alone and ran no closer at
 * all, and four actor-scoped surfaces had joined neither. This class is the
 * single door that makes "on neither" unrepresentable.
 *
 * **Two levels, deliberately** — the shape the goal doc sanctions for a drop
 * that spans scopes ("one list per assembly, the outer calling the inner",
 * apple's per-target `dropActorScopedState()` over FaunaKit's
 * `resetSharedState()`; windows' over `Core.Services.ActorScope`) — never a list
 * per *site*:
 *
 *  - **Outer (here):** the ordered, statically greppable drop. This is where a
 *    new *process-scoped* surface goes, and where the ORDER lives.
 *  - **Inner:** [AccountStores.registerCloser], for state a process-scoped list
 *    cannot name — activity-/ViewModel-scoped holders
 *    ([com.fauna.app.ui.viewmodel.SupervisedIndicatorVM], the e2e session
 *    override on [AppState]) and Hilt `@Singleton`s whose construction is lazy,
 *    where naming them here would *construct* them (and run their `init`
 *    network fetches) on a teardown path. Those register their drop next to
 *    their own state, which is where it belongs.
 *
 * **Order is load-bearing.** The closers run FIRST, while the nest client is
 * still up: they include the native teardowns that must complete before the
 * client they were built over goes away, and — on sign-out — before
 * [AccountStores.eraseAllAccounts] deletes the very directories they hold open.
 * `clearAuth()` follows, tearing down the session rails and the client itself.
 *
 * **A drop is only half of it.** A background loop that holds no cancellation
 * handle cannot be stopped by any list, so the seam comes before the drop
 * (linux's actor-generation counter, apple's cadences owning their `Task`,
 * windows' lease-per-loop token). android has **two** such loops:
 *
 *  1. the custodian foreground push-debounce — shared Rust states outright that
 *     it cannot self-exit when its source disconnects — which carries its seam
 *     next to its state ([com.fauna.app.service.CustodianPushKick]), as a
 *     registered closer that cancels the native handle synchronously and
 *     re-arms for the incoming actor;
 *  2. the events rail's **launch restore**
 *     ([com.fauna.app.core.events.EventDraftsHost]), a `fauna.drafts.get`
 *     round-trip started before `connect()` and so in flight for up to the
 *     request deadline. Cancelling its `Job` is necessary but never sufficient
 *     — a coroutine already suspended inside the UniFFI call is not cancelled
 *     mid-call and still returns — so it carries an actor-generation counter,
 *     linux's form: bumped by both `startDraftsSync` and `stopDraftsSync`,
 *     captured at launch, re-checked before every write of the live draft.
 *
 * The events rail was added after this paragraph first claimed android had one
 * such loop, and reopened the in-memory dimension the ledger had recorded
 * closed — which is the standing
 * reason a new background writer of actor-scoped state updates this list in the
 * same change that introduces it.
 */
@Singleton
class ActorScope @Inject constructor(
    private val api: ApiClient,
    private val accountStores: AccountStores,
) {

    /**
     * Retire everything scoped to the OUTGOING identity. Idempotent, safe with
     * no session established, and safe from the main thread (every step is
     * either synchronous or hands off to its owner's own scope).
     *
     * Callers: the account switch, sign-out, delete-account, the admin factory
     * reset, the post-auth `NestIdentityChanged` re-entry, and the test agent's
     * `reset` / `logout` arms. **That list is a fact about this function's
     * callers, not a list any of them keeps** — each calls this and nothing
     * else.
     *
     * Erasure is deliberately NOT here: dropping in-memory state and erasing
     * on-disk state are different obligations with different callers
     * (`account-scoping.md` § Erasure follows scope). Sign-out drops *then*
     * erases; a switch only drops.
     */
    fun dropActorScopedState() {
        // Count the teardown at its initiation, before anything below runs —
        // the e2e session generation (`fauna_e2e_agent::SESSION_GENERATION_KEY`).
        // This is the one door every teardown site calls, so the count cannot
        // miss a site; a no-op in release (the `noAgent` twin).
        TestAgent.recordSessionTeardown()
        // The inner list, plus the account-scoped handles AccountStores owns and
        // the first-adopter guards with them: a handle opened under the outgoing
        // account points at that account's files no matter who is active now.
        accountStores.endActiveAccountSession()
        // The session rails: the FFI clients, the six pumps, the auto-renew
        // cadence, the conversations receive session, the drafts autosave, and
        // the critical-alert banner keyed to the departing DID.
        api.clearAuth()
    }
}
