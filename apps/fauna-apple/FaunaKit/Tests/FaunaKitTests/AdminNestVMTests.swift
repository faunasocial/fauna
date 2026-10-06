import Testing
@testable import FaunaKit

// Non-e2e unit tests for the admin-nest VM gating logic that needs no live
// nest / FFI round-trip. Pins the serving-port editability rule
// (nest/common.md § Serving ports): the field is editable only on a
// direct-listener nest and read-only when a Docker/cloud router fronts it
// (`setup.status.fronted_by_router == true`). Mirrors the sibling legs'
// gate tests (android `AdminNestContentTest`, windows
// `Load_*_ServingPort*`). The full UI round-trip (hydrate → render gate) is
// the harness e2e; this pins the pure rule so an inverted gate fails fast.

@Test @MainActor func servingPortEditableOnDirectListener() {
    // Default direct-listener nest (fronted_by_router == false): editable.
    #expect(AdminNestVM.servingPortEditable(isBusy: false, frontedByRouter: false) == true)
}

@Test @MainActor func servingPortReadOnlyWhenRouterFronted() {
    // Docker/cloud router-fronted nest: the field is locked read-only.
    #expect(AdminNestVM.servingPortEditable(isBusy: false, frontedByRouter: true) == false)
}

@Test @MainActor func servingPortNotEditableWhileBusy() {
    // A hydrate/save in flight disables the field regardless of fronting.
    #expect(AdminNestVM.servingPortEditable(isBusy: true, frontedByRouter: false) == false)
    #expect(AdminNestVM.servingPortEditable(isBusy: true, frontedByRouter: true) == false)
}

// Host-OS-maintenance status line (installers/vps.md § Host OS Maintenance § 4):
// the `nest-os-maintenance-status` headline is the shared `os_maintenance_status_label`
// decision resolved through the same `renderLocalizedText` → `L.lookup` pipeline
// production uses (the real UniFFI export, like `ValueFormatTests`). Pins apple to
// the shared state→key map + its priority (reboot > updates > up-to-date) so a
// per-app drift / inverted priority fails fast (the count badge + button gating
// is the harness e2e `test_host_maintenance.py`).
@Test @MainActor func osMaintenanceLabelResolvesPerState() {
    // No updates, no reboot → "OS up to date".
    #expect(AdminNestVM.osMaintenanceLabel(securityUpdates: 0, rebootPending: false)
        == L.admin.nestPage.osUpToDate)
    // Updates pending, no reboot → "Security updates pending".
    #expect(AdminNestVM.osMaintenanceLabel(securityUpdates: 3, rebootPending: false)
        == L.admin.nestPage.osUpdatesPending)
    // Reboot pending (no updates) → "Restart pending …".
    #expect(AdminNestVM.osMaintenanceLabel(securityUpdates: 0, rebootPending: true)
        == L.admin.nestPage.osRestartPending)
    // Reboot pending takes priority over a non-zero pending-update count.
    #expect(AdminNestVM.osMaintenanceLabel(securityUpdates: 5, rebootPending: true)
        == L.admin.nestPage.osRestartPending)
}

// Outside-app sign-in keys (`admin-nest-oauth-*`; authorization-server.md § The
// issuer → Two rotation arms). The four gestures' guards live in the pure
// `OauthSectionState` (the Swift twin of linux's GTK-free `OauthSectionState`),
// so a driver-forced press on a control the view paints disabled is refused by
// the same test the paint reads. The confirm fold is the real UniFFI export —
// the sentence it captures is the shared one every app shows. The walk itself
// is the harness e2e `test_admin_oauth_issuer_keys.py`.

private func signer(_ kid: String) -> FfiIssuerKeyRow {
    FfiIssuerKeyRow(kid: kid, signing: true, retiredAt: nil, servedUntil: nil)
}

private func oneKey(_ kid: String) -> FfiIssuerKeyView {
    FfiIssuerKeyView(activeKid: kid, keys: [signer(kid)],
                     retirementHorizonSecs: 1_200, rotationInFlight: false)
}

