import Testing
import FaunaKit
@testable import FaunaMacOSLib

// Unit tests for `LocationsModel.reconcile()` — the device-local folder↔folder
// binding adoption/pending-bind retry logic post the A4 agent cutover. These pin the
// residual availability faces the 2026-07-19 A4 review found, which were previously UNPINNED: the
// e2e `sync_inject_locations` seam bypasses `reconcile()` entirely, and the tier_3
// agent-process harness pins only the AGENT side of the bind sequence. The fake
// substitutes for the agent via the `LocationControlChannel` seam (the macOS peer of
// windows' `InMemorySyncPipeClient`), so the reconcile behavior is deterministic
// with no live agent, socket, or launchctl. The convergence loop's rising-edge
// firing itself is pinned in Rust (`fauna_ipc::convergence` + the
// `fauna-ffi` observer-forwarding test); here we pin what the Swift edge re-drives.

/// In-memory `LocationControlChannel` fake. Driven sequentially from the model's
/// `@MainActor` `await`s (one call at a time, awaited to completion), hence
/// `@unchecked Sendable` — there is no concurrent access to pin against.
final class FakeLocationControlChannel: LocationControlChannel, @unchecked Sendable {
    /// Whether the agent's socket answers this tick. Flip to simulate the
    /// just-spawned agent coming up (the reachable edge).
    var reachable: Bool
    /// Folder names whose `bindLocation` throws even when reachable (a
    /// rejected bind).
    var rejectFolders: Set<String> = []
    /// Folder names the fake reports as access-revoked (parked) —
    /// `FfiAgentLocation.accessRevoked`.
    var revokedFolders: Set<String> = []
    private(set) var bound: [LocationBinding] = []
    private(set) var bindCallCount = 0
    /// `"bind:<folder>"` appended on every successful `bindLocation` —
    /// the ordering pin folds this onto a caller-side log to observe
    /// the content-key push and the bind push on one shared timeline.
    private(set) var bindOrder: [String] = []
    /// The `folderId` each successful bind carried, by folder name — pins that
    /// `LocationsModel.add`'s ref reaches the channel unchanged.
    private(set) var boundFolderIds: [String: String] = [:]
    /// Canned `applyHeldDeletes` replies, by folder — row 123. Absent folder
    /// throws `.unreachable`, mirroring an agent that can no longer see the set.
    var heldDeletesReplies: [String: FfiHeldDeletesApplied] = [:]
    private(set) var appliedHeldDeletesFolders: [String] = []

    init(reachable: Bool) { self.reachable = reachable }

    enum FakeError: Error { case unreachable, rejected }

    func bindLocation(path: String, folder: String, folderId: String) async throws {
        bindCallCount += 1
        guard reachable else { throw FakeError.unreachable }
        if rejectFolders.contains(folder) { throw FakeError.rejected }
        bound.removeAll { $0.folderId == folderId } // one folder ↔ one set
        bound.append(LocationBinding(path: path, folder: folder, folderId: folderId))
        boundFolderIds[folder] = folderId
        bindOrder.append("bind:\(folder)")
    }

    func unbindLocation(path: String) async throws {
        guard reachable else { throw FakeError.unreachable }
        bound.removeAll { $0.path == path }
    }

    func listLocations() async throws -> [FfiAgentLocation] {
        guard reachable else { throw FakeError.unreachable }
        return bound.map {
            FfiAgentLocation(
                path: $0.path, folder: $0.folder, folderId: $0.folderId, mode: "always",
                accessRevoked: revokedFolders.contains($0.folder))
        }
    }

    func applyHeldDeletes(folder: String) async throws -> FfiHeldDeletesApplied {
        appliedHeldDeletesFolders.append(folder)
        guard let reply = heldDeletesReplies[folder] else { throw FakeError.unreachable }
        return reply
    }
}

/// (i) The race face: on a launch that just spawned the agent it isn't up when the
/// model attaches, so the single at-attach reconcile finds it unreachable. The
/// pending-bind rows must be KEPT (not blanked), and the reachable edge must re-drive
/// reconcile so the pending bind finally lands.
@Test @MainActor
func reconcileKeepsRowsWhenUnreachableThenPushesOnReachableEdge() async {
    let model = LocationsModel(testSeed: [LocationBinding(path: "/docs", folder: "docs", folderId: "local:1")])
    let fake = FakeLocationControlChannel(reachable: false)
    model.setChannelForTest(fake)

    // At-attach reconcile, agent unreachable: rows kept, nothing pushed.
    await model.reconcile()
    #expect(model.mappings.map(\.folder) == ["docs"])
    #expect(fake.bound.isEmpty)

    // Agent becomes reachable → the edge re-fires reconcile → pending bind lands.
    fake.reachable = true
    await model.reconcile()
    #expect(fake.bound.map(\.folder) == ["docs"])
    #expect(model.mappings.map(\.folder) == ["docs"])
}

