import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit tests for the backup-destination management VM logic — the
// client-side parts that don't need a live nest round-trip: the display-name /
// URL-host label wiring (through the shared `backupDestinationLabel` FFI fn), the
// edit-different-nest error → i18n mapping, and the inline add/edit/remove dialog
// state transitions. The full UI round-trip (dispatch → FFI → persisted list →
// rows) is the harness e2e; these pin the logic so a flow regression fails fast
// WITHOUT the (slow, wedge-prone) e2e harness. The host-derivation itself now
// lives in shared Rust (`fauna_core::format::backup_destination_label`, tested in
// `libs/fauna-ffi/src/backup_destinations.rs::label_prefers_name_else_host`);
// this file only pins the apple VM wiring + the FFI `EDIT_DIFFERENT_NEST_ERR`
// contract.

// Both fixtures carry the client-device kind's columns explicitly (backups.md
// § Third destination kind). A `nest` row is what every case here seeds, so they
// take the shape a pre-discriminator row decodes to: `kind: "nest"` and neither
// optional column — never a client-device row's shape by accident.
private func dest(_ id: String, _ url: String, _ name: String?) -> FfiBackupDestinationView {
    FfiBackupDestinationView(
        destinationId: id, destinationNestUrl: url, displayName: name,
        kind: "nest", custodianDeviceId: nil, capacityCapBytes: nil)
}

// `heldBytes` / `capState` / `auditState` / `lastAuditPassedAt` are all
// client-device columns: nil on a nest row. nil `capState` is also what the
// shared `backupUsageLabel` reads as "never checked in" — deliberately not a
// `CAP_STATE_OK` stand-in, which would assert a healthy verdict nothing
// reported. nil `auditState` reads the same way through
// `backupSelfAuditIsAlerting`: *not yet audited*, never *passing* and never
// *failing* — an `AUDIT_STATE_OK` stand-in here would let a fixture assert a
// verdict no custodian ever sent.
private func status(_ id: String, lastUpload: UInt64?, backlog: UInt32) -> FfiBackupDestinationStatus {
    FfiBackupDestinationStatus(
        destinationId: id, lastUploadTime: lastUpload, backlogCount: backlog,
        heldBytes: nil, capState: nil, auditState: nil, lastAuditPassedAt: nil)
}

@Test @MainActor func labelWiresThroughSharedFnNameElseHost() {
    // The VM wires `display_name` / `destination_nest_url` into the shared
    // `backupDestinationLabel` fn and returns its result: name-if-nonempty, else
    // the URL host (the host-derivation itself is tested in shared Rust).
    let vm = BackupDestinationsVM()
    #expect(vm.label(for: dest("a", "https://aunt.example.com/", "Aunt's nest")) == "Aunt's nest")
    // nil display_name ⇒ derive the label from the URL host (a destination saved without a name).
    #expect(vm.label(for: dest("b", "https://b.example.com/", nil)) == "b.example.com")
    // Blank name ⇒ also fall back (the FFI rides a blank name as nil, but be defensive).
    #expect(vm.label(for: dest("c", "https://c.example.com:9000/", "")) == "c.example.com")
}

@Test @MainActor func openAddFormResetsAndReveals() {
    let vm = BackupDestinationsVM()
    vm.urlText = "stale"
    vm.nameText = "stale"
    vm.openAddForm()
    #expect(vm.formVisible)
    #expect(vm.editingId == nil)
    #expect(vm.urlText == "")
    #expect(vm.nameText == "")
    #expect(vm.formTitle == L.backups.backupDestinationFormAddTitle)
}

@Test @MainActor func openEditFormPrefillsFromDestination() {
    let vm = BackupDestinationsVM()
    vm.openEditForm(dest("xyz", "https://x.example.com/", "X box"))
    #expect(vm.formVisible)
    #expect(vm.editingId == "xyz")
    #expect(vm.urlText == "https://x.example.com/")
    #expect(vm.nameText == "X box")
    #expect(vm.formTitle == L.backups.backupDestinationFormEditTitle)
}

@Test @MainActor func openEditFormUsesEmptyNameForNilDisplay() {
    let vm = BackupDestinationsVM()
    vm.openEditForm(dest("xyz", "https://x.example.com/", nil))
    #expect(vm.nameText == "")
    #expect(vm.editingId == "xyz")
}

