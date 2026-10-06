import Testing
import Foundation
@testable import FaunaKit

// The reaction quick-set (`docs/goal/ui/conversations.md` § Reactions & message delete) is ONE
// definition in shared Rust — `fauna_conversations::QUICKSET_EMOJIS`, which linux and tui use as a
// crate dep and the FFI apps read through the `quicksetEmojis()` face (apple adopted it 2026-08-02;
// android/windows/web still hold hand-kept copies).
//
// Two things worth pinning on this side of the binding, neither of which the Rust test can see:
//   1. The emoji survive the FFI byte-identically. "❤️" is U+2764 U+FE0F — a base character plus a
//      variation selector — and a lossy hop would silently paint a monochrome heart, or compare
//      unequal against a wire emoji the user actually reacted with.
//   2. The ORDER, because `dm-reaction-option` is an indexed ui.yaml element: an e2e that taps
//      index 0 is asserting 👍 on all 7 apps.

@Test func theQuickSetCrossesTheFfiByteIdentically() {
    #expect(quicksetEmojis() == ["👍", "❤️", "😂", "😮", "😢", "🙏"])

    // Spelled out for the one that carries a variation selector: a lossy hop would drop U+FE0F and
    // still *look* right in most diffs.
    #expect(quicksetEmojis()[1] == "\u{2764}\u{FE0F}")
    #expect(quicksetEmojis()[1].unicodeScalars.count == 2)
}

@Test func theQuickSetIsStableAcrossCalls() {
    // The face allocates a fresh Vec per call; `dm-reaction-option[i]` indexes into it, so the
    // order must not depend on call site or iteration.
    #expect(quicksetEmojis() == quicksetEmojis())
    #expect(quicksetEmojis().count == 6)
}