/// (ii) Face (b): a pending row whose `bindLocation` errors must stay rendered
/// (the union fix), not vanish when we adopt the agent's shorter truth — otherwise
/// the user loses sight of it and the next reconcile can't retry it.
@Test @MainActor
func reconcileKeepsAFailedBindRowRendered() async {
    let model = LocationsModel(testSeed: [
        LocationBinding(path: "/docs", folder: "docs", folderId: "local:1"),
        LocationBinding(path: "/photos", folder: "photos", folderId: "local:2"),
    ])
    let fake = FakeLocationControlChannel(reachable: true)
    fake.rejectFolders = ["photos"] // the photos bind will error
    model.setChannelForTest(fake)

    await model.reconcile()
    // docs pushed + adopted; photos failed but must remain rendered for retry.
    #expect(fake.bound.map(\.folder) == ["docs"])
    #expect(Set(model.mappings.map(\.folder)) == ["docs", "photos"])
}

/// (iii) Face (a): a binding the user makes while the agent is down lives only in
/// memory (nothing writes `folder-map.json` anymore) — it must survive and push on
/// the reachable edge rather than being silently lost.
@Test @MainActor
func addWhileAgentDownSurvivesAndPushesOnReachableEdge() async {
    let model = LocationsModel(testSeed: [])
    let fake = FakeLocationControlChannel(reachable: false)
    model.setChannelForTest(fake)

    // User binds a folder while the agent is still spawning: the row is in memory.
    model.add(path: "/notes", folder: "notes", folderId: "local:4")
    #expect(model.mappings.map(\.folder) == ["notes"])

    // Agent comes up → edge reconcile pushes the pending binding.
    fake.reachable = true
    await model.reconcile()
    #expect(fake.bound.map(\.folder) == ["notes"])
}

/// (iv) `revokedFolders` mirrors the agent's `accessRevoked` flag, derived
/// from the SAME reconcile read as `mappings` (no extra round trip) — the
/// writer-binding fan-out's `folder-access-revoked-warning` input. The row
/// stays rendered (in `mappings`) while parked — a park is not a deletion.
@Test @MainActor
func reconcileSurfacesAccessRevokedFolders() async {
    let model = LocationsModel(testSeed: [
        LocationBinding(path: "/docs", folder: "docs", folderId: "local:1"),
        LocationBinding(path: "/shared", folder: "shared", folderId: "local:3"),
    ])
    let fake = FakeLocationControlChannel(reachable: true)
    model.setChannelForTest(fake)
    await model.reconcile() // adopt both bindings onto the agent first

    fake.revokedFolders = ["shared"]
    await model.reconcile()

    #expect(model.revokedFolders == ["shared"])
    #expect(Set(model.mappings.map(\.folder)) == ["docs", "shared"])

    // Recovery: the grant returns, the agent clears the park, the warning clears.
    fake.revokedFolders = []
    await model.reconcile()
    #expect(model.revokedFolders.isEmpty)
}

/// (v) `add`'s `folderId` reaches the agent's `bindLocation` unchanged,
/// for both ref shapes a real call site can pick: an owned row's `local:<id>`
/// and a cross-nest member row's `foreign:<channel>` (the resolution
/// semantics themselves are pinned tier_1 in the agent's `content_keys.rs`
/// (`keys_are_found_by_ref_past_a_same_named_entry_never_by_name`); this only pins
/// that the Swift plumbing does not drop or mangle the ref in transit).
@Test @MainActor
func addWithFolderIdPushesTheRefThroughReconcile() async {
    let model = LocationsModel(testSeed: [])
    let fake = FakeLocationControlChannel(reachable: true)
    model.setChannelForTest(fake)

    model.add(path: "/docs", folder: "docs", folderId: "local:42")
    await model.reconcile()
    #expect(fake.boundFolderIds["docs", default: "MISSING"] == "local:42")

    model.add(path: "/shared", folder: "shared-docs", folderId: "foreign:aabbccdd")
    await model.reconcile()
    #expect(fake.boundFolderIds["shared-docs", default: "MISSING"] == "foreign:aabbccdd")
}

