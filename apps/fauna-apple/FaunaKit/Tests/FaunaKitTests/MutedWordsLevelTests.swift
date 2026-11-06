import FaunaFFISwift
import Testing
@testable import FaunaKit

// The muted-words level picker (ui/settings.md § Muted words — the level
// picker): the picker's options, tokens and the weight classification all come
// from the shared faces, so the app never spells a level or compares a weight.

@Test func levelPickerOptionsAreTheSharedCatalogInOrder() {
    let tokens = mutedKeywordLevels().map { mutedKeywordLevelValue(level: $0) }
    #expect(tokens == ["hide", "show-less"])
}

@Test func levelTokenRoundTripsThroughTheSharedFaces() {
    for level in mutedKeywordLevels() {
        #expect(mutedKeywordLevelFromValue(value: mutedKeywordLevelValue(level: level)) == level)
    }
    #expect(mutedKeywordLevelFromValue(value: "bogus") == nil)
}

@MainActor
@Test func rowLevelIsTheSharedClassificationOfItsStoredWeight() {
    let vm = MutedWordsVM()
    #expect(vm.level(at: 0) == nil)
    #expect(vm.busy == false)
}
