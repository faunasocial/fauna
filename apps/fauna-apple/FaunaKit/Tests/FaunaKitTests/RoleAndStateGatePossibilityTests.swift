#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// The role and state gates tui leads (`admin.md` § Errors & edge cases, *Role
// and state gates answer one `Possibility` too`; `dns-management.md` § Errors &
// edge cases, *Renewal delegation and the rename banner answer too*;
// `bridges.md` § Errors & edge cases, rule 4; `ui/devices.md` § Errors & edge
// cases, *The participation switch answers one `Possibility`*), lifted to the
// apple apps through shared FaunaKit: each control paints from the shared
// answer, and each press asks it again and refuses with the same reason before
// anything reaches the nest. A VM with no API proves the refusal came first:
// a press that got past it would land the not-configured error instead.

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

private func user(isAdmin: Bool) -> FfiAdminUser {
    FfiAdminUser(
        actorId: Data([7]),
        tier: "free",
        label: "",
        handle: "someone",
        suspended: false,
        createdAt: 0,
        inboxBytesUsed: 0,
        storageBytesUsed: 0,
        eviction: nil,
        mailServingEnabled: true,
        isAdmin: isAdmin
    )
}

// MARK: - Admin entry

@Test func theAdminEntryIsAbsentForANonAdmin() {
    #expect(adminEntryPossibility(isAdmin: true) == .possible)
    #expect(adminEntryPossibility(isAdmin: false) == .absent)
}

// MARK: - Users row

@Test @MainActor func aRowControlTheRowDoesNotOfferIsAbsentAndItsPressRefused() async {
    let vm = AdminVM()
    let admin = user(isAdmin: true)
    // An admin row offers neither cut-off control (the nest would refuse).
    #expect(vm.rowPossibility(admin, .suspend) == .absent)
    #expect(vm.rowPossibility(admin, .evict) == .absent)
    #expect(vm.rowPossibility(admin, .makeAdmin) == .absent)
    // An active plain row offers suspend, evict and make-admin, not restore.
    let plain = user(isAdmin: false)
    #expect(vm.rowPossibility(plain, .suspend) == .possible)
    #expect(vm.rowPossibility(plain, .evict) == .possible)
    #expect(vm.rowPossibility(plain, .makeAdmin) == .possible)
    #expect(vm.rowPossibility(plain, .restore) == .absent)
    #expect(vm.rowPossibility(plain, .takeOver) == .absent)

    // Each refused press lands on `admin-users-action-error` (an absent
    // control's refusal is empty) and never reaches the unconfigured API.
    await vm.suspendUser(admin)
    #expect(vm.actionError == "")
    await vm.evictUser(admin)
    #expect(vm.actionError == "")
    await vm.makeAdmin(admin)
    #expect(vm.actionError == "")
    await vm.cancelEviction(plain)
    #expect(vm.actionError == "")
    await vm.removeAdmin(plain)
    #expect(vm.actionError == "")
    vm.armTakeOver(plain)
    #expect(vm.actionError == "")
    #expect(!vm.isTakeOverArmed(plain))
}

// MARK: - Nest origin and channel

@Test @MainActor func originAndChannelRefuseBeforeTheirRead() async {
    let vm = AdminNestVM()
    #expect(reasonKey(vm.webAppOriginSetPossibility) == "common.still_loading")
    #expect(reasonKey(vm.updateChannelSetPossibility) == "common.still_loading")
    #expect(!vm.webAppOriginEditable)
    #expect(!vm.updateChannelEditable)
    await vm.saveWebAppOrigin()
    #expect(vm.errorMessage == readPossibility(read: .pending).refusal)
    vm.errorMessage = nil
    await vm.saveUpdateChannel()
    #expect(vm.errorMessage == readPossibility(read: .pending).refusal)
}

// MARK: - Web app section while the central origin is not live

// The three states tui pins (`apps/fauna-tui/src/admin/nest.rs`;
// `web-content-hosting.md` § Same-origin security model → *While the central
// origin is not live, the choice is not offered*, rule 5): the section and the
// central radio paint from the shared answers, and the save asks with the draft.

private func originView(
    selected: FfiWebAppOrigin?, section: Possibility, central: Possibility
) -> FfiAdminWebAppOriginView {
    FfiAdminWebAppOriginView(
        selected: selected,
        status: LocalizedText(key: "admin.nest_page.web_app_origin_status_central_not_live", args: [:]),
        scope: LocalizedText(key: "admin.nest_page.web_app_origin_scope", args: [:]),
        canSet: true,
        centralLabel: LocalizedText(key: "admin.nest_page.web_app_origin_central", args: [:]),
        sectionPossibility: section,
        centralPossibility: central
    )
}

@Test @MainActor func theWebAppSectionIsAbsentBeforeItsRead() {
    let vm = AdminNestVM()
    #expect(vm.webAppOriginSectionPossibility == .absent)
    #expect(vm.webAppOriginCentralPossibility == .absent)
}

@Test @MainActor func bundledWhileCentralIsNotLivePaintsNoSection() {
    let vm = AdminNestVM()
    vm.paintWebAppOrigin(originView(selected: .bundled, section: .absent, central: .absent))
    #expect(vm.webAppOriginSectionPossibility == .absent)
    #expect(vm.webAppOriginCentralPossibility == .absent)
}

