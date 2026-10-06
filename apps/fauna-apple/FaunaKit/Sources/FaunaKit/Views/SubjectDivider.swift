import SwiftUI

/// Inline `subject-divider` rendered *between* message bubbles, before any
/// message whose `subjectLine` is set (the shared manager populates it only on
/// messages that change the running subject). Keeps participant-keyed threads
/// readable when a sender changes subject mid-flight (spec §"Subject-divider
/// rendering"). Indexed element. Shared by the macOS + iOS conversations
/// detail views.
public struct SubjectDivider: View {
    public let subject: String

    public init(subject: String) { self.subject = subject }

    public var body: some View {
        HStack(spacing: 8) {
            Rectangle().fill(Color.secondary.opacity(0.25)).frame(height: 1)
            Text(subject.isEmpty ? L.conversations.detail.noSubject : subject)
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
                .fixedSize()
            Rectangle().fill(Color.secondary.opacity(0.25)).frame(height: 1)
        }
        .padding(.vertical, 6)
        .accessibilityElement()
        .accessibilityLabel(subject)
        .accessibilityIdentifier(Ids.subjectDivider)
        // Indexed labeled container — register the subject as the read.
        .automationValue(Ids.subjectDivider, text: { subject })
    }
}
