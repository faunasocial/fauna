import SwiftUI

/// Two-click inline delete confirmation with an auto-disarm timeout — no modal,
/// tap once to arm (row shows "Confirm?"), tap again while armed to actually
/// delete; armed state clears itself after `seconds` if never confirmed (linux
/// `wire_two_click` parity). `MailListsView` and `MailAliasesView` each hand-rolled
/// the identical arm/confirm/auto-disarm body around their own `armedDeleteId`
/// `@State` — extracted here rather than merged into one shared view, since each
/// caller's own row label already reads `armedDeleteId` directly for its
/// "Confirm?" text.
@MainActor
func tapArmedDelete(_ id: String, armed: Binding<String?>, seconds: Double = 4, perform: @escaping () async -> Void) {
    if armed.wrappedValue == id {
        Task { await perform() }
        armed.wrappedValue = nil
    } else {
        armed.wrappedValue = id
        Task {
            try? await Task.sleep(for: .seconds(seconds))
            if armed.wrappedValue == id { armed.wrappedValue = nil }
        }
    }
}
