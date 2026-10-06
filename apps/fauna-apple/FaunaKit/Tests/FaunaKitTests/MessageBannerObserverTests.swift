import Foundation
import Testing
@testable import FaunaKit

// The apple half of `conversations` outcome 11 — *a new message raises a system
// notification while the app is running, except in the conversation you already
// have open* (`docs/goal/ui/conversations.md` § Where logic lives).
//
// **Scope, deliberately narrow.** The three rules themselves are the shared
// `MessageNotificationTracker`'s and are unit-tested once in
// `libs/fauna-conversations/src/notification.rs`; re-asserting them here would
// pin a copy that does not exist. What these tests pin is the APPLE WIRING the
// shared tests cannot see: that `MessageBannerObserver` projects the snapshot and
// fires what the tracker returns, and — the one that has already cost the fleet a
// real bug on another app — that the tracker is REBUILT at the identity change.
//
// **Why the open-thread suppression is not here.** Proving it needs a *second*
// message in an already-selected thread, and `ConversationsTestInject.injectInbound`
// stamps `timestampMs` from the wall clock: two injects inside one millisecond
// leave `last_activity_ms` unchanged, so the tracker would decline to fire for a
// reason that has nothing to do with suppression and the assertion would pass
// vacuously (convention 14 — a test whose correctness turns on wall-clock timing is
// defunct, not flaky). That rule is witnessed end to end instead, over real
// round trips, by `tests/e2e-unified/tests/test_conversations_message_banner.py`
// `--app ios`, which drives this exact FaunaKit code.

/// Stands in for the platform firing call so the decision path is observable
/// headlessly. Also stands in for a notification host that exists at all: the real
/// `NotificationManager` returns `false` in any non-bundle process
/// (`NotificationHost.isAvailable`), which a unit-test binary always is.
private final class RecordingNotifications: NotificationManager, @unchecked Sendable {
    var raised: [String] = []

    override func postMessageNotification(
        from: String, subject: String, conversationId: String
    ) -> Bool {
        raised.append(conversationId)
        return true
    }
}

@MainActor
private func mockBackedManager() -> ConversationsManager {
    let manager = ConversationsManager()
    // `ingest_inbound` throws `NotSupported` on a rail with no registered
    // backend, exactly as it does for the app shells' inject seam.
    manager.installMockBackendsForTest()
    return manager
}

@MainActor
private func inject(_ sender: String, _ body: String, into manager: ConversationsManager) {
    ConversationsTestInject.injectInbound(
        ["rail": "FaunaMls", "sender": sender, "body": body], into: manager
    )
}

@MainActor
@Test func theThreadsAlreadyThereAtLoginRaiseNoBanner() {
    let manager = mockBackedManager()
    let fake = RecordingNotifications()
    let observer = MessageBannerObserver(notifications: fake)

    inject("alpha@self-nest.test", "already here when you signed in", into: manager)
    observer.tick(manager: manager)

    #expect(fake.raised.isEmpty, """
        the first non-empty snapshot seeds the tracker silently — a banner for a \
        thread that was already there is a sign-in toast storm. Raised: \(fake.raised)
        """)
}

@MainActor
@Test func aBrandNewThreadRaisesExactlyOneBanner() {
    let manager = mockBackedManager()
    let fake = RecordingNotifications()
    let observer = MessageBannerObserver(notifications: fake)

    inject("alpha@self-nest.test", "seeds the tracker", into: manager)
    observer.tick(manager: manager)
    inject("beta@self-nest.test", "a message you are not looking at", into: manager)
    observer.tick(manager: manager)

    let beta = manager.snapshot().threads.first { $0.label.contains("beta") }?.threadId
    #expect(beta != nil, "the beta inject did not land in a thread")
    #expect(fake.raised == [beta].compactMap { $0 }, """
        a brand-new thread must raise exactly one banner, for itself. \
        Raised: \(fake.raised); expected the beta thread \(String(describing: beta))
        """)
}

@MainActor
@Test func theIncomingIdentitysRestoredThreadsSeedSilently() {
    let manager = mockBackedManager()
    let fake = RecordingNotifications()
    let observer = MessageBannerObserver(notifications: fake)

    inject("alpha@self-nest.test", "the outgoing identity's thread", into: manager)
    observer.tick(manager: manager)
    #expect(fake.raised.isEmpty, "precondition: the seeding tick is silent")

    // The account switch / sign-out, in the order `ActorScope.dropAppOwnedState`
    // runs it: the manager's identity-scoped wipe (which PRESERVES observers),
    // then `ConversationsVM.deactivate()`, whose banner-side duty is this reset.
    manager.clearForIdentityChange()
    observer.resetForIdentityChange()

    // The incoming identity's threads arriving for the first time.
    inject("gamma@other-nest.test", "the incoming identity's restored thread", into: manager)
    observer.tick(manager: manager)

    // Mutation check: delete `resetForIdentityChange()` from
    // `ConversationsVM.deactivate()` — or its body here — and this goes red.
    // The tracker's `seeded` flag never clears on its own and the wipe leaves its
    // `last_activity_ms` map holding only the departed identity's threads, so a
    // carried tracker finds every restored thread "new" and toasts one apiece.
    #expect(fake.raised.isEmpty, """
        the threads restored for the INCOMING identity are not new messages — the \
        fresh tracker must seed on them silently. Raised: \(fake.raised)
        """)
}
