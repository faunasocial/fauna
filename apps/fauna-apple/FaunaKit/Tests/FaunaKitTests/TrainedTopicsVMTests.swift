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
