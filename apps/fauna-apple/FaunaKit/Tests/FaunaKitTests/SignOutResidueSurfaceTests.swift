import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

/// `SignOutResidueSurface` — the apple seat of the sign-out residue surface
/// (`account-scoping.md` § Erasure follows scope → *the residue surface*): the
/// sign-out's record, Remove Again, and the signed-out launch's silent
/// re-check, over the `fauna-ffi` residue face. The face's own rules are pinned
/// in shared Rust; these pin the seat's plumbing — that the record lands under
/// the base the seat names, that the line is the localized `Rendered` copy, and
/// that the retry and launch re-check reach the same record.
///
/// Hermetic: temp bases and a private in-memory secret store under the
/// registry — never the developer's real Application Support root or keychain.
@Suite struct SignOutResidueSurfaceTests {
    private final class MemorySecretStore: FfiSecretStore, @unchecked Sendable {
        private var values: [String: String] = [:]
        private let lock = NSLock()
        func get(key: String) -> String? { lock.withLock { values[key] } }
        func set(key: String, value: String) { lock.withLock { values[key] = value } }
        func delete(key: String) { lock.withLock { _ = values.removeValue(forKey: key) } }
    }

    private static let actor = String(repeating: "ab", count: 32)

    /// One signed-out device: an install base, a store container, an empty
    /// registry, and the seat over them.
    private struct Device {
        let root: URL
        let base: URL
        let container: URL
        let seat: ResidueSeat

        init() throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("fauna-residue-\(UUID().uuidString)", isDirectory: true)
            base = root.appendingPathComponent("base", isDirectory: true)
            container = root.appendingPathComponent("container", isDirectory: true)
            try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: container, withIntermediateDirectories: true)
            let store = MemorySecretStore()
            let lockDir = base.path
            seat = ResidueSeat(
                baseDir: base.path, storeContainerDir: container.path,
                registry: { FfiAccountRegistry.newWithLockDir(store: store, lockDir: lockDir) })
        }

        /// An actor scope under the base holding the user's data.
        func scope() throws -> URL {
            let dir = base.appendingPathComponent(SignOutResidueSurfaceTests.actor, isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try Data("the user's data".utf8).write(to: dir.appendingPathComponent("mls.db"))
            return dir
        }

        var recordFile: URL { base.appendingPathComponent("sign-out-residue.json") }

        func remove() { try? FileManager.default.removeItem(at: root) }
    }

    private static func sweep(_ survivors: [URL]) -> FfiEraseSweep {
        FfiEraseSweep(
            erased: 0,
            survivors: survivors.map(\.path),
            residue: FfiEraseResidueView(
                survivors: UInt32(survivors.count), credentialsSurvived: false,
                owesWork: !survivors.isEmpty))
    }

    private static let clean = FfiCredentialSweep(survivors: [], wipeFailed: false)

    @Test func aCleanSignOutPaintsNothingAndKeepsNoRecord() throws {
        let device = try Device()
        defer { device.remove() }

        #expect(SignOutResidueSurface.record(
            seat: device.seat, sweep: Self.sweep([]), credentials: Self.clean) == nil)
        #expect(!FileManager.default.fileExists(atPath: device.recordFile.path))
    }

    /// The line is the `Rendered` copy — it names the Remove Again button the
    /// view paints beside it — and the record is on disk for the relaunch.
    @Test func aSurvivorPaintsTheRemoveAgainLineAndIsRecordedUnderTheBase() throws {
        let device = try Device()
        defer { device.remove() }
        let scope = try device.scope()

        let residue = try #require(SignOutResidueSurface.record(
            seat: device.seat, sweep: Self.sweep([scope]), credentials: Self.clean))
        #expect(residue.line == L.settings.signOutResidue(count: "1"))
        #expect(FileManager.default.fileExists(atPath: device.recordFile.path))
    }

    /// Credentials alone owe the credentials line — the sweep went, the
    /// keychain did not.
    @Test func survivingCredentialsAlonePaintTheCredentialsLine() throws {
        let device = try Device()
        defer { device.remove() }

        let residue = try #require(SignOutResidueSurface.record(
            seat: device.seat, sweep: Self.sweep([]),
            credentials: FfiCredentialSweep(survivors: ["fauna/ab/secret"], wipeFailed: false)))
        #expect(residue.line == L.settings.signOutResidueCredentials)
    }

    /// Remove Again over a scope that can now go: the scope, the record and the
    /// view all go.
    @Test func removeAgainFinishesTheEraseAndClosesTheView() throws {
        let device = try Device()
        defer { device.remove() }
        let scope = try device.scope()
        let residue = try #require(SignOutResidueSurface.record(
            seat: device.seat, sweep: Self.sweep([scope]), credentials: Self.clean))

        #expect(residue.retry() == nil, "a clean re-sweep closes the view")
        #expect(!FileManager.default.fileExists(atPath: scope.path))
        #expect(!FileManager.default.fileExists(atPath: device.recordFile.path))
    }

    /// The POSIX fault the e2e journeys inject: a read-only scope directory.
    /// While it holds, the retry keeps the view and its line; the signed-out
    /// launch re-check paints it again from the record; once lifted, the
    /// launch re-check finishes the erase without a word.
    @Test func theLaunchRecheckRepaintsWhatIsStillLeftAndFinishesSilently() throws {
        let device = try Device()
        defer { device.remove() }
        let scope = try device.scope()
        try FileManager.default.setAttributes([.posixPermissions: 0o555], ofItemAtPath: scope.path)
        defer {
            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: scope.path)
        }
        // A process that writes through a read-only dir (root) would make every
        // assertion below pass for the wrong reason.
        let probe = scope.appendingPathComponent(".probe")
        if FileManager.default.createFile(atPath: probe.path, contents: Data()) {
            try? FileManager.default.removeItem(at: probe)
            return
        }

        let residue = try #require(SignOutResidueSurface.record(
            seat: device.seat, sweep: Self.sweep([scope]), credentials: Self.clean))
        let still = try #require(residue.retry(), "the scope is still there")
        #expect(still.line == residue.line)

        let relaunched = try #require(
            SignOutResidueSurface.recheckAtLaunch(seat: device.seat),
            "a residue still on the disk is still on the screen after a relaunch")
        #expect(relaunched.line == L.settings.signOutResidue(count: "1"))

        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: scope.path)
        #expect(SignOutResidueSurface.recheckAtLaunch(seat: device.seat) == nil)
        #expect(!FileManager.default.fileExists(atPath: scope.path))
        #expect(!FileManager.default.fileExists(atPath: device.recordFile.path))
    }

    @Test func aLaunchWithNoRecordPaintsNothing() throws {
        let device = try Device()
        defer { device.remove() }
        #expect(SignOutResidueSurface.recheckAtLaunch(seat: device.seat) == nil)
    }
}

