import SwiftUI

/// Quota usage bar with label, used/max bytes, and color thresholds.
public struct QuotaBar: View {
    public let label: String
    public let used: Int
    public let max: Int

    public init(label: String, used: Int, max: Int) {
        self.label = label
        self.used = used
        self.max = max
    }

    private var fraction: Double {
        FaunaFFISwift.quotaFraction(usedBytes: Int64(used), maxBytes: Int64(max))
    }

    private var color: Color {
        if fraction > 0.9 { return .red }
        if fraction > 0.7 { return .orange }
        return .accentColor
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(label)
                Spacer()
                Text("\(ValueFormat.byteSize(used)) / \(ValueFormat.byteSize(max))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(Ids.settingsStorageText)
            }
            ProgressView(value: fraction)
                .tint(color)
        }
        .accessibilityIdentifier(Ids.settingsStorageBar)
    }
}
