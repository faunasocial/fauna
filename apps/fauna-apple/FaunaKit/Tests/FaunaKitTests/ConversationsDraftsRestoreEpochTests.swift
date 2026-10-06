import Foundation
import Testing
@testable import FaunaKit

// The apple shell pin for `docs/goal/ui/conversations.md` § Persistence → *A
// restore the outgoing account started fills nothing after an identity change*:
// `ConversationsVM.restoreDraftsOnLaunch` reads the manager's identity epoch
// **synchronously, before the load is awaited**, and hands it to
// `restoreDraftsAt`, which refuses once the epoch has moved.
//
// **Why a shell pin.** The shared library pins the manager's half
// (`libs/fauna-conversations/tests/manager_integration_tests.rs`, the refusal
// and its positive control) and cannot see WHEN a shell reads the epoch: a shell
// that reads it after the load resolves hands back the *new* epoch and the
// stale reply fills the manager the incoming account uses. tui, linux and
// windows each pin their own read
// (`apps/fauna-linux/src/conversations/drafts.rs`
// `a_load_the_outgoing_account_started_fills_nothing_after_the_switch`,
// `ConversationDraftsServiceTests.cs`
// `RestoreOnLaunch_ReadsTheIdentityEpochBeforeTheLoad_NotAfter`); this is apple's.
//
// The double is a handle-less `FfiDraftsSync` (`noHandle:`, the seam UniFFI
// generates for fakes) whose `load()` parks until the test releases the reply —
// the outgoing account's fetch still in flight when the switch lands.
//
// Mutation check: in `ConversationsVM.restoreDraftsOnLaunch`, move
// `manager.identityEpoch()` from before the `Task` to inside it, after
// `sync.load()` returns — both negative arms go red (the positive arm stays
// green, so the red is the epoch read and not a restore that never fills).

/// A drafts autosync whose launch `load()` answers only when the test says so.
private final class HeldDraftsSync: FfiDraftsSync, @unchecked Sendable {
    private let entered = AsyncStream<Void>.makeStream()
    private let reply = AsyncStream<Data>.makeStream()

    override func load() async throws -> Data? {
        entered.continuation.yield()
        return await reply.stream.first { _ in true }
    }

    /// The autosave a manager notification arms after a fill. Off the fake's
    /// missing handle: the inherited implementation would call into Rust.
    override func saveIfChanged(snapshot: Data) async throws -> Bool { false }

    func loadHasStarted() async { _ = await entered.stream.first { _ in true } }
    func answer(_ bytes: Data) { reply.continuation.yield(bytes) }
}

/// The canonical drafts snapshot of an account whose new-thread composer holds
/// `body` — what `FfiDraftsSync.load()` would unseal for that account.
@MainActor
private func draftsBlob(newThreadBody body: String) -> Data {
    let account = ConversationsManager()
    account.startNewConversation()
    account.setNewThreadBody(body: body)
    return account.draftsSnapshotBytes()
}

/// What the new-thread composer holds once opened, the way the UI opens it.
@MainActor
private func newThreadBody(of vm: ConversationsVM) -> String? {
    vm.manager.startNewConversation()
    return vm.manager.snapshot().newThreadCompose?.bodyDraft
}

private let outgoingDraft = "the outgoing account's draft"

@MainActor
@Test("a launch restore the outgoing account started fills nothing once the switch has landed")
func aRestoreStartedBeforeTheSwitchFillsNothingAfterIt() async {
    let blob = draftsBlob(newThreadBody: outgoingDraft)
    let vm = ConversationsVM()
    let held = HeldDraftsSync(noHandle: .init())

    vm.attachDraftsSync(held)
    // `ActorScope.dropAppOwnedState`'s order: the manager's identity-scoped
    // wipe, then `deactivate()` — which must not (and does not) cancel the
    // restore, so the reply below still arrives and must be refused.
    vm.manager.clearForIdentityChange()
    vm.deactivate()
    held.answer(blob)
    await vm.draftsRestoreSettledForTesting()

    #expect(newThreadBody(of: vm) == "", """
        the outgoing account's reply filled the manager the incoming account uses \
        — the epoch was read after the load, not before it
        """)
}

@MainActor
@Test("a launch load already in flight when the switch lands fills nothing")
func aLoadInFlightAtTheSwitchFillsNothing() async {
    let blob = draftsBlob(newThreadBody: outgoingDraft)
    let vm = ConversationsVM()
    let held = HeldDraftsSync(noHandle: .init())

    vm.attachDraftsSync(held)
    await held.loadHasStarted()
    vm.manager.clearForIdentityChange()
    vm.deactivate()
    held.answer(blob)
    await vm.draftsRestoreSettledForTesting()

    #expect(newThreadBody(of: vm) == "", """
        the outgoing account's reply, answered mid-switch, filled the manager \
        the incoming account uses
        """)
}

@MainActor
@Test("a launch restore started after the switch still fills")
func aRestoreStartedAfterTheSwitchStillFills() async {
    let blob = draftsBlob(newThreadBody: outgoingDraft)
    let vm = ConversationsVM()
    let held = HeldDraftsSync(noHandle: .init())

    // The identity change happened first; THIS restore is the incoming
    // account's own, so its epoch is the moved one and the reply is honoured.
    vm.manager.clearForIdentityChange()
    vm.attachDraftsSync(held)
    held.answer(blob)
    await vm.draftsRestoreSettledForTesting()

    #expect(newThreadBody(of: vm) == outgoingDraft, """
        a restore started after the identity change must fill — otherwise the \
        two arms above are green for a restore that never fills at all
        """)
}
