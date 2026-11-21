import Foundation
import Testing
@testable import FaunaKit

// The admin answers apple paints and re-asks (`docs/goal/behavior/admin.md`
// § 2, *What a row offers*, and § 4, *A forwarder needs a hosted domain*):
// these pin that each answer the views read is the shared one and that each
// action refuses out loud with its reason. The predicates' own grammar is
// pinned in `fauna_client_admin` and `fauna_client_mail_settings::forwarders`.

private func key(_ p: Possibility) -> String {
    switch p {
    case .possible: return "<possible>"
    case .absent: return "<absent>"
    case .blocked(let reason): return reason.key
    }
}

private func tier(_ name: String) -> FfiAdminTier {
    FfiAdminTier(name: name, maxInboxBytes: 0, maxStorageBytes: 0, maxDevices: 0,
                 maxBlobSize: 0, maxFeeds: 0)
}

@Suite @MainActor struct AdminOfferOnlyPossibleTests {
    @Test func admitAndInviteWaitForATier() async {
        let vm = AdminVM()
        #expect(key(vm.tierPickAnswer) == "admin.users_page.no_tier")
        // The press asks again and refuses with the same reason, sending nothing.
        await vm.admitUser(actorHex: String(repeating: "ab", count: 32), handle: "", tier: "")
        #expect(vm.actionError == vm.tierPickAnswer.refusal)
        #expect(vm.actionError?.isEmpty == false)
        vm.actionError = nil
        #expect(await vm.mintInviteCode(tier: "", uses: 1) == nil)
        #expect(vm.actionError == vm.tierPickAnswer.refusal)

        vm.tiers = [tier("free")]
        #expect(vm.tierPickAnswer.isPossible)
    }

    @Test func usersPagingIsBlockedAtItsBounds() async {
        let vm = AdminVM()
        vm.totalUsers = 10
        #expect(key(vm.pageStepAnswer(forward: false)) == "admin.users_page.no_page_before")
        #expect(key(vm.pageStepAnswer(forward: true)) == "admin.users_page.no_page_after")
        await vm.nextPage()
        #expect(vm.actionError == vm.pageStepAnswer(forward: true).refusal)
        #expect(vm.offset == 0)

        vm.totalUsers = Int(vm.pageSize) + 1
        #expect(vm.pageStepAnswer(forward: true).isPossible)
    }

    @Test func membershipClearSaysNothingIsSet() async {
        let vm = AdminTiersVM()
        let answer = vm.membershipClearAnswer(tierName: "gold")
        #expect(key(answer) == "admin.settings_page.membership_clear_nothing_set")
        await vm.clearMembershipTier(tierName: "gold")
        #expect(vm.errorMessage == answer.refusal)
    }

    @Test func aForwarderBeforeItsReadOffersNoDnsStep() async {
        let vm = ForwardersVM()
        // No machine yet: the read is on its way, which is not "no domain".
        #expect(key(vm.formAnswer) == key(readPossibility(read: .pending)))
        #expect(!vm.offersDomainsStep)
        // The submit says the page is still loading before it judges the draft.
        #expect(key(vm.createAnswer(localDomain: "", pattern: "", forwardTarget: ""))
                == key(readPossibility(read: .pending)))
        await vm.create(localDomain: "", pattern: "a", forwardTarget: "b@example.com")
        #expect(vm.errorMessage == readPossibility(read: .pending).refusal)
    }
}
