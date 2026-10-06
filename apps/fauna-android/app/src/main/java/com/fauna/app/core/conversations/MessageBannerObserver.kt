package com.fauna.app.core.conversations

import com.fauna.app.core.ShellLog
import com.fauna.app.testing.TestAgent
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.MessageNotificationTracker
import uniffi.fauna_conversations.ThreadActivity
import uniffi.fauna_conversations.threadActivityFromSummary

/**
 * The running-app new-message OS banner — android's leg of `conversations`
 * outcome 11 (*a new message raises a system notification while the app is
 * running, except in the conversation you already have open*).
 *
 * **The decision is not here.** `docs/goal/ui/conversations.md` § Where logic
 * lives puts the whole when/for-whom question in the shared
 * [MessageNotificationTracker] (`fauna_conversations`), unit-tested once and
 * identical on every app: seed silently on the first non-empty snapshot, fire
 * when a thread's unread count rises while its newest activity is at or past
 * the snapshot's launch floor, suppress the selected thread. What this class owes is the *firing* —
 * one [raise] per returned activity — plus the log the e2e witness reads. Its
 * twins are apple's FaunaKit `MessageBannerObserver` (same name, same shape)
 * and windows' `MessageToastObserver`.
 *
 * ⚠ **There is no fourth rule.** Do not add an "the app is in the foreground"
 * or "the activity is resumed" suppression: that divergence was removed from
 * linux on 2026-09-20 because it falsified the outcome in its most ordinary case
 * — the user *is* in the app, on another page — and the goal doc says the three
 * rules are the whole decision. What android itself does with a notification
 * posted while the app is frontmost is the platform's business.
 *
 * **Ticked from [ConversationsManagerHost]'s app-lifetime observer**, not from a
 * `SnapshotObserver` of its own, so a message reaching the manager by any route —
 * the real receive loop, the mail rail, an e2e injection — raises a banner
 * without each route remembering to, and the observer count stays flat.
 *
 * @param raise the platform firing half; returns whether the banner was actually
 *   handed to the platform. The app passes [com.fauna.app.core.NotificationHelper];
 *   a unit test passes a recorder.
 */
class MessageBannerObserver(private val raise: (ThreadActivity) -> Boolean) {
    // Guards the tracker swap and serialises ticks. The host's observer fires on
    // whichever Rust thread mutated the manager, so two ticks can overlap; the
    // tracker is internally synchronised, but a tick that read an OLDER snapshot
    // and diffed it second would roll a thread's stamp back and let the next
    // tick fire that thread twice. One lock around started → snapshot → diff →
    // fires → completed keeps the stateful diff in snapshot order (windows takes
    // the same lock for the same reason).
    private val lock = Any()

    // `var`, because a fresh tracker is the identity-change reset — see
    // [resetForIdentityChange].
    private var tracker = MessageNotificationTracker()

    /**
     * One diff tick: project the snapshot, ask the shared tracker, fire what it
     * returns, record what the platform took.
     *
     * The two barrier bumps are not optional bookkeeping. Two of outcome 11's
     * three rules are *negative* — a banner that must NOT appear — so the witness
     * proves that a tick which *began* after its plant has finished
     * (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`). Hence
     * [TestAgent.bannerPassStarted] strictly **before** the snapshot read and
     * [TestAgent.bannerPassCompleted] after the last fire, including on a tick
     * that fired nothing, which is most of them. The three recorders are
     * `test-helpers` UniFFI seams the shipping bindings do not carry, which is
     * why they are reached through the agent's same-signature twins rather than
     * called here.
     */
    fun tick(manager: ConversationsManager) {
        synchronized(lock) {
            TestAgent.bannerPassStarted()
            val snapshot = manager.snapshot()
            // The shared projection, never a field-by-field constructor: a field
            // the decision grows reaches every app with no per-app edit.
            val activities = snapshot.threads.map { threadActivityFromSummary(it) }
            // The floor comes off the SAME snapshot as the threads — it is the
            // store's, so the banner and the unread indicator read one value.
            for (activity in tracker.diff(activities, snapshot.selectedThreadId, snapshot.launchFloorMs)) {
                if (!raise(activity)) {
                    // Loud rather than silent, and NOT recorded: no banner reached
                    // the platform, and an entry in the fired log means one did.
                    ShellLog.w(
                        "MessageBanner",
                        "[banner] not raised for ${activity.threadId}: the notification " +
                            "call failed (NotificationManagerCompat refused it)",
                    )
                    continue
                }
                // Recorded at the FIRING site, after every suppression above it,
                // so the log means "a banner was raised for this thread" and never
                // "the tracker returned this".
                TestAgent.recordFiredBanner(activity.threadId, activity.label)
            }
            TestAgent.bannerPassCompleted()
        }
    }

    /**
     * Drop the outgoing identity's seed — called from
     * [ConversationsManagerHost.stopConversationsSession], which every sign-out
     * and account switch runs.
     *
     * **Load-bearing, not defensive.** The tracker's `seeded` flag never resets
     * on its own, and its stamp map holds only the departed identity's threads,
     * so a tracker carried across the switch would find every thread the
     * incoming identity restores "new" and raise one banner apiece. linux, web
     * and apple each rebuild the tracker at this boundary for the same reason.
     */
    fun resetForIdentityChange() {
        synchronized(lock) {
            val outgoing = tracker
            tracker = MessageNotificationTracker()
            outgoing.close()
        }
    }
}