@Test @MainActor func storedCentralWhileNotLiveOffersOnlyTheWayBack() async {
    let vm = AdminNestVM()
    vm.paintWebAppOrigin(originView(selected: .central, section: .possible, central: .absent))
    // Status, bundled radio and save stay; the central radio is absent.
    #expect(vm.webAppOriginSectionPossibility == .possible)
    #expect(vm.webAppOriginCentralPossibility == .absent)
    #expect(vm.webAppOriginSelectPossibility(.bundled) == .possible)
    // The central draft the read holds is refused by the save with the nest's
    // own reason, before anything reaches the (unconfigured) API.
    #expect(reasonKey(vm.webAppOriginSetPossibility) == "error.admin.web_app_origin.central_not_live")
    await vm.saveWebAppOrigin()
    #expect(vm.errorMessage == vm.webAppOriginSetPossibility.refusal)
    vm.errorMessage = nil
    // A central pick is not taken; bundled is, and its save is offered.
    vm.selectWebAppOrigin(.central)
    #expect(vm.webAppOriginDraft == .central)
    vm.selectWebAppOrigin(.bundled)
    #expect(vm.webAppOriginDraft == .bundled)
    #expect(vm.webAppOriginSetPossibility == .possible)
    #expect(vm.webAppOriginEditable)
}

@Test @MainActor func centralLivePaintsTheFullSection() {
    let vm = AdminNestVM()
    vm.paintWebAppOrigin(originView(selected: .bundled, section: .possible, central: .possible))
    #expect(vm.webAppOriginSectionPossibility == .possible)
    #expect(vm.webAppOriginCentralPossibility == .possible)
    vm.selectWebAppOrigin(.central)
    #expect(vm.webAppOriginDraft == .central)
    #expect(vm.webAppOriginSetPossibility == .possible)
}

// MARK: - Mail enable

@Test @MainActor func turningMailOnRefusesBeforeTheRead() async {
    let vm = AdminMailVM()
    #expect(reasonKey(vm.enableTogglePossibility) == "common.still_loading")
    await vm.setMailEnabled(true)
    #expect(vm.errorMessage == readPossibility(read: .pending).refusal)
}

// MARK: - DNS delegate and rename banner

@Test @MainActor func delegationWaitsForACredentialZone() async {
    let vm = AdminDnsVM()
    #expect(reasonKey(vm.delegatePossibility) == "admin.dns.cert.delegate_no_zones")
    await vm.delegateRenewal(domain: "example.test", zone: "example.test")
    #expect(vm.errorMessage == vm.delegatePossibility.refusal)
}

@Test @MainActor func theRenameBannerOffersNothingWithNoRenameInFlight() {
    let vm = AdminDnsVM()
    #expect(vm.renamePossibility(.complete) == .absent)
    #expect(vm.renamePossibility(.extend) == .absent)
    #expect(vm.renamePossibility(.abort) == .absent)
}

// MARK: - Bridge Link

@Suite @MainActor struct BridgeLinkPossibilityTests {
    private func bridge(modes: [BridgeLinkMode], error: String? = nil, linked: Bool = false) -> BridgeInfo {
        BridgeInfo(
            id: "bluesky", name: "Bluesky", available: true, linked: linked, identity: nil, mode: nil,
            settings: [], supportsFollows: true, linkModes: modes, error: error)
    }

    private let password = BridgeLinkMode(mode: "password", label: "Password", clientAction: nil,
                                          platform: nil, fields: [])

    @Test func aModelessBridgeIsBlockedAndItsLinkRefusedWithTheSameReason() async {
        let b = bridge(modes: [])
        let answer = BridgeManagerVM.linkPossibility(b)
        #expect(reasonKey(answer) == "bridges.no_link_method")
        let vm = BridgeManagerVM()
        await vm.link(bridge: b)
        #expect(vm.errorMessage == answer.refusal)
    }

    @Test func theProvidersOwnExplanationBlocksLink() {
        let answer = BridgeManagerVM.linkPossibility(bridge(modes: [], error: "quota spent"))
        #expect(reasonKey(answer) == "bridges.link_blocked_by_provider")
        #expect(answer.reasonText == "quota spent")
    }

    @Test func aModeForAnotherPlatformDoesNotApply() {
        let web = BridgeLinkMode(mode: "nip07", label: "Extension", clientAction: nil,
                                 platform: "web", fields: [])
        #expect(reasonKey(BridgeManagerVM.linkPossibility(bridge(modes: [web]))) == "bridges.no_link_method")
        #expect(BridgeManagerVM.linkPossibility(bridge(modes: [password])) == .possible)
        // Unlink needs no mode.
        #expect(BridgeManagerVM.linkPossibility(bridge(modes: [], linked: true)) == .possible)
    }
}

// MARK: - P2P participation

@Test func aSiblingsParticipationSwitchSaysWhyItIsBlocked() {
    let label = LocalizedText(key: "devices.p2p_participation_off_requested", args: [:])
    let pending = P2pParticipationPaint(own: false, checked: true, label: label, actionable: false)
    #expect(reasonKey(p2pParticipationPossibility(paint: pending)) == "devices.p2p_participation_off_requested")
    let off = P2pParticipationPaint(own: false, checked: false, label: label, actionable: false)
    #expect(reasonKey(p2pParticipationPossibility(paint: off)) == "devices.p2p_participation_on_there")
    let own = P2pParticipationPaint(own: true, checked: false, label: label, actionable: true)
    #expect(p2pParticipationPossibility(paint: own) == .possible)
}

@Test @MainActor func aParticipationPressOnNoRowIsRefused() {
    let vm = DevicesMachineVM()
    #expect(vm.p2pParticipationPossibility(index: 0) == .absent)
}
#endif
