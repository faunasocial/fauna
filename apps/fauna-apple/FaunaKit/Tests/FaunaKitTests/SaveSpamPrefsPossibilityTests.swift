import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

// `save-spam-prefs` is offered only while a threshold differs from the value
// the nest confirmed (`docs/goal/ui/settings.md`; `docs/goal/ui/README.md`
// § Offer only what is possible, rules 5-7). The FaunaKit leg shared by macOS
// and iOS; mirrors tui's apps 6657 build and the shared
// `fauna_client_spam::possibility` tests.

private let stillLoading = Possibility.blocked(reason: LocalizedText(key: "common.still_loading", args: [:]))
private let nothingToSave = Possibility.blocked(reason: LocalizedText(key: "common.nothing_to_save", args: [:]))

@MainActor
struct SaveSpamPrefsPossibilityTests {
    @Test("before the read resolves, Save is blocked still-loading")
    func blockedBeforeTheRead() {
        let vm = PrivacySettingsVM()
        #expect(vm.saveSpamPrefsAnswer == stillLoading)
    }

    @Test("with both thresholds equal to the saved prefs, Save has nothing to save")
    func blockedWhileUnchanged() {
        let vm = PrivacySettingsVM()
        vm.seedSpamPrefsForTest(SpamPreferences(spamThreshold: 0.7, phishingThreshold: 0.4))
        #expect(vm.saveSpamPrefsAnswer == nothingToSave)
    }

    @Test("an edit to either threshold makes Save possible")
    func possibleAfterAnEdit() {
        let vm = PrivacySettingsVM()
        vm.seedSpamPrefsForTest(SpamPreferences(spamThreshold: 0.7, phishingThreshold: 0.4))
        vm.spamThreshold = 0.8
        #expect(vm.saveSpamPrefsAnswer == .possible)
        vm.spamThreshold = 0.7
        vm.phishingThreshold = 0.3
        #expect(vm.saveSpamPrefsAnswer == .possible)
        vm.phishingThreshold = 0.4
        #expect(vm.saveSpamPrefsAnswer == nothingToSave, "editing back to the saved value is no edit")
    }

    @Test("a blocked press is refused on error-message and sends nothing")
    func blockedPressIsRefused() async {
        let vm = PrivacySettingsVM()
        vm.seedSpamPrefsForTest(SpamPreferences(spamThreshold: 0.7, phishingThreshold: 0.4))
        await vm.saveSpamPreferences()
        #expect(vm.spamPrefsError == nothingToSave.refusal)
        #expect(!vm.spamPrefsSaved)
    }
}
