import Foundation
import Testing
@testable import FaunaKit

// The post's mark-as-spam verb (`report-sharing.md` § Report capture → *The
// post's mark-as-spam verb*): `FeedVM.markPostSpam` is a toggle over the shared
// surfaces — the plane write first (its stored list reaches the manager's spam
// fold and the shared `ContentPolicyStore`), then the `train_moderation_correction`
// façade with the verb's new state; a failure of either reaches the page's
// `error-message` and never undoes the other. The manager is REAL, over a
// torn-down nest (nothing dials); the plane write and the façade are the VM's
// two seams.

private final class TornDownFeedAPI: APIClient {
    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func feedManager(secret secretHex: String, deviceIdHex: String? = nil) async throws -> FfiFeedManager {
        try await tornDownNest().feedManager(secret: Data(repeating: 7, count: 32))
    }
}

private struct SeamFailure: Error, CustomStringConvertible {
    let description: String
}

@Suite("FeedVM mark-as-spam toggle")
@MainActor
struct FeedMarkSpamTests {
    private static let post = String(repeating: "ab", count: 32)

    /// A configured VM whose plane write keeps a list and whose façade records
    /// each call.
    private func configuredVM() async throws -> (FeedVM, Box) {
        let vm = FeedVM()
        await vm.configure(api: TornDownFeedAPI(), secretHex: "aa", deviceIdHex: "")
        try #require(vm.manager != nil, "precondition: the manager is installed")
        let box = Box()
        vm.spamPlaneWrite = { id, isSpam in
            if let failure = box.planeFailure { throw SeamFailure(description: failure) }
            if isSpam { box.stored.append(id) } else { box.stored.removeAll { $0 == id } }
            return box.stored
        }
        vm.spamTrain = { _, id, isSpam in
            box.trained.append((id, isSpam))
            if let failure = box.trainFailure { throw SeamFailure(description: failure) }
        }
        return (vm, box)
    }

    final class Box: @unchecked Sendable {
        var stored: [String] = []
        var trained: [(String, Bool)] = []
        var planeFailure: String?
        var trainFailure: String?
    }

    @Test("a press marks: the list reaches the manager and the store, the façade trains spam")
    func aPressMarks() async throws {
        let (vm, box) = try await configuredVM()
        let store = ContentPolicyStore()

        await vm.markPostSpam(Self.post, contentPolicy: store)

        #expect(vm.isSpamMarked(Self.post), "the manager's fold must hold the mark")
        #expect(store.spamMarked == [Self.post], "the shared store must hold the stored list")
        #expect(box.trained.count == 1 && box.trained[0].1 == true, "the façade trains is_spam = true")
        #expect(vm.clientErrorMessage == nil)
    }

    @Test("a second press un-marks and trains ham — undo is the verb itself")
    func aSecondPressUnmarks() async throws {
        let (vm, box) = try await configuredVM()
        let store = ContentPolicyStore()

        await vm.markPostSpam(Self.post, contentPolicy: store)
        await vm.markPostSpam(Self.post, contentPolicy: store)

        #expect(!vm.isSpamMarked(Self.post))
        #expect(store.spamMarked.isEmpty)
        #expect(box.trained.map(\.1) == [true, false])
    }

    @Test("a façade failure reaches error-message and keeps the mark")
    func aTrainFailureKeepsTheMark() async throws {
        let (vm, box) = try await configuredVM()
        box.trainFailure = "nest refused"

        await vm.markPostSpam(Self.post, contentPolicy: nil)

        #expect(vm.isSpamMarked(Self.post), "a façade failure never undoes the plane mark")
        #expect(vm.clientErrorMessage == L.feed.errorMarkSpam(message: "nest refused"))
    }

    @Test("a plane failure still runs the façade and reaches error-message")
    func aPlaneFailureStillTrains() async throws {
        let (vm, box) = try await configuredVM()
        box.planeFailure = "plane locked"

        await vm.markPostSpam(Self.post, contentPolicy: nil)

        #expect(!vm.isSpamMarked(Self.post))
        #expect(box.trained.count == 1, "the nest half runs whatever the plane write did")
        #expect(vm.clientErrorMessage == L.feed.errorMarkSpam(message: "plane locked"))
    }

    @Test("marks loaded before the manager was built reach it")
    func marksLoadedEarlyReachTheManager() async throws {
        let vm = FeedVM()
        vm.setSpamMarked([Self.post])
        await vm.configure(api: TornDownFeedAPI(), secretHex: "aa", deviceIdHex: "")

        #expect(vm.isSpamMarked(Self.post))
    }
}
