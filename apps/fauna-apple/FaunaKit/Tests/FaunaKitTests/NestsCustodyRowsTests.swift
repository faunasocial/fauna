import Foundation
import Testing
@testable import FaunaKit

// The Nests page's custodian-nest rows, escrow-holder badge gate and mint
// duration pick (`docs/goal/ui/nests.md` § Trust facet — custody rows,
// § Expiry / renewal → *Duration and blessing*). Pins the shell-side rules the
// shared fold cannot: which custody rows this page owns, when the revoke is
// offered, which nest wears the badge, and which duration a mint sends. The
// app-level witnesses are `test_custody_ceremony_journey.py::
// test_custody_ceremony_nest_anchored` and `test_nest_trust_grants.py`
// (macos, ios).

@MainActor @Suite struct NestsCustodyRowsTests {
    private func row(nestUrl: String?, pending: Bool = false, host: UInt8 = 0xAB)
        -> CustodyHolderRowView
    {
        let text = LocalizedText(key: "k", args: [:])
        return CustodyHolderRowView(
            grantId: Data([host]),
            host: Data(repeating: host, count: 32),
            custodianKey: pending ? nil : Data(repeating: host, count: 32),
            custodianNestUrl: nestUrl,
            scopes: nil,
            lastsUntil: nil,
            liveness: nil,
            receiptState: .noReceiptYet,
            receipt: CustodyReceiptRowView(
                statusLabel: text, attestedAtSecs: nil, heldBytesLabel: text, held: text,
                cap: text, degraded: false, heldBytes: nil, attestedCap: nil),
            pending: pending)
    }

    @Test func thePageOwnsExactlyTheNestAnchoredRows() {
        let device = row(nestUrl: nil, host: 0x01)
        let nest = row(nestUrl: "wss://nest.example", host: 0x02)
        let owned = LinkedNestsVM.nestAnchored([device, nest])
        #expect(owned == [nest])
        #expect(LinkedNestsVM.nestAnchored([device]).isEmpty)
    }

    @Test func aNestAnchoredRowRendersTheHostFamily() {
        let nest = row(nestUrl: "wss://nest.example")
        let host = shortId(hex: nest.host.hexString)
        #expect(CustodyNestItemRow.labelText(for: nest) == L.nests.custodyNestLabel(host: host))
    }

    @Test func aPendingCeremonyDisablesTheRevoke() {
        #expect(CustodyNestItemRow.isRevocable(row(nestUrl: "wss://n")))
        #expect(!CustodyNestItemRow.isRevocable(row(nestUrl: "wss://n", pending: true)))
    }

    @Test func theBadgeMarksOnlyTheMatchingNest() {
        let vm = LinkedNestsVM()
        let holder = String(repeating: "ab", count: 32)
        let other = String(repeating: "cd", count: 32)
        #expect(!vm.holdsEscrow(holder))
        vm.escrowHolders = [holder]
        #expect(vm.holdsEscrow(holder))
        #expect(vm.holdsEscrow(holder.uppercased()))
        #expect(!vm.holdsEscrow(other))
    }

    @Test func aMintSendsThePickedDurationElseTheRowDefault() {
        let oneOff = TrustGrantDuration.oneOff
        let standard = TrustGrantDuration.standard
        #expect(MintFlowView.duration(picked: "", default: oneOff) == oneOff)
        #expect(MintFlowView.duration(picked: "", default: standard) == standard)
        #expect(MintFlowView.duration(picked: durationText(standard), default: oneOff) == standard)
        #expect(MintFlowView.duration(picked: durationText(oneOff), default: standard) == oneOff)
    }
}
