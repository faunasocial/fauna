import Foundation

/// The running-app new-message OS banner — apple's leg of `conversations`
/// outcome 11 (*a new message raises a system notification while the app is
/// running, except in the conversation you already have open*).
///
/// **The decision is not here.** `docs/goal/ui/conversations.md` § Where logic
/// lives puts the whole when/for-whom question in the shared
/// `MessageNotificationTracker` (`fauna_conversations`), unit-tested once and
/// identical on every app: seed silently on the first non-empty snapshot, fire
/// when a thread's `lastActivityMs` increases or a brand-new thread appears,
/// suppress the selected thread. What this type owes is the *firing* — one
/// `NotificationManager.postMessageNotification` per returned activity — plus
/// the log the e2e witness reads. The windows twin is
/// `FaunaApp/Conversations/MessageToastObserver.cs`; linux's is `main.rs`'s
/// tray/toast loop; tui's is `conversations::fire_message_banners`.
///
/// ⚠ **There is no fourth rule.** Do not add an "the app is frontmost" or "the
/// window is key" suppression: that divergence was removed from linux on
/// 2026-09-20 because it falsified the outcome in its most ordinary case — the
/// user *is* in the app, on another page — and the goal doc now says in as many
/// words that the three rules above are the whole decision.
///
/// **Ticked from `ConversationsVM.onManagerChanged`**, not from a second
/// `SnapshotObserver` of its own: that callback is already the app-lifetime
/// manager observer, and it already runs on the main actor via
/// `notifyOnMainActor` — which is exactly the marshalling windows has to arrange
/// by hand (its `MessageToastObserver` enqueues on the captured `DispatcherQueue`
/// because the manager fires `OnChanged` synchronously, mid-mutation, on the
/// mutator's thread). Riding the one existing observer also keeps
/// `ConversationsManager.observer_count()` flat, and means a message reaching the
/// manager by *any* route — the real MLS receive loop, the mail rail, an e2e
/// injection — raises a banner without each route remembering to.
@MainActor
public final class MessageBannerObserver {
    /// The platform firing half. Owned rather than injected app-side because
    /// both shells would otherwise each construct one (priority #1) — and until
    /// this type existed, `NotificationManager` had no instance anywhere.
    private let notifications: NotificationManager

    /// The shared decision. `var`, because a fresh one is the identity-change
    /// reset — see ``resetForIdentityChange()``.
    private var tracker: MessageNotificationTracker

    public init(notifications: NotificationManager = NotificationManager()) {
        self.notifications = notifications
        self.tracker = MessageNotificationTracker()
    }

    /// One diff tick: project the snapshot, ask the shared tracker, fire what it
    /// returns, record what the platform took.
    ///
    /// The two barrier bumps are not optional bookkeeping. Two of outcome 11's
    /// three rules are *negative* — a banner that must NOT appear — and there is
    /// nothing on screen to watch for its non-arrival, so the witness proves a
    /// tick that *began* after its plant has finished (`fauna_e2e_agent::
    /// MESSAGE_BANNERS_KEY`). Hence `bannerPassStarted()` strictly **before** the
    /// snapshot read this tick will diff, and `bannerPassCompleted()` after the
    /// last fire — including on a tick that fired nothing, which is most of them.
    ///
    /// **All three bookkeeping calls sit behind `#if DEBUG`, and must.**
    /// `bannerPassStarted`, `recordFiredBanner` and `bannerPassCompleted` are
    /// `test-helpers` UniFFI seams: the shared crate keys their `uniffi::export` on
    /// that feature alone and gives the release build a "no surface" twin, so the
    /// production FFI flavor's Swift bindings do not carry them at all — they are
    /// *absent*, not no-ops (the no-op twin is the Rust callers' — linux, tui).
    /// `#if DEBUG` is the apple pairing for that flavor (convention 15), the same
    /// gate `AppStateObservables` puts on `messageBannersJsonText()`. Ungated, this
    /// file compiles under `mac-debug`/`swift-test` and fails only under
    /// `-c release` — `apple-store-safe-check`, `mac-release` — which is exactly
    /// how it first shipped (2026-09-21).
    public func tick(manager: ConversationsManager) {
        #if DEBUG
        bannerPassStarted()
        #endif
        let snapshot = manager.snapshot()
        // The shared projection, never a field-by-field constructor: a field
        // the decision grows reaches every app with no per-app edit.
        let activities = snapshot.threads.map { threadActivityFromSummary(summary: $0) }
        for activity in tracker.diff(
            threads: activities, selected: snapshot.selectedThreadId,
            launchFloorMs: snapshot.launchFloorMs
        ) {
            // `label` is the DM peer / group name, `snippet` the message preview —
            // the same two fields every other app's firing site hands its platform.
            let raised = notifications.postMessageNotification(
                from: activity.label, subject: activity.snippet,
                conversationId: activity.threadId
            )
            guard raised else {
                // The process has no notification host (a bare, non-bundle build —
                // `NotificationHost`). Loud rather than silent: recording here
                // would put a banner in a log whose entries mean "the user was
                // shown this", and swallowing it without a word is how a real
                // firing regression would hide behind a build-shape quirk.
                logMessage(
                    level: .warn, target: "fauna.conversations.banner",
                    message: "[banner] not raised for \(activity.threadId): no notification "
                        + "host in this build (NotificationHost.isAvailable == false)"
                )
                continue
            }
            // Recorded at the FIRING site, after every suppression above it, so the
            // log means "a banner was raised for this thread" and never "the tracker
            // returned this" — glue that swallowed a decision has to fail the
            // witness, not pass it.
            #if DEBUG
            recordFiredBanner(threadId: activity.threadId, label: activity.label)
            #endif
        }
        #if DEBUG
        bannerPassCompleted()
        #endif
    }

    /// Drop the outgoing identity's seed — called from `ConversationsVM.deactivate()`,
    /// which `ActorScope.dropAppOwnedState` runs at every account switch / sign-out.
    ///
    /// **Load-bearing, not defensive.** `ConversationsManager.clearForIdentityChange()`
    /// wipes the threads but deliberately preserves observers, and the tracker's
    /// `seeded` flag never resets on its own — so a tracker carried across the switch
    /// would find none of the incoming identity's restored threads in its
    /// `lastActivityMs` map and toast once per restored thread. linux reaches the
    /// same place by building a fresh tracker each time it re-attaches its observer;
    /// apple's observer is never dropped, so the reset is explicit.
    public func resetForIdentityChange() {
        tracker = MessageNotificationTracker()
    }
}
