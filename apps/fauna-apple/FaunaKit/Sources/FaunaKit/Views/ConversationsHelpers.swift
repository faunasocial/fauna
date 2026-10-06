import Foundation
import SwiftUI

// Shared rendering helpers for the unified conversations page. Lives in FaunaKit
// so both the macOS and iOS conversations views reuse it (it's platform-agnostic:
// protocol glyph, `TypedAddress` display, timestamp formatting, list-row model).
// Nothing here makes a per-rail *behaviour* decision — `glyphEmoji(_:)` is
// purely the cosmetic `protocol-icon` glyph; capability gating still keys off
// `capabilities.*` (see `docs/goal/ui/conversations.md` "Architectural rules").
public enum ConversationsUI {
    /// The emoji apple paints for a `SourceGlyph` concept — apple's single
    /// `SourceGlyph → native asset` map (the D5 model: shared Rust owns the
    /// canonical *concept* each source-protocol icon depicts, every app owns
    /// only its concept→asset rendering). Callers resolve the concept from the
    /// snapshot's precomputed `glyph` (`ThreadSummary` / `ThreadDetail`) or, for
    /// an off-snapshot `Rail`, via the shared `railGlyph(rail:)` FFI fn — so the
    /// brand decision lives in one Rust mapping (`fauna_core::source_glyph`),
    /// not duplicated per rail in each app.
    ///
    /// Apple renders the **emoji in a `Text`** for all six concepts (matching the
    /// other emoji clients web / linux / android — priority #1), because SF
    /// Symbols has no fox or butterfly, and mixing an emoji fox next to an SF
    /// Symbol envelope in the same rail column reads inconsistently. See
    /// `docs/goal/architecture/render-model.md` § Deltas → D5. Delegates to the
    /// shared-Rust `fauna_core::source_glyph::SourceGlyph::emoji()` (priority #2
    /// — apple's own copy, already correct, was the last hand-rolled duplicate
    /// of the map the D5 lift centralized for the other five apps).
    public static func glyphEmoji(_ glyph: SourceGlyph) -> String {
        sourceGlyphEmoji(glyph: glyph)
    }

    /// User-typeable / canonical string for a typed address — the single
    /// `user@host`-style identifier the spec calls for (no split handle/node).
    /// Delegates to the shared-Rust `fauna_conversations::TypedAddress::display()`
    /// over the `typed_address_display` FFI free fn (priority #2/#4 — the per-rail
    /// switch was duplicated across windows/apple/android/web; windows consumed the
    /// same export and dropped its copy). See `conversations.md` § Where logic lives.
    public static func display(_ addr: TypedAddress) -> String {
        typedAddressDisplay(addr: addr)
    }

    /// The rail a typed address belongs to — a structural 1:1 mapping (like
    /// `glyphEmoji(_:)`), not a behaviour decision; capability gating still keys
    /// off `capabilities.*`. Used by the recipient picker to derive a
    /// suggestion's `SourceGlyph` (via `railGlyph(rail:)`) for the `protocol-icon`.
    /// `nil` for an address of a kind this build does not name (`.unknown`,
    /// carried in from a newer device): it has no rail, so no icon — the same
    /// answer as the shared `TypedAddress::rail`.
    public static func rail(for addr: TypedAddress) -> Rail? {
        switch addr {
        case .fauna:    .faunaMls
        case .email:    .smtp
        case .bridged:  .bridged
        case .unknown:  nil
        }
    }

    /// Stable label for the active `SortOrder`, read by `conversation-sort`'s
    /// `automationActivate(value:)` — the cycle-button pattern
    /// (`AutomationRegistry.swift`: "a cycle-button (read the current option)").
    /// Matches the serde variant names `next_sort_order_from_variant` accepts
    /// (`fauna_conversations::snapshot`); purely a diagnostic/read value — no
    /// client feeds this string back into an API.
    public static func sortLabel(_ order: FaunaFFISwift.SortOrder) -> String {
        switch order {
        case .latestActivity: "LatestActivity"
        case .oldestFirst:    "OldestFirst"
        case .unread:         "Unread"
        }
    }
}

/// XAML-`ThreadRow`-equivalent: a value wrapper over `ThreadSummary` for the
/// list row. Keeps the row view free of UniFFI-type plumbing and gives the
/// `List` a stable `Identifiable` element.
public struct ConversationRowModel: Identifiable, Equatable {
    public let id: ThreadId
    public let label: String
    public let snippet: String
    public let rail: Rail
    /// Precomputed D5 icon concept from the snapshot — drives the cosmetic
    /// `protocol-icon` glyph via `ConversationsUI.glyphEmoji(_:)`.
    public let glyph: SourceGlyph
    public let flavor: ThreadFlavor
    public let unreadCount: UInt32
    public let participantCount: UInt32
    public let lastActivityMs: Int64

    public init(_ t: ThreadSummary) {
        self.id = t.threadId
        // Display label off the shared `thread_label_display` — a blank label
        // becomes the localized `(no subject)`, a real label rides verbatim. This
        // row model is display-only (no client-side filter/sort twin), so the
        // derivation lives here; the rename value reads the raw `detail.label`.
        self.label = renderLocalizedText(threadLabelDisplay(label: t.label))
        self.snippet = t.snippet
        self.rail = t.rail
        self.glyph = t.glyph
        self.flavor = t.flavor
        self.unreadCount = t.unreadCount
        self.participantCount = t.participantCount
        self.lastActivityMs = t.lastActivityMs
    }
}

/// Lifted 2026-09-02: both
/// `ConversationsListView` (iOS) and `MacConversationsView` (macOS) hand-rolled
/// byte-for-byte-identical private `rows`/`searchBinding` computed properties.
/// Both read only VM state (no view-identity dependency), so — unlike
/// `dnsProviderSection`'s free-function shape — a plain extension on the VM
/// itself is the right home: no untracked-read risk, since accessing `threads`/
/// `searchQuery` here goes through the exact same `@Observable`-tracked getters
/// either caller would have hit directly.
public extension ConversationsVM {
    /// The list's row models, mapped fresh from the live thread snapshot. The
    /// manager filters `snapshot().threads` off `searchBinding`'s query, so this
    /// is already the filtered set — no client-side `.filter` twin (priority #4;
    /// matches linux/web/windows).
    var rows: [ConversationRowModel] {
        threads.map(ConversationRowModel.init)
    }

    /// Two-way search-box binding — an emptied field clears the filter (`nil`),
    /// matching every other apple search box's empty-means-unfiltered contract.
    var searchBinding: Binding<String> {
        Binding(
            get: { self.searchQuery ?? "" },
            set: { self.setSearchQuery($0.isEmpty ? nil : $0) }
        )
    }
}
