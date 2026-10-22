import Testing
import Foundation
@testable import FaunaKit

// `renderLocalizedText` / `renderLocalizedTextNested` fill placeholders through the
// one shared single-pass implementation (`fauna_core::localized::fill_placeholders`,
// over UniFFI), so an arg holding another arg's placeholder is data, never expanded —
// whatever the dictionary order (`docs/goal/behavior/notifications.md` § Localized
// body; the stranger-chosen knock-doorbell args ride this path). A key missing from
// the catalog resolves to itself, so the key doubles as the template here.

@Test func renderLocalizedTextDoesNotExpandAnArgHoldingAPlaceholder() {
    // Each Dictionary has its own random seed, so 64 rounds make a sequential
    // replace loop impossible to pass by luck.
    for _ in 0..<64 {
        let lt = LocalizedText(key: "{a} / {b}", args: ["a": "{b}", "b": "second"])
        #expect(renderLocalizedText(lt) == "{b} / second")
        #expect(renderLocalizedTextNested(lt) == "{b} / second")
    }
}
