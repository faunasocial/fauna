import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit test for `TrainedTopicsVM.mapError` — the boundary error → i18n
// mapping only, not the full FFI round-trip (harness e2e territory).
// `.BlankName` used to fall back to `"\(error)"`,
// painting the generated enum's debug shape (`ui/README.md` § A cancellation
// is not an error, rule 1 — one shared mapping function, not a per-catch-block
// decision).

@Test @MainActor func blankNameMapsToRealCopyNotADebugShape() {
    let vm = TrainedTopicsVM()
    let text = vm.mapError(FfiTrainedTopicsError.BlankName)
    #expect(text == L.personalization.trainedFactorBlankName)
    #expect(text?.contains("BlankName") == false)
}

@Test @MainActor func capMapsToTheCatalogSentenceWithTheBound() {
    let vm = TrainedTopicsVM()
    #expect(vm.mapError(FfiTrainedTopicsError.Cap(max: 8)) == L.personalization.trainedFactorCap(max: "8"))
}

@Test @MainActor func generalSurfacesItsOwnMessage() {
    let vm = TrainedTopicsVM()
    #expect(vm.mapError(FfiTrainedTopicsError.General(msg: "nest down")) == "nest down")
}

@Test @MainActor func aCancelledTrainedTopicsCallMapsToNothing() {
    let vm = TrainedTopicsVM()
    #expect(vm.mapError(CancellationError()) == nil)
}

// topic-factors.md § Authoring surface & picker, outcome 13: a blank name is
// refused before the round trip with the blank-name sentence — never a silent
// no-op — on create and rename alike (linux's `submit`, tui's paint).
@Test @MainActor func aBlankCreateSaysItNeedsAName() async {
    let vm = TrainedTopicsVM()
    await vm.create(name: "   ")
    #expect(vm.errorMessage == L.personalization.trainedFactorBlankName)
}

@Test @MainActor func aBlankRenameSaysItNeedsAName() async {
    let vm = TrainedTopicsVM()
    await vm.rename(id: Data([1]), name: "\n ")
    #expect(vm.errorMessage == L.personalization.trainedFactorBlankName)
}