@Test @MainActor func cancelFormHidesAndClearsEditing() {
    let vm = BackupDestinationsVM()
    vm.openEditForm(dest("xyz", "https://x.example.com/", "X"))
    vm.cancelForm()
    #expect(!vm.formVisible)
    #expect(vm.editingId == nil)
}

@Test @MainActor func armRemoveSetsTargetAndHidesForm() {
    let vm = BackupDestinationsVM()
    vm.openAddForm()
    vm.armRemove("rid")
    #expect(vm.removingId == "rid")
    // Opening the remove-confirm closes the add/edit form (one dialog at a time).
    #expect(!vm.formVisible)
}

@Test @MainActor func cancelRemoveClearsTarget() {
    let vm = BackupDestinationsVM()
    vm.armRemove("rid")
    vm.cancelRemove()
    #expect(vm.removingId == nil)
}

@Test @MainActor func editDifferentNestSentinelMapsToLocalizedGuidance() {
    // The FFI throws FfiError.General carrying the stable, locale-agnostic token;
    // the VM maps it to the localized "remove + add the new one" guidance.
    let vm = BackupDestinationsVM()
    let err = FfiError.General(msg: "backup-destination-edit-different-nest")
    #expect(vm.mapError(err) == L.backups.backupDestinationEditDifferentNest)
}

@Test @MainActor func otherErrorsPassThroughAsDescription() {
    let vm = BackupDestinationsVM()
    let err = FfiError.General(msg: "connect: connection refused")
    #expect(vm.mapError(err) != L.backups.backupDestinationEditDifferentNest)
}

// the non-sentinel fallback now routes through
// `DisplayError.message` instead of painting `"\(error)"` — a cancellation
// (nothing new here to say) must not surface at all.
@Test @MainActor func aCancelledCallMapsToNothing() {
    let vm = BackupDestinationsVM()
    #expect(vm.mapError(CancellationError()) == nil)
}

@Test @MainActor func emptyListRendersPlaceholderState() {
    let vm = BackupDestinationsVM()
    #expect(vm.isEmpty)
}

// MARK: - Live per-destination status text mapping (`backups.md` § Per-destination
// status read). The pure status → i18n mapping; the live FFI read itself is the
// harness e2e. None / no-read ⇒ the "never" / "0 queued" baseline.

@Test @MainActor func lastUploadTextIsNeverWhenNoStatusOrNoUpload() {
    // No read yet for this destination, or a destination with no upload yet — both
    // render the "never" baseline (uniform with linux until an upload coordinator runs).
    #expect(BackupDestinationsVM.lastUploadText(nil) == L.backups.backupDestinationLastUploadNever)
    #expect(BackupDestinationsVM.lastUploadText(status("a", lastUpload: nil, backlog: 3))
        == L.backups.backupDestinationLastUploadNever)
}

@Test @MainActor func lastUploadTextRendersTimestampThroughSharedFormatter() {
    // A real `last_upload_time` (unix seconds) renders through the shared
    // relative-time formatter (epoch ms) — NOT the "never" baseline.
    let secs: UInt64 = 1_700_000_000
    let mapped = BackupDestinationsVM.lastUploadText(status("a", lastUpload: secs, backlog: 0))
    #expect(mapped == L.backups.backupDestinationLastUpload(
        when: ValueFormat.relativeTime(thenMs: Int64(secs) * 1000)))
    #expect(mapped != L.backups.backupDestinationLastUploadNever)
}

@Test @MainActor func backlogTextRendersCountWithZeroDefault() {
    #expect(BackupDestinationsVM.backlogText(nil)
        == L.backups.backupDestinationBacklog(count: "0"))
    #expect(BackupDestinationsVM.backlogText(status("a", lastUpload: nil, backlog: 5))
        == L.backups.backupDestinationBacklog(count: "5"))
}

@Test @MainActor func perRowHelpersBaselineWhenNoStatusLoaded() {
    // With no statuses map populated (fresh VM, read not yet done), the per-row
    // helpers fall back to the baseline — the rows never show a blank/scary value.
    let vm = BackupDestinationsVM()
    let d = dest("a", "https://a.example.com/", "A")
    #expect(vm.lastUploadText(for: d) == L.backups.backupDestinationLastUploadNever)
    #expect(vm.backlogText(for: d) == L.backups.backupDestinationBacklog(count: "0"))
}

