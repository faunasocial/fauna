import SwiftUI

/// One grouped card in an eager `ScrollView { VStack }` container — the
/// `Section`-in-a-`Form` replacement that realizes eagerly on iOS (`Form`'s
/// lazy `Section` skips e2e-visible rows off-screen on first render). Shared
/// by every settings/wizard page (priority #2): `mailSettingsGroup`,
/// `mailExportGroup`, `mailImportGroup`, `photoBackupGroup` and
/// `webSettingsGroup` each hand-rolled a byte-for-byte-identical private
/// copy until
/// this consolidation.
@ViewBuilder
public func groupedSection<Content: View>(
    title: String? = nil,
    @ViewBuilder _ content: () -> Content
) -> some View {
    GroupBox {
        VStack(alignment: .leading, spacing: 8) {
            if let title {
                Text(title)
                    .font(.headline)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
