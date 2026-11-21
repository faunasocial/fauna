import Foundation
import Testing
@testable import FaunaKit

// settings.md § Errors & edge cases, *Personal form submits paint from their
// draft*, and connected-apps.md § Errors & edge cases — the apple leg of the
// rule tui led. Each submit is painted from a shared draft answer and its
// press refuses with the same reason on the page's error line, nothing
// reaching the nest. These cover the Swift seams with no nest.

private func isBlocked(_ answer: Possibility) -> Bool {
    if case .blocked = answer { return true }
    return false
}

// MARK: - Mute word

@Test @MainActor func muteWordIsBlockedUntilATermIsTypedAndTheBlankPressRefuses() async {
    let vm = MutedWordsVM()
    let blank = vm.addAnswer("   ")
    #expect(isBlocked(blank))
    #expect(vm.addAnswer("spoilers") == .possible)
    await vm.add("   ")
    #expect(vm.errorMessage == blank.refusal)
    #expect(vm.errorMessage?.isEmpty == false)
}

// MARK: - Trained topics

@Test @MainActor func aTrainedTopicNameIsBlockedWhenBlankAndThePressSaysTheSameReason() async {
    let vm = TrainedTopicsVM()
    let blank = vm.nameAnswer("")
    #expect(isBlocked(blank))
    #expect(vm.nameAnswer("Cats") == .possible)
    await vm.create(name: " ")
    #expect(vm.errorMessage == blank.refusal)
    #expect(vm.errorMessage == L.personalization.trainedFactorBlankName)
}

@Test func publishOpenIsAbsentWithNoModelAndBlockedWithNoFeedWindow() {
    #expect(trainedFactorPublishOpenPossibility(hasFactorKey: false, managerReady: true, busy: false) == .absent)
    #expect(isBlocked(trainedFactorPublishOpenPossibility(hasFactorKey: true, managerReady: false, busy: false)))
    #expect(trainedFactorPublishOpenPossibility(hasFactorKey: true, managerReady: true, busy: false) == .possible)
}

@Test func thePublishSheetNeedsANameWithinTheBoundAndAKeptItem() {
    #expect(isBlocked(trainedFactorPublishSubmitPossibility(name: "", anyKept: true, busy: false)))
    #expect(isBlocked(trainedFactorPublishSubmitPossibility(name: String(repeating: "x", count: 200), anyKept: true, busy: false)))
    #expect(isBlocked(trainedFactorPublishSubmitPossibility(name: "Cats", anyKept: false, busy: false)))
    #expect(trainedFactorPublishSubmitPossibility(name: "Cats", anyKept: true, busy: false) == .possible)
}

// MARK: - Labeler catalog

@Test @MainActor func subscribeWaitsForTheCatalogAndThePressRefusesWithThatReason() async {
    let vm = LabelerCatalogVM()
    let answer = vm.subscribeAnswer(index: 0)
    #expect(answer == readPossibility(read: .pending))
    await vm.subscribe(index: 0)
    #expect(vm.errorMessage == answer.refusal)
}

// MARK: - Connected apps

@Test @MainActor func connectIsBlockedUntilACodeIsTypedAndABlankCodeIsNeverSent() async {
    let vm = ConnectedAppsVM()
    vm.code = "  "
    let blank = vm.connectCodeAnswer
    #expect(isBlocked(blank))
    await vm.submitCode()
    #expect(vm.errorMessage == blank.refusal)
    #expect(vm.code == "  ")
    vm.code = "ABCD-1234"
    #expect(vm.connectCodeAnswer == .possible)
}

@Test @MainActor func approveOnACardNoLongerListedIsAbsentAndThePressDoesNothing() async {
    let vm = ConnectedAppsVM()
    #expect(vm.approveAnswer(consentIdHex: "00") == .absent)
    await vm.resolveRequest(consentIdHex: "00", approved: true)
    #expect(vm.refusal == "")
}
