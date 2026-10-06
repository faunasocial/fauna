import SwiftUI

/// A labeled, trailing-aligned editable text-field row — the shared shape behind
/// admin policy/tier/profile settings forms. `LabeledFieldRow`'s sibling: that one
/// covers the read-only caption/value DISPLAY case, this
/// covers the EDITABLE case.
///
/// `AdminMailView` (`policyField`), `AdminTiersView` (`capField`), and
/// `ProfileView` (`formField`) each hand-rolled a private helper of this exact
/// shape — a label beside a bound `TextField` — differing along five independent,
/// real axes (each verified by diffing the three original bodies before merging,
/// same discipline as `LabeledFieldRow`/row 7 pass 24): label color, field width,
/// subtitle support, trailing text alignment (`formField` alone omits it), and row
/// alignment (`formField` alone uses the HStack default `.center` rather than
/// `.firstTextBaseline`) — plus disabled support (`formField`'s only). Found by
/// the 2026-09-09 dedup sweep. `ProfileView`'s
/// fourth near-miss, `linkField`, deliberately stays OUT of this consolidation:
/// it has no label/HStack wrapper and no width cap at all, a structurally
/// different, smaller shape the prior pass already named as not belonging here.
///
/// Defaults match `policyField`'s shape (the majority caller by call-site count);
/// `AdminTiersView`/`ProfileView` pass the differing params explicitly.
public struct EditableFieldRow: View {
    let label: String
    let id: String
    @Binding var text: String
    var subtitle: String?
    var labelColor: Color
    var maxWidth: CGFloat
    var trailingAlign: Bool
    var rowAlignment: VerticalAlignment
    var disabled: Bool

    public init(_ label: String, _ id: String, _ text: Binding<String>,
                subtitle: String? = nil, labelColor: Color = .primary,
                maxWidth: CGFloat = 160, trailingAlign: Bool = true,
                rowAlignment: VerticalAlignment = .firstTextBaseline,
                disabled: Bool = false) {
        self.label = label
        self.id = id
        self._text = text
        self.subtitle = subtitle
        self.labelColor = labelColor
        self.maxWidth = maxWidth
        self.trailingAlign = trailingAlign
        self.rowAlignment = rowAlignment
        self.disabled = disabled
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: rowAlignment) {
                Text(label)
                    .foregroundStyle(labelColor)
                Spacer(minLength: 12)
                TextField("", text: $text)
                    .multilineTextAlignment(trailingAlign ? .trailing : .leading)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: maxWidth)
                    .disabled(disabled)
                    .accessibilityIdentifier(id)
                    .automationField(id, text: $text, isEnabled: { !disabled })
            }
            if let subtitle {
                Text(subtitle).font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
