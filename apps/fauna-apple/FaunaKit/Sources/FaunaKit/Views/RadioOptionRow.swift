import SwiftUI

/// A single selectable radio-style row — a filled/hollow circle glyph next
/// to a label + detail line, the whole row tappable. Shared by macOS and iOS
/// (priority #2): the `nat_mode_choice` screens each hand-rolled a
/// byte-for-byte-identical private `radioOption` helper until this consolidation.
public struct RadioOptionRow: View {
    let id: String
    let selected: Bool
    let label: String
    let detail: String
    let enabled: Bool
    let action: () -> Void

    public init(
        id: String,
        selected: Bool,
        label: String,
        detail: String,
        enabled: Bool,
        action: @escaping () -> Void
    ) {
        self.id = id
        self.selected = selected
        self.label = label
        self.detail = detail
        self.enabled = enabled
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(label).foregroundStyle(.primary)
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityIdentifier(id)
        .automationActivate(id) { action() }
    }
}