// MARK: - Reclaim this device's copy (backups.md § Manage backup destinations)

/// A client-device row shaped the way enrollment writes it: the kind AND a
/// `custodian_device_id`, which is what the shared `row_is_a_client_device`
/// reads. A `client-device` row *without* that id is deliberately not a client
/// device — the case `clientDeviceRowWithNoDeviceIdOffersNoOptIn` pins.
private func clientDest(_ id: String, deviceId: String?) -> FfiBackupDestinationView {
    FfiBackupDestinationView(
        destinationId: id, destinationNestUrl: "", displayName: "This Mac",
        kind: destinationKindClientDevice(), custodianDeviceId: deviceId, capacityCapBytes: nil)
}

// MARK: - Client-device audit surface (`ui/backups.md` § Audit-alert surface
// → *The client-device arm*, row 151). Mirrors linux's five reference tests
// (`views/backups/destinations.rs`) exactly — the shared predicate/formatter
// pins live shared-Rust side; these pin only this VM's dispatch + wiring.

private func auditStatus(_ id: String, auditState: String?, lastAuditPassedAt: UInt64?) -> FfiBackupDestinationStatus {
    FfiBackupDestinationStatus(
        destinationId: id, lastUploadTime: nil, backlogCount: 0,
        heldBytes: nil, capState: nil, auditState: auditState, lastAuditPassedAt: lastAuditPassedAt)
}

/// A custodian that has never self-audited renders ABSENCE, not a verdict —
/// reading silence as a pass would render an unverified copy as verified;
/// reading it as a failure would raise a fleet-wide false data-loss alarm
/// for every custodian that has not self-audited yet.
@Test @MainActor func selfAuditTextAbsentRendersNotYet() {
    #expect(BackupDestinationsVM.selfAuditText(nil) == L.backups.backupDestinationLastSelfAuditNever)
    #expect(BackupDestinationsVM.selfAuditText(auditStatus("d1", auditState: nil, lastAuditPassedAt: nil))
        == L.backups.backupDestinationLastSelfAuditNever)
}

/// The custodian's cell never borrows the owner-side wording — rendering
/// "Last checked" over a self-report would let it wear the words of an
/// independent verification it never received.
@Test @MainActor func selfAuditTextNeverWearsTheOwnerSideWording() {
    let now = UInt64(Date().timeIntervalSince1970)
    let text = BackupDestinationsVM.selfAuditText(
        auditStatus("d1", auditState: "ok", lastAuditPassedAt: now))
    #expect(text != L.backups.backupDestinationLastSelfAuditNever)
    #expect(text.hasPrefix("Self-checked:"))
    #expect(!text.contains("Last checked"))
}

/// A reported failure is loud — the only failure signal that exists for a
/// kind the owner cannot sample.
@Test @MainActor func aCustodianReportingItsOwnFailureIsFlaggedForABanner() {
    let d = clientDest("d1", deviceId: "dev1")
    // Deliberately stale: the last time it PASSED. A failure never advances
    // that clock, so a flagged row must not read as fresh.
    let status = auditStatus("d1", auditState: "failed", lastAuditPassedAt: 1_700_000_000)
    let statuses = ["d1": status]
    let flagged = BackupDestinationsVM.selfReportedAlertTexts(
        destinations: [d], statuses: statuses, label: { $0.displayName ?? "" })
    #expect(flagged.count == 1)

    // The row must not read as fresh: it renders the stale PASSED time, not
    // "checked seconds ago" and not "not yet". Red-verified against
    // PROBE-71-A (flipping the fixture above to `now`). The expected date is
    // computed the same way `relativeTimeMatchesSharedRustFormatter` does, so
    // this stays timezone/locale-stable rather than pinning a literal year.
    let expectedDate = DateFormatter.localizedString(
        from: Date(timeIntervalSince1970: 1_700_000_000), dateStyle: .short, timeStyle: .none)
    let auditTime = BackupDestinationsVM.selfAuditText(status)
    #expect(auditTime.hasPrefix("Self-checked:"))
    #expect(auditTime.contains(expectedDate))
    #expect(auditTime != L.backups.backupDestinationLastSelfAuditNever)
}