/// Row 123 — `foldEngineHolds` is a MIRROR keyed by folder: only non-zero
/// entries survive into `deletesHeld`, and re-folding with the set absent (or
/// `0`) retracts it — the reading that turns off `folder-location-deletes-held`
/// / `folder-location-apply-deletes-button` (`delete-propagation.md` § A
/// wholesale-vanished folder is infrastructure failure).
@Test @MainActor
func foldEngineHoldsMirrorsNonZeroHoldsAndRetractsOnZero() {
    let model = LocationsModel(testSeed: [
        LocationBinding(path: "/docs", folder: "docs", folderId: "local:1"),
        LocationBinding(path: "/photos", folder: "photos", folderId: "local:2"),
    ])

    model.foldEngineHolds([FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3)])
    #expect(model.deletesHeld["docs"] == 3)
    #expect(model.deletesHeld["photos"] == nil)

    // The next roster no longer reports "docs" (agent restarted, or it
    // resolved) — the mirror retracts it, not sticks at 3.
    model.foldEngineHolds([])
    #expect(model.deletesHeld["docs"] == nil)

    // An explicit 0 is the same retraction, never a stored zero.
    model.foldEngineHolds([FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 5)])
    #expect(model.deletesHeld["docs"] == 5)
    model.foldEngineHolds([FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 0)])
    #expect(model.deletesHeld["docs"] == nil)
}

/// `folder-location-unreadable` — `foldEngineHolds` mirrors a set's
/// unreadable-path count exactly as it mirrors the hold: only non-zero entries
/// survive into `deletesSkippedUnreadable`, and the next fold retracts it. It is
/// INDEPENDENT of `deletesHeld` — an unreadable-only set leaves `deletesHeld`
/// empty, the reading `folder-location-apply-deletes-button` is gated on, so no
/// apply verb is ever offered for it (`delete-propagation.md` § Unreadable is
/// not absent — a status line, deliberately no action).
@Test @MainActor
func foldEngineHoldsMirrorsTheUnreadableCountApartFromTheHold() {
    let model = LocationsModel(testSeed: [
        LocationBinding(path: "/docs", folder: "docs", folderId: "local:1"),
        LocationBinding(path: "/photos", folder: "photos", folderId: "local:2"),
    ])

    // Unreadable only: the count shows, the hold (and so the apply verb) does not.
    model.foldEngineHolds([
        FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 0, deletesSkippedUnreadable: 4),
    ])
    #expect(model.deletesSkippedUnreadable["docs"] == 4)
    #expect(model.deletesSkippedUnreadable["photos"] == nil)
    #expect(model.deletesHeld["docs"] == nil)

    // Both at once are independent readings of the same set.
    model.foldEngineHolds([
        FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3, deletesSkippedUnreadable: 2),
    ])
    #expect(model.deletesHeld["docs"] == 3)
    #expect(model.deletesSkippedUnreadable["docs"] == 2)

    // An explicit 0 retracts the count (never a stored zero) without touching the hold.
    model.foldEngineHolds([
        FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3, deletesSkippedUnreadable: 0),
    ])
    #expect(model.deletesSkippedUnreadable["docs"] == nil)
    #expect(model.deletesHeld["docs"] == 3)

    // The next roster no longer reports the set — both retract.
    model.foldEngineHolds([])
    #expect(model.deletesSkippedUnreadable["docs"] == nil)
    #expect(model.deletesHeld["docs"] == nil)
}

/// The park watch (`file-sync.md` § Multi-writer shared sets → *Revocation*):
/// `foldParks` mirrors the agent's `accessRevoked` onto a row the model already
/// holds — no reconcile needed — and a re-bind's un-park clears it again.
@Test @MainActor
func foldParksMirrorsTheAgentsParkBothWays() {
    let model = LocationsModel(testSeed: [
        LocationBinding(path: "/shared", folder: "photos", folderId: "ref:photos"),
    ])
    func agentRow(_ parked: Bool) -> FfiAgentLocation {
        FfiAgentLocation(path: "/shared", folder: "photos", folderId: "ref:photos",
                         mode: "always", accessRevoked: parked)
    }
    #expect(model.revokedFolders.isEmpty)

    model.foldParks([agentRow(true)])
    #expect(model.revokedFolders == ["photos"])

    model.foldParks([agentRow(false)])
    #expect(model.revokedFolders.isEmpty)
}

