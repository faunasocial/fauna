#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// atproto.md § Errors & edge cases — the apple leg of the AT Protocol page's
// possibility answers (tui led). The page paints each depth rung and the
// delegation authorize from `AtprotoSettingsVM`'s answers, which are the
// shared machine's own predicates once it exists; these cover the Swift seam
// with no nest: before the machine is built, and when it cannot be built.
// The machine-side answers (the hosted gate, the no-identity authorize) are
// pinned in `libs/fauna-atproto-settings-machine`.

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

/// No rung is offered before the first status read — Off → Linked is an
/// immediate nest write, so a rung live on the pre-fetch default would act
/// on a guess — and each says why; authorize waits the same way.
@Test @MainActor func noRungOrAuthorizeIsOfferedBeforeTheMachineHasRead() {
    let vm = AtprotoSettingsVM()
    for rung in depthLevelOptions() {
        #expect(reasonKey(vm.rungAnswer(rung.level)) == "common.still_loading", "\(rung.level)")
    }
    #expect(reasonKey(vm.authorizeAnswer) == "common.still_loading")
    #expect(!vm.buildFailed)
}

/// A machine that cannot be built (here: no secret in this session) leaves
/// the page unavailable — the view paints the one `page_unavailable` line in
/// place of every control — and every answer says the read failed.
@Test @MainActor func aMachineThatCannotBeBuiltLeavesThePageUnavailable() async {
    let vm = AtprotoSettingsVM()
    await vm.configure(api: APIClient(nodeUrl: URL(string: "https://nest.invalid")!))
    #expect(vm.buildFailed)
    #expect(vm.machine == nil)
    for rung in depthLevelOptions() {
        #expect(reasonKey(vm.rungAnswer(rung.level)) == "common.load_failed", "\(rung.level)")
    }
    #expect(reasonKey(vm.authorizeAnswer) == "common.load_failed")
}
#endif