/// An unrecognised verdict a NEWER client wrote stays quiet — the
/// conservative direction is the shared predicate's, not each app's.
@Test @MainActor func anUnrecognisedReportedVerdictStaysQuiet() {
    let d = clientDest("d1", deviceId: "dev1")
    let statuses = ["d1": auditStatus("d1", auditState: "degraded-in-some-newer-way", lastAuditPassedAt: nil)]
    let flagged = BackupDestinationsVM.selfReportedAlertTexts(
        destinations: [d], statuses: statuses, label: { $0.displayName ?? "" })
    #expect(flagged.isEmpty)
}

/// A nest row is untouched by all of the above: its cell still carries this
/// client's own independent check, dispatched by kind rather than a
/// re-derived guess.
@Test @MainActor func auditCellTextDispatchesOnKind() {
    let nest = dest("d1", "https://a.example.com/", nil)
    #expect(BackupDestinationsVM.auditCellText(nest, status: nil, auditRow: nil)
        == L.backups.backupDestinationLastAuditNever)

    let custodian = clientDest("d2", deviceId: "dev1")
    let status = auditStatus("d2", auditState: "ok", lastAuditPassedAt: UInt64(Date().timeIntervalSince1970))
    let custodianText = BackupDestinationsVM.auditCellText(custodian, status: status, auditRow: nil)
    #expect(custodianText != L.backups.backupDestinationLastAuditNever)
}

/// Records the ORDER of the two sides' calls — the load-bearing half of the
/// remove-with-opt-in contract (reclaim strictly after a landed deregister).
@MainActor private final class CallLog {
    var calls: [String] = []
}

@MainActor private final class RecordingCustodian: CustodianStoreAccess {
    let log: CallLog
    var bytes: UInt64 = 0
    var stillHosting = false
    var footprintThrows = false

    init(log: CallLog) { self.log = log }

    func custodianStoreFootprint() async throws -> FfiCustodianStoreInfo {
        log.calls.append("footprint")
        if footprintThrows { throw APIError.ffiError("no store read") }
        return FfiCustodianStoreInfo(generations: 0, files: 2, bytes: bytes)
    }

    func reclaimCustodianStore() async throws -> FfiCustodianReclaimOutcome {
        log.calls.append("reclaim")
        return FfiCustodianReclaimOutcome(
            stillHosting: stillHosting, freedFiles: 2, freedBytes: bytes)
    }

    var reseedResult = FfiReseedResult(
        stopped: nil, resultLines: [], isWhole: false, reenrollError: nil)

    func reseedCustodianStore() async throws -> FfiReseedResult {
        log.calls.append("reseed")
        return reseedResult
    }
}