/// Row 123 — `applyHeldDeletes` is deliberately NON-optimistic: nothing in
/// `deletesHeld` changes until the reply lands, and it repaints from the
/// reply's `remainingHeld`, never from the count that was showing before the
/// call (the agent re-derives what is actually missing at click time).
@Test @MainActor
func applyHeldDeletesRepaintsOnlyFromTheReply() async {
    let model = LocationsModel(testSeed: [LocationBinding(path: "/docs", folder: "docs", folderId: "local:1")])
    let fake = FakeLocationControlChannel(reachable: true)
    model.setChannelForTest(fake)
    model.foldEngineHolds([FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3)])
    #expect(model.deletesHeld["docs"] == 3)

    fake.heldDeletesReplies["docs"] = FfiHeldDeletesApplied(
        applied: 3, remainingHeld: 0, floorWasActive: true)
    await model.applyHeldDeletes(folder: "docs")

    #expect(fake.appliedHeldDeletesFolders == ["docs"])
    #expect(model.deletesHeld["docs"] == nil) // remainingHeld: 0 retracts it
}

/// A failed `applyHeldDeletes` call leaves the hold standing — there is no
/// client-side state to unwind, since nothing was updated before the reply.
@Test @MainActor
func applyHeldDeletesLeavesTheHoldStandingOnFailure() async {
    let model = LocationsModel(testSeed: [LocationBinding(path: "/docs", folder: "docs", folderId: "local:1")])
    let fake = FakeLocationControlChannel(reachable: true)
    model.setChannelForTest(fake)
    model.foldEngineHolds([FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3)])

    // No canned reply for "docs" → the fake throws.
    await model.applyHeldDeletes(folder: "docs")

    #expect(fake.appliedHeldDeletesFolders == ["docs"])
    #expect(model.deletesHeld["docs"] == 3) // unchanged — the hold still stands
}

/// `FolderSummary.folderRef` for the two row shapes `MacFolderBindingSection`
/// actually renders a binding UI for: an owned/same-nest row (`Local`) and a
/// cross-nest member row (`Foreign`) — pinning that the call site (the
/// "apple gap") supplies a non-nil ref in both cases. The `Local`/`Foreign`
/// CHOICE is shared Rust and already pinned tier_1; this only pins that this
/// call site's three fields reach it.
@Test
func folderRefIsNonNilForAnOwnedRow() {
    let owned = makeFolderSummaryFixture(id: 42, mlsGroupId: nil, homeNestUrl: nil)
    #expect(owned.folderRef == "local:42")
}

@Test
func folderRefIsNonNilForAForeignMemberRow() {
    let member = makeFolderSummaryFixture(
        id: -1,
        mlsGroupId: String(repeating: "ab", count: 32),
        homeNestUrl: "https://home.example")
    #expect(member.folderRef != nil)
    #expect(member.folderRef?.hasPrefix("foreign:") == true)
}

/// Builds a `FolderSummary` with only the three `folderRef`-relevant fields set
/// meaningfully — the rest are inert placeholders, matching the shared Rust
/// `FolderSummary::default()` fixture convention (`snapshots.rs`) this type
/// can't use directly (UniFFI records get no Swift `Default`).
private func makeFolderSummaryFixture(
    id: Int64,
    mlsGroupId: String?,
    homeNestUrl: String?
) -> FolderSummary {
    FolderSummary(
        id: id, name: "docs", retentionPolicy: nil,
        cachedSnapshotCount: 0, cachedTotalBytes: 0,
        cachedLastSnapshotAt: nil, includePaths: nil, excludePaths: nil,
        mlsGroupId: mlsGroupId, role: nil, access: nil, ownerHandle: nil,
        ownerDisplay: "", webdavEnabled: false, conflictPolicy: nil,
        webPaywallTier: nil, homeNestUrl: homeNestUrl, nestSnapshots: nil,
        nestSnapshotQuietSecs: nil,
        // The version-retention bounds pair. `0` is that bound *unset*, so
        // `(0, 0)` is the resting state — keep everything — and is what an
        // absent `version_retention` on the wire lands as
        // (`fauna_devices_machine::snapshots`, `file-versions.md` § Retention
        // ruling 1). The inert placeholder, matching the rest of this fixture.
        versionRetentionMaxVersions: 0, versionRetentionMaxAgeDays: 0,
        // Phase 4's two fields. `"private"` is the inert value, deliberately —
        // it is what an absent audience transcribes to for an
        // unbound folder, and `"public"` is never a safe placeholder: it means
        // the owner declassified the set and its content, names and paths rest
        // world-readable (`principles.md` § The user always controls their
        // data, the one deliberate exception). `websiteEnabled: false` matches
        // `webdavEnabled` above — owner-side opt-ins are off in a fixture.
        // Phase 5's content-residency field: `""` is the wire's own spelling
        // of the default (full) residency, matching `FolderSummary::default()`
        // (`fauna-devices-machine/src/snapshots.rs`) and android's identical
        // fixture convention (`FoldersContentTest.kt`).
        audience: "private", websiteEnabled: false, residency: "")
}