private func twoKeys() -> FfiIssuerKeyView {
    FfiIssuerKeyView(
        activeKid: "k2",
        keys: [signer("k2"),
               FfiIssuerKeyRow(kid: "k1", signing: false, retiredAt: 1_000, servedUntil: 2_200)],
        retirementHorizonSecs: 1_200,
        rotationInFlight: true)
}

@Test func oauthControlsRefuseUntilTheKeySetAnswers() {
    // Not asked yet and couldn't-find-out both hold every control dead: the
    // forced confirm could not name what it drops.
    for keys in [OauthKeysRead.unread, .failed("nope")] {
        var s = OauthSectionState()
        s.keysLoaded(keys)
        #expect(!s.controlsLive)
        // (`#expect` cannot call a mutating member itself — hence the locals.)
        let pressed = s.pressRotate()
        #expect(!pressed)
        s.arm(.issuerKey)
        #expect(s.armed == nil)
        #expect(s.status == nil && !s.inFlight)
    }
}

@Test func oauthRotateGoesInFlightAndRefusesAChainedPress() {
    var s = OauthSectionState()
    s.keysLoaded(.ready(twoKeys()))
    s.arm(.issuerKey)
    let first = s.pressRotate()
    #expect(first)
    // Disarms the confirm beside it: its key count is about to change.
    #expect(s.armed == nil)
    #expect(s.inFlight && s.status == L.admin.nestPage.oauthWorking)
    #expect(!s.controlsLive)
    // A second press while the first is out would chain a second rotation.
    let second = s.pressRotate()
    #expect(!second)
    s.arm(.sessionSecret)
    #expect(s.armed == nil)
}

@Test func oauthArmCapturesTheSharedConfirmAndOneArmAtATime() {
    var s = OauthSectionState()
    s.keysLoaded(.ready(twoKeys()))
    s.status = "an earlier verdict"
    s.arm(.issuerKey)
    #expect(s.armed?.arm == .issuerKey)
    #expect(s.status == nil)
    #expect(s.armed.map { renderLocalizedText($0.confirm.summary) }
        == L.admin.nestPage.oauthForceRotateConfirmMany(count: "2"))
    // Arming the sibling replaces it.
    s.arm(.sessionSecret)
    #expect(s.armed?.arm == .sessionSecret)
    #expect(s.armed.map { renderLocalizedText($0.confirm.summary) }
        == L.admin.nestPage.oauthSecretForceRotateConfirm)
    // A re-read landing while armed keeps the cost the admin already read.
    s.arm(.issuerKey)
    s.keysLoaded(.ready(oneKey("k3")))
    #expect(s.armed.map { renderLocalizedText($0.confirm.summary) }
        == L.admin.nestPage.oauthForceRotateConfirmMany(count: "2"))
    s.cancel()
    #expect(s.armed == nil && !s.inFlight && s.status == nil)
}

@Test func oauthConfirmFiresOnlyTheArmedArmAndDisarmsFirst() {
    var s = OauthSectionState()
    s.keysLoaded(.ready(twoKeys()))
    s.arm(.issuerKey)
    // A confirm rendered for the other arm dispatches nothing and keeps what
    // the admin can see.
    let mismatched = s.pressConfirm(.sessionSecret)
    #expect(mismatched == nil)
    #expect(s.armed?.arm == .issuerKey && !s.inFlight)
    let fired = s.pressConfirm(.issuerKey)
    #expect(fired == .issuerKey)
    #expect(s.armed == nil)
    #expect(s.inFlight && s.status == L.admin.nestPage.oauthWorking)
    // The double press: nothing is armed any more, so nothing fires.
    let again = s.pressConfirm(.issuerKey)
    #expect(again == nil)
}

@Test func oauthDoneLandsTheVerdictAndTheReReadTogether() {
    var s = OauthSectionState()
    s.keysLoaded(.ready(twoKeys()))
    let pressed = s.pressRotate()
    #expect(pressed)
    s.done(status: "verdict", keys: .ready(oneKey("k3")))
    #expect(!s.inFlight && s.status == "verdict")
    #expect(s.answeredView?.keys.map(\.kid) == ["k3"])
    #expect(s.controlsLive)
}
