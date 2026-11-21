import Foundation
import Testing
@testable import FaunaKit

// The tier_1 pin for the Media page's write controls on macos and ios — one
// FaunaKit change paints both (`ui/media.md` § Errors & edge cases → *What this
// person may write*; `ui/README.md` § Offer only what is possible, rules 2, 5
// and 6). `file-upload` / `upload-button` paint from the shared
// `MediaMachine::upload_possibility`, `media-delete-button` and every
// `file-version-restore-button` from `write_possibility(<item filter value>)`,
// through `faunaPossible` / `FaunaPossibilityReason`. Which answer the machine
// gives for which set is pinned in shared Rust; what is pinned here is that the
// binding reaches the apple apps, that the VM hands the answer over whole (no
// Swift re-derivation) and how the painter reads each case. How a painted
// control LOOKS is the last inch.

/// A handle-less `MediaMachine` whose two answers are scripted — the seam UniFFI
/// generates for fakes. Every call `configure` makes on a machine is overridden,
/// so nothing reaches Rust.
private final class ScriptedAnswersMachine: MediaMachine, @unchecked Sendable {
    var upload: Possibility
    var write: [String: Possibility]
    private(set) var askedFolders: [String] = []

    init(upload: Possibility, write: [String: Possibility] = [:]) {
        self.upload = upload
        self.write = write
        super.init(noHandle: .init())
    }

    required init(unsafeFromHandle handle: UInt64) {
        fatalError("a scripted fake is never lifted from Rust")
    }

    override func uploadPossibility() -> Possibility { upload }

    override func writePossibility(folder: String) -> Possibility {
        askedFolders.append(folder)
        return write[folder] ?? .possible
    }

    override func refresh(backupKey: Data?) async {}
    override func setOwnerBackupKey(key: Data) {}
    override func setShareAuthor(secret: Data, nestUrl: String) {}
}

/// An `APIClient` vending one prepared Media machine.
private final class OneMachineAPI: APIClient {
    private let make: (MediaObserver) async throws -> MediaMachine

    init(_ make: @escaping (MediaObserver) async throws -> MediaMachine) {
        self.make = make
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func mediaMachine(observer: MediaObserver) async throws -> MediaMachine {
        try await make(observer)
    }
}

private func blocked(_ key: String) -> Possibility {
    .blocked(reason: LocalizedText(key: key, args: [:]))
}

private func reasonKey(_ answer: Possibility) -> String? {
    guard case .blocked(let reason) = answer else { return nil }
    return reason.key
}

@MainActor
private func configured(_ machine: MediaMachine) async -> MediaMachineVM {
    let vm = MediaMachineVM()
    await vm.configure(api: OneMachineAPI { _ in machine }, deviceId: "device",
                       predecessors: MediaPredecessors())
    return vm
}

@Suite("Media write controls paint from the media machine's answers")
@MainActor
struct MediaWritePossibilityTests {
    // ── The real machine, offline: the binding reaches FaunaKit ────────────

    @Test("before the first read the upload controls are disabled, saying the page is still loading")
    func uploadBeforeTheFirstReadIsStillLoading() async throws {
        let machine = buildMediaMachine(nest: try await tornDownNest(), observer: MediaObserverBox())
        let answer = machine.uploadPossibility()
        #expect(answer.isShown && !answer.isPossible)
        #expect(reasonKey(answer) == "common.still_loading")
        #expect(answer.reasonText?.isEmpty == false, "the reason renders beside the button")
    }

    @Test("a first read that failed leaves the upload controls disabled with the load failure")
    func uploadAfterAFailedReadSaysSo() async throws {
        let vm = await configured(
            buildMediaMachine(nest: try await tornDownNest(), observer: MediaObserverBox()))
        let answer = vm.uploadPossibility
        #expect(answer.isShown && !answer.isPossible)
        #expect(reasonKey(answer) == "common.load_failed")
    }

    @Test("with no machine yet there is nothing to upload into or delete from")
    func noMachineIsAbsent() {
        let vm = MediaMachineVM()
        #expect(!vm.uploadPossibility.isShown)
        #expect(!vm.writePossibility(folder: "photos").isShown)
    }

    // ── The answers, handed over whole ──────────────────────────────────────

    @Test("a reader's target disables the upload controls with the read-only reason")
    func readerTargetBlocksTheUpload() async {
        let vm = await configured(ScriptedAnswersMachine(
            upload: blocked("media.error_read_only_shared_folder")))
        let answer = vm.uploadPossibility
        #expect(answer.isShown && !answer.isPossible, "visible: picking another folder lifts it")
        #expect(reasonKey(answer) == "media.error_read_only_shared_folder")
        #expect(answer.reasonText?.isEmpty == false)
    }

    @Test("a metadata-only target disables the upload controls with the metadata-only reason")
    func metadataOnlyTargetBlocksTheUpload() async {
        let vm = await configured(ScriptedAnswersMachine(
            upload: blocked("media.error_metadata_only_folder")))
        let answer = vm.uploadPossibility
        #expect(answer.isShown && !answer.isPossible)
        #expect(reasonKey(answer) == "media.error_metadata_only_folder")
    }

    @Test("a followed scope has no upload controls at all")
    func followedScopeHasNoUpload() async {
        let vm = await configured(ScriptedAnswersMachine(upload: .absent))
        #expect(!vm.uploadPossibility.isShown, "upload-button == 0, not a disabled button")
    }

    @Test("a reader's item detail has no delete and no restore; an owner's has both")
    func readerDetailHasNoDeleteOrRestore() async {
        let machine = ScriptedAnswersMachine(
            upload: .possible, write: ["local:shared": .absent, "photos": .possible])
        let vm = await configured(machine)

        let reader = MediaItemDetailView.writeAnswer(vm: vm, folder: "local:shared", followed: false)
        #expect(!reader.isShown, "media-delete-button and file-version-restore-button absent")

        let owner = MediaItemDetailView.writeAnswer(vm: vm, folder: "photos", followed: false)
        #expect(owner.isPossible)
        #expect(machine.askedFolders == ["local:shared", "photos"],
                "asked over the item's own filter value")
    }

    @Test("a followed item's detail has no delete and no restore, without asking the machine")
    func followedDetailHasNoWrite() async {
        let machine = ScriptedAnswersMachine(upload: .absent)
        let vm = await configured(machine)
        let answer = MediaItemDetailView.writeAnswer(vm: vm, folder: "follow:x", followed: true)
        #expect(!answer.isShown)
        #expect(machine.askedFolders.isEmpty)
    }
}
