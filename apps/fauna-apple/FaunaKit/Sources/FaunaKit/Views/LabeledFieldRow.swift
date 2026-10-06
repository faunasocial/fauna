import SwiftUI

/// A caption/value row inside an admin detail card: an optional caption label
/// (hidden when `caption` is empty — the bare-value rows on
/// `AdminCustodyHostingView`'s status/receipt lines) beside a value `Text`
/// that carries its own automation id and registers its read via
/// `automationValue` so the in-process driver can `get_text` it per-row.
/// `value` is `@autoclosure @escaping` so the registered read re-evaluates
/// the live expression on every lookup (never a captured frozen `let`),
/// exactly as the rendered `Text` re-evaluates each body pass.
///
/// `AdminBridgesPendingView` and `AdminCustodyHostingView` each hand-rolled a
/// private `field` helper of this exact shape (the latter's own doc comment:
/// "same idiom as AdminBridgesPendingView.field") until this consolidation —
/// same class of finding, same fix shape, as `RadioOptionRow` (row 7 harvest
/// pass 24).
public struct LabeledFieldRow: View {
    let caption: String
    let value: () -> String
    let id: String
    let monospaced: Bool

    public init(_ caption: String, _ value: @autoclosure @escaping () -> String,
                id: String, monospaced: Bool = false) {
        self.caption = caption
        self.value = value
        self.id = id
        self.monospaced = monospaced
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 8) {
            if !caption.isEmpty {
                Text(caption)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(width: 90, alignment: .leading)
            }
            Text(value())
                .font(monospaced ? .caption.monospaced() : .caption)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(id)
                .automationValue(id, text: { value() })
        }
    }
}