/// `OnboardingVM.signOutResidue` — the state behind `identity_choice`'s
/// `sign-out-residue` view, driven by a fake residue. Twin of windows'
/// `OnboardingViewModelSignOutResidueTests`.
@MainActor
@Suite struct OnboardingVMSignOutResidueTests {
    private final class FakeResidue: SignOutResidueSurfacing, @unchecked Sendable {
        let line: String
        private let onRetry: @Sendable () -> (any SignOutResidueSurfacing)?
        private let lock = NSLock()
        private var _retries = 0
        var retries: Int { lock.withLock { _retries } }

        init(_ line: String, onRetry: @escaping @Sendable () -> (any SignOutResidueSurfacing)? = { nil }) {
            self.line = line
            self.onRetry = onRetry
        }

        func retry() -> (any SignOutResidueSurfacing)? {
            lock.withLock { _retries += 1 }
            return onRetry()
        }
    }

    private static let line = "Signed out, but 1 item(s) of your data could not be removed."

    @Test func theHandoverMovesTheResidueOffTheSession() {
        let vm = OnboardingVM()
        let session = SessionState()
        session.signOutResidue = FakeResidue(Self.line)

        vm.takeSignOutResidue(from: session)

        #expect(vm.signOutResidue?.line == Self.line)
        #expect(session.signOutResidue == nil)
        #expect(vm.errorMessage == nil, "the residue has its own view, never error-message")
    }

    /// A clean erase's handover closes a view a previous one left up.
    @Test func aCleanHandoverClosesTheView() {
        let vm = OnboardingVM()
        vm.signOutResidue = FakeResidue(Self.line)
        vm.takeSignOutResidue(from: SessionState())
        #expect(vm.signOutResidue == nil)
    }

    @Test func aRetryThatFinishesTheEraseClosesTheView() async {
        let vm = OnboardingVM()
        vm.signOutResidue = FakeResidue(Self.line)
        await vm.retrySignOutResidue()
        #expect(vm.signOutResidue == nil)
    }

    /// The retry's refusal beside a sibling window replaces the line in place.
    @Test func aRefusedRetryKeepsTheViewWithItsOwnLine() async {
        let vm = OnboardingVM()
        let blocked = "Still there — close the other Fauna window, then press Remove Again."
        vm.signOutResidue = FakeResidue(Self.line, onRetry: { FakeResidue(blocked) })
        await vm.retrySignOutResidue()
        #expect(vm.signOutResidue?.line == blocked)
    }

    /// Presses are serialized, never dropped: the second runs over what the
    /// first left.
    @Test func twoPressesRunInTurnOverWhatTheFirstLeft() async {
        let vm = OnboardingVM()
        let second = FakeResidue(Self.line)
        let first = FakeResidue(Self.line, onRetry: { second })
        vm.signOutResidue = first

        async let press1: Void = vm.retrySignOutResidue()
        async let press2: Void = vm.retrySignOutResidue()
        _ = await (press1, press2)

        #expect(first.retries == 1)
        #expect(second.retries == 1, "the second press ran over the first's result")
        #expect(vm.signOutResidue == nil)
    }

    @Test func theLaunchRecheckPaintsOnlyWhatIsStillLeft() async {
        let vm = OnboardingVM()
        await vm.recheckSignOutResidueAtLaunch { nil }
        #expect(vm.signOutResidue == nil)

        await vm.recheckSignOutResidueAtLaunch { FakeResidue(Self.line) }
        #expect(vm.signOutResidue?.line == Self.line)
    }

    /// A sign-out that landed while the launch re-check ran recorded the newer
    /// statement; the re-check never paints over it.
    @Test func theLaunchRecheckNeverOverwritesANewerSignOut() async {
        let vm = OnboardingVM()
        let newer = FakeResidue("newer")
        vm.signOutResidue = newer
        await vm.recheckSignOutResidueAtLaunch { FakeResidue("older") }
        #expect(vm.signOutResidue?.line == "newer")
    }
}
