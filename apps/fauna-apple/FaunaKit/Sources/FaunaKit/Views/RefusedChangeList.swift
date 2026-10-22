import SwiftUI

/// The Events page's refused-change list (`ui/events.md` § Refused scheduling
/// changes): `refused-change-list` with one `refused-change-item` per open row
/// (`-title`, `-author`, `-reason`, `-time`, `-dismiss`).
///
/// **One view for both apple apps** (priority #2). Absent entirely when the list
/// is empty. The record is informational: `-dismiss` is the only control, and
/// there is deliberately no "apply anyway" (`inbound-scheduling-authority.md`
/// § Surfacing). Every text is the shared `refused_change_view`'s, resolved
/// through `renderLocalizedText`; the author is the nest-attested sender, never
/// read off the `.ics`. The reason line is the reason, a space, then the repeat
/// count when present. Only the date format is the platform's.
///
/// The host page attaches `.onStoreChange { await vm.loadRefusedChanges() }` at
/// its surface root (not here: this view renders nothing when empty, and an
/// empty view is no reliable host for the modifier), so a refusal that lands
/// while the page stays open paints with no re-entry.
public struct RefusedChangeList: View {
    private let vm: EventsVM

    public init(vm: EventsVM) {
        self.vm = vm
    }

    public var body: some View {
        if !vm.refusedChanges.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text(L.events.refusedChanges.title)
                    .font(.subheadline.weight(.semibold))
                ForEach(vm.refusedChanges, id: \.key) { change in
                    row(change)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.refusedChangeList)
            .automationValue(Ids.refusedChangeList, text: { vm.refusedChanges.map(\.key).joined(separator: ",") })
        }
    }

    @ViewBuilder
    private func row(_ change: FfiRefusedChange) -> some View {
        let title = renderLocalizedText(change.headline)
        let author = renderLocalizedText(change.author)
        let reason = [renderLocalizedText(change.reason), change.attempts.map(renderLocalizedText)]
            .compactMap { $0 }
            .joined(separator: " ")
        let time = Date(timeIntervalSince1970: TimeInterval(change.lastRefusedAt))
            .formatted(date: .abbreviated, time: .omitted)
        HStack(alignment: .center, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .accessibilityIdentifier(Ids.refusedChangeTitle)
                    .automationValue(Ids.refusedChangeTitle, text: { title })
                Text(author)
                    .font(.caption)
                    .accessibilityIdentifier(Ids.refusedChangeAuthor)
                    .automationValue(Ids.refusedChangeAuthor, text: { author })
                Text(reason)
                    .font(.caption)
                    .accessibilityIdentifier(Ids.refusedChangeReason)
                    .automationValue(Ids.refusedChangeReason, text: { reason })
                Text(time)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(Ids.refusedChangeTime)
                    .automationValue(Ids.refusedChangeTime, text: { time })
            }
            Spacer(minLength: 0)
            Button(L.events.refusedChanges.dismiss) {
                Task { await vm.dismissRefusedChange(key: change.key) }
            }
            .accessibilityIdentifier(Ids.refusedChangeDismiss)
            .automationActivate(Ids.refusedChangeDismiss) {
                Task { await vm.dismissRefusedChange(key: change.key) }
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.refusedChangeItem)
        .automationValue(Ids.refusedChangeItem, text: { title })
    }
}
