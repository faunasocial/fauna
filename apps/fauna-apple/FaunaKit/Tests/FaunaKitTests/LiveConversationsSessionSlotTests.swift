import Testing
@testable import FaunaKit

/// `LiveConversationsSessionSlot`: the process's newest conversations session
/// per actor, which every `APIClient` of that actor reads before its own cache
/// — a second client's build retires the first's engine, so the first client's
/// cached session must not be the one an act posts through.
@Suite struct LiveConversationsSessionSlotTests {
    final class FakeSession {}

    @Test func theNewestBuildForAnActorIsTheOneServed() {
        let slot = LiveConversationsSessionSlot<FakeSession>()
        let first = FakeSession(), second = FakeSession()
        slot.set(actorSecretHex: "aa", session: first)
        slot.set(actorSecretHex: "aa", session: second)
        #expect(slot.session(for: "aa") === second)
    }

    @Test func anotherActorsSessionIsNeverServed() {
        let slot = LiveConversationsSessionSlot<FakeSession>()
        slot.set(actorSecretHex: "aa", session: FakeSession())
        #expect(slot.session(for: "bb") == nil)
    }

    @Test func releaseForgetsOnlyItsOwnActor() {
        let slot = LiveConversationsSessionSlot<FakeSession>()
        let live = FakeSession()
        slot.set(actorSecretHex: "aa", session: live)
        slot.clear(actorSecretHex: "bb")
        #expect(slot.session(for: "aa") === live)
        slot.clear(actorSecretHex: "aa")
        #expect(slot.session(for: "aa") == nil)
    }
}
