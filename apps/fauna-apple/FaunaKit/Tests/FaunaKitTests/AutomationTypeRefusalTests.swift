import Testing
import Foundation
@testable import FaunaKit

// The *entry mode* type gate on the in-process automation server's text routes
// (`/element/{type,clear}`): a button whose free-entry form is an input under the
// same id (the fuller reaction picker's `dm-reaction-more-button`,
// conversations.md § Rendering / picker glue) is typeable only while that input
// is open. A type at the closed control is refused loudly — never written into a
// field no user could type into (convention 11), as linux refuses a closed emoji
// chooser. The routes' own wiring is proven end to end by the fuller-picker
// witness in `tests/e2e-unified/tests/test_conversations_reaction_toggle_and_picker.py`.

@Suite("Automation type refusal (entry mode)")
struct AutomationTypeRefusalTests {
    private func message(_ refusal: (() -> String?)?) -> String? {
        InProcessAutomationServer.typeRefusalMessage(
            route: "type", id: "dm-reaction-more-button", index: 0, typeRefusal: refusal)
    }

    @Test("an entry with no typeRefusal predicate is always typeable")
    func noPredicateProceeds() {
        #expect(message(nil) == nil)
    }

    @Test("an open entry-mode input is typeable")
    func openInputProceeds() {
        #expect(message({ nil }) == nil)
    }

    @Test("a closed entry-mode input is refused with the route, the id and the reason")
    func closedInputIsRefused() {
        let text = message({ "the emoji field is not open" })
        #expect(text?.contains("type at dm-reaction-more-button[0]") == true)
        #expect(text?.contains("the emoji field is not open") == true)
    }

    @Test("the registry snapshot reports an entry-mode control editable only while open")
    @MainActor
    func snapshotEditableFollowsTheOpenState() {
        let reg = AutomationRegistry.shared
        // Unique id so this can't collide with a parallel test on the shared singleton.
        let id = "test-entry-mode-\(UUID().uuidString)"
        let token = UUID()
        var open = false
        reg.register(id, token: token, .init(
            activate: { open.toggle() }, setValue: { _ in },
            typeRefusal: { open ? nil : "closed" }))
        defer { reg.unregister(id, token: token) }
        #expect(reg.snapshot().first { $0.id == id }?.editable == false)
        open = true
        #expect(reg.snapshot().first { $0.id == id }?.editable == true)
    }
}