/// Stands in for the one nest call `confirmRemove` makes. Subclassed rather than
/// protocol-injected because `APIClient` is the seam every other method on this
/// VM already takes, and `@testable` lets the override stand.
private final class RecordingAPI: APIClient {
    let log: CallLog
    init(log: CallLog) {
        self.log = log
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func removeBackupDestination(id: String) async throws -> [FfiBackupDestinationView] {
        await MainActor.run { log.calls.append("remove:\(id)") }
        return []
    }

    /// The page's re-list after a landed restore — recorded, and empty, so a
    /// test can see it happened without a live nest.
    override func listBackupDestinations() async throws -> [FfiBackupDestinationView] {
        await MainActor.run { log.calls.append("list") }
        return []
    }

    /// The at-rest re-read after a Keep: the kept row, now lowered.
    override func keepBackupDestination(id: String) async throws -> [FfiBackupDestinationView] {
        await MainActor.run { log.calls.append("keep:\(id)") }
        var kept = dest(id, "https://a.example.com", nil)
        kept.unattested = false
        return [kept]
    }
}

/// Keep dispatches the shared keep export for THAT row and renders the
/// returned re-read — the mark stops painting because the row came back
/// lowered from rest, never because the VM flipped it optimistically. No
/// confirm step sits in front of it (non-destructive, re-decidable).
@Test @MainActor func keepDispatchesForTheRowAndRendersTheReRead() async {
    let log = CallLog()
    let vm = wired(log, custodian: nil)
    var raised = dest("d1", "https://a.example.com", nil)
    raised.unattested = true
    vm.seedDestinationsForTest([raised])

    await vm.keep("d1")

    #expect(log.calls == ["keep:d1"])
    #expect(vm.destinations.map(\.unattested) == [false])
    #expect(vm.errorMessage == nil)
}

@MainActor private func wired(
    _ log: CallLog, deviceId: String? = "dev-1", custodian: RecordingCustodian?
) -> BackupDestinationsVM {
    let vm = BackupDestinationsVM()
    vm.configure(api: RecordingAPI(log: log), deviceId: deviceId, custodianStore: custodian)
    return vm
}

@Test @MainActor func orphanedVerdictPaintsOnlyWhenNoRowClaimsThisDevice() async {
    // The row's render rule, over the SHARED predicate: bytes held and no
    // destination row naming this device ⇒ paint; a row that names it ⇒ never.
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 4096
    let vm = wired(log, custodian: custodian)

    await vm.refreshOrphanedStore()
    #expect(vm.orphanedStoreBytes == 4096)

    // A live client-device row naming this device claims those bytes — the
    // offer must vanish, or the page invites deleting live custody. (This is
    // exactly what `custodian_assignment_for(..) == nil` would get wrong.)
    vm.seedDestinationsForTest([clientDest("d1", deviceId: "dev-1")])
    await vm.refreshOrphanedStore()
    #expect(vm.orphanedStoreBytes == nil)

    // A row naming a DIFFERENT device claims nothing here.
    vm.seedDestinationsForTest([clientDest("d2", deviceId: "other-device")])
    await vm.refreshOrphanedStore()
    #expect(vm.orphanedStoreBytes == 4096)
}

@Test @MainActor func cannotTellNeverOffersTheDelete() async {
    // Every failure path lands on "not orphaned". The gesture behind this row
    // deletes the owner's ONLY offline copy, so not knowing must never paint it.
    let log = CallLog()

    // (a) No seam configured at all (a shell that never wired one).
    let noSeam = wired(log, custodian: nil)
    await noSeam.refreshOrphanedStore()
    #expect(noSeam.orphanedStoreBytes == nil)

    // (b) No device id yet — the join has no left-hand side.
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 4096
    let noDevice = wired(log, deviceId: nil, custodian: custodian)
    await noDevice.refreshOrphanedStore()
    #expect(noDevice.orphanedStoreBytes == nil)

    // (c) The read itself failed — no agent running, a refusing agent, no store.
    custodian.footprintThrows = true
    let vm = wired(log, custodian: custodian)
    await vm.refreshOrphanedStore()
    #expect(vm.orphanedStoreBytes == nil)

    // (d) The store is empty — nothing to offer back.
    custodian.footprintThrows = false
    custodian.bytes = 0
    await vm.refreshOrphanedStore()
    #expect(vm.orphanedStoreBytes == nil)
}

@Test @MainActor func armReclaimRefusesWithoutAPaintedVerdict() {
    // The keyboard / stale-registry backstop: no verdict ⇒ the gesture does
    // nothing, rather than running because the button happened to be reachable.
    let vm = BackupDestinationsVM()
    vm.armReclaim()
    #expect(!vm.reclaiming)
}

@Test @MainActor func reclaimRunsStrictlyAfterALandedRemoval() async {
    // The ordering the opt-in turns on: deregister first, reclaim second.
    // Reclaiming first would destroy the owner's only offline copy while the
    // destination row still stood — and a failed removal would then leave a row
    // claiming a copy that no longer exists.
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 8192
    let vm = wired(log, custodian: custodian)
    vm.seedDestinationsForTest([clientDest("d1", deviceId: "dev-1")])

    vm.armRemove("d1")
    vm.removeReclaim = true
    await vm.confirmRemove()

    let removeIdx = log.calls.firstIndex(of: "remove:d1")
    let reclaimIdx = log.calls.firstIndex(of: "reclaim")
    #expect(removeIdx != nil)
    #expect(reclaimIdx != nil)
    #expect((removeIdx ?? 0) < (reclaimIdx ?? 0))
}

@Test @MainActor func removalWithoutTheOptInKeepsTheCopy() async {
    // § 3c-ii: removing a client-device destination deliberately does NOT delete
    // the local sealed store. Only the ticked checkbox frees it — and the bytes
    // it keeps are exactly what the orphaned row then offers back.
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 8192
    let vm = wired(log, custodian: custodian)
    vm.seedDestinationsForTest([clientDest("d1", deviceId: "dev-1")])

    vm.armRemove("d1")
    await vm.confirmRemove()

    #expect(!log.calls.contains("reclaim"))
    #expect(vm.orphanedStoreBytes == 8192)
}

@Test @MainActor func theOptInNeverSurvivesItsOwnDialog() async {
    // A tick is an intent about THIS removal. Surviving a cancel — or the next
    // arm — would free a copy nobody offered up in that gesture.
    let log = CallLog()
    let vm = wired(log, custodian: RecordingCustodian(log: log))

    vm.armRemove("d1")
    vm.removeReclaim = true
    vm.cancelRemove()
    #expect(!vm.removeReclaim)

    vm.removeReclaim = true
    vm.armRemove("d2")
    #expect(!vm.removeReclaim)

    vm.removeReclaim = true
    await vm.confirmRemove()
    #expect(!vm.removeReclaim)
}

@Test @MainActor func stillHostingIsSurfacedEvenThoughTheCallSucceeded() async {
    // Nothing was deleted. Reporting success would retire the row in the user's
    // mind while the bytes remain — so it reads as the shared refusal sentence,
    // and the row stays standing, which IS the retry.
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 8192
    custodian.stillHosting = true
    let vm = wired(log, custodian: custodian)
    await vm.refreshOrphanedStore()
    vm.armReclaim()
    #expect(vm.reclaiming)

    await vm.confirmReclaim()
    #expect(!vm.reclaiming)
    #expect(vm.errorMessage == L.backups.backupReclaimStillHosting)
    #expect(vm.orphanedStoreBytes == 8192)
}

@Test @MainActor func aLandedReclaimRetiresTheRow() async {
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 8192
    let vm = wired(log, custodian: custodian)
    await vm.refreshOrphanedStore()
    vm.armReclaim()

    // The store is empty after the reclaim, which is what the re-read sees.
    custodian.bytes = 0
    await vm.confirmReclaim()
    #expect(vm.errorMessage == nil)
    #expect(vm.orphanedStoreBytes == nil)
}

@Test @MainActor func clientDeviceRowWithNoDeviceIdOffersNoOptIn() {
    // The checkbox's render rule is the shared `row_is_a_client_device`, not the
    // kind string: a `client-device` row with no `custodian_device_id` cannot be
    // driven by anything, so there is no copy to offer to free.
    let vm = BackupDestinationsVM()
    #expect(vm.rowIsAClientDevice(clientDest("d1", deviceId: "dev-1")))
    #expect(!vm.rowIsAClientDevice(clientDest("d2", deviceId: nil)))
    #expect(!vm.rowIsAClientDevice(dest("d3", "https://a.example.com/", "A nest")))
}

@Test @MainActor func removingDestinationResolvesTheArmedRow() {
    // What the remove dialog reads to decide whether to offer the opt-in.
    let vm = BackupDestinationsVM()
    vm.seedDestinationsForTest([clientDest("d1", deviceId: "dev-1")])
    #expect(vm.removingDestination == nil)
    vm.armRemove("d1")
    #expect(vm.removingDestination?.destinationId == "d1")
    vm.armRemove("gone")
    #expect(vm.removingDestination == nil)
}

@Test @MainActor func orphanedRowTextComesFromTheSharedSentence() {
    // The row is the only place a user is told what deleting these bytes costs,
    // so the copy is shared — this composes nothing of its own, and the size
    // resolves before it lands in the `{held}` slot.
    let vm = BackupDestinationsVM()
    let display = orphanedStoreLabel(heldBytes: 4096)
    let expected = renderLocalizedText(display.label)
        .replacingOccurrences(of: "{held}", with: renderLocalizedText(display.held))
    #expect(vm.orphanedStoreText(held: 4096) == expected)
    #expect(!vm.orphanedStoreText(held: 4096).contains("{held}"))
}

// MARK: - Restore after losing the nest (`backups.md` § Restore after losing the nest)

@Test @MainActor func reseedPaintsOnTheOrphanedStoreAndOnThisDevicesOwnRowOnly() async {
    // The shared `backupReseedRows` over the VM's cached state: an orphaned store
    // carries the button; so does this device's own custodian row; another
    // device's row and a nest row never do (the nest-held leg is not built).
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 4096
    let vm = wired(log, custodian: custodian)

    await vm.refreshOrphanedStore()
    #expect(vm.reseedOnOrphanedStore)

    let mine = clientDest("mine", deviceId: "dev-1")
    let theirs = clientDest("theirs", deviceId: "dev-2")
    let offSite = dest("off-site", "https://b.example.com/", nil)
    vm.seedDestinationsForTest([mine, theirs, offSite])
    await vm.refreshOrphanedStore()
    #expect(!vm.reseedOnOrphanedStore)
    #expect(vm.reseedOnRow(mine))
    #expect(!vm.reseedOnRow(theirs))
    #expect(!vm.reseedOnRow(offSite))

    // A shell with no store seam offers the gesture nowhere.
    let noSeam = wired(log, custodian: nil)
    noSeam.seedDestinationsForTest([mine])
    #expect(noSeam.reseedRows.isEmpty)
}

@Test @MainActor func armReseedRefusesWithoutAPaintedRow() {
    let vm = wired(CallLog(), custodian: RecordingCustodian(log: CallLog()))
    vm.armReseed()
    #expect(!vm.reseedConfirming)
}

@Test @MainActor func aWholeRestorePaintsTheSharedLinesThenReLists() async {
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 4096
    let lines = [
        LocalizedText(key: "backups.reseed_result_whole", args: [:]),
        LocalizedText(key: "backups.reseed_set_restored",
                      args: ["set": "backups.reseed_set_mail", "count": "1"]),
    ]
    custodian.reseedResult = FfiReseedResult(
        stopped: nil, resultLines: lines, isWhole: true, reenrollError: nil)
    let vm = wired(log, custodian: custodian)
    await vm.refreshOrphanedStore()
    vm.armReseed()
    #expect(vm.reseedConfirming)

    await vm.confirmReseed()

    #expect(!vm.reseedConfirming)
    #expect(!vm.reseedRunning)
    // Resolved NESTED: the set name inside a line is itself a key.
    #expect(vm.reseedResultText == lines.map(renderLocalizedTextNested).joined(separator: "\n"))
    #expect(vm.reseedResultText?.contains("backups.") == false)
    #expect(vm.errorMessage == nil)
    // The re-list follows the ceremony, never precedes it.
    let reseedIdx = log.calls.firstIndex(of: "reseed")
    let listIdx = log.calls.firstIndex(of: "list")
    #expect(reseedIdx != nil && listIdx != nil && reseedIdx! < listIdx!)
}

@Test @MainActor func aStoppedRestoreRendersOnTheErrorAndLeavesNoResult() async {
    let log = CallLog()
    let custodian = RecordingCustodian(log: log)
    custodian.bytes = 4096
    custodian.reseedResult = FfiReseedResult(
        stopped: "the agent restarted", resultLines: [], isWhole: false, reenrollError: nil)
    let vm = wired(log, custodian: custodian)
    await vm.refreshOrphanedStore()
    vm.armReseed()

    await vm.confirmReseed()

    #expect(vm.reseedResultText == nil)
    #expect(vm.errorMessage == L.backups.backupReseedFailed(reason: "the agent restarted"))
    #expect(!log.calls.contains("list"))
}

@Test @MainActor func aFailedReenrollmentKeepsTheResultAndSaysWhatDidNotHappen() {
    // The data IS back: the result stays, and the error names the one thing
    // that did not happen. No re-list here — its landing would clear the message.
    let vm = BackupDestinationsVM()
    vm.applyReseed(FfiReseedResult(
        stopped: nil,
        resultLines: [LocalizedText(key: "backups.reseed_result_whole", args: [:])],
        isWhole: true, reenrollError: "offline"))
    #expect(vm.reseedResultText == L.backups.reseedResultWhole)
    #expect(vm.errorMessage == L.backups.backupReseedReenrollFailed(reason: "offline"))
}
