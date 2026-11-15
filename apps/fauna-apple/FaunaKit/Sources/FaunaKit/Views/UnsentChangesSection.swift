import SwiftUI

/// The unsent tick (`account-offline-mutation.md` § The offline-mutation
/// contract → *The unsent-changes surface*): the runtime's generic drain
/// tells the app nothing when it sends or parks an intent, so a surface that
/// shows unsent changes re-reads the device outbox every two seconds while it is
/// mounted — the feed's hearts, a conversation's queued own copies, the account
/// page's list. tui's `session::spawn_unsent_refresh` is the same edge. Every
/// read is of this device's store: no nest is needed, and none is asked.
extension View {
    public func unsentRefresh(_ read: @escaping @MainActor () async -> Void) -> some View {
        task {
            while !Task.isCancelled {
                await read()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
}

// The account page's list reads the device outbox through the free UniFFI
// functions `unsentChanges` / `unsentChangeStateText` / `unsentChangeReasonText`
// / `retryUnsentChange` / `discardUnsentChange`, which ride `fauna-ffi`'s
// `feed-manager` gate — excised from the Fauna Kids build (family-safety.md
// § The account age band, the kids-app bullet, item (4)). The unsent tick above
// stays: the kids app's conversations use it for their queued own copies.
#if !FAUNA_KIDS
/// The STANDING unsent-changes section (`account-offline-mutation.md` § The
/// offline-mutation contract → *The unsent-changes surface*, point 1),
/// shared by macOS + iOS — the sibling of ``PendingActionsSection`` and its
/// shape, mounted right after it on the Account page. ALWAYS rendered: a
/// conditional render would hide the affordance exactly when a user goes looking
/// for a change that never showed up. Mirrors tui's `unsent_changes_elements`.
///
/// One row per undrained intent in the shared model's order (scope, then
/// compose), its words from the shared describer (`description.label`,
/// `unsentChangeStateText`, `unsentChangeReasonText`) — no app decides them.
/// Retry and discard are offered on a not-sent row only; discard arms a confirm
/// on that row first. Read from the device outbox (UniFFI `unsentChanges()`),
/// never the nest, so it answers with the nest down.
public struct UnsentChangesSection: View {
    /// `nil` before the first read lands (the bare title), `[]` once read and
    /// empty (the empty line), rows otherwise (the counted title).
    @State private var changes: [UnsentChange]?
    /// The intent whose discard is armed — its row shows the confirm.
    @State private var armedDiscard: String?

    public init() {}

    private var title: String {
        guard let changes else { return L.settings.unsentChanges.title }
        if changes.isEmpty { return L.settings.unsentChanges.none }
        return L.settings.unsentChanges.titleCount(count: "\(changes.count)")
    }

    public var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                automationText(Ids.unsentChangesSection, title)
                    .font(.headline)
                if let changes {
                    ForEach(Array(changes.enumerated()), id: \.element.intentId) { index, change in
                        row(change, index: index)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .unsentRefresh { await reload() }
    }

    /// Re-read the list; a failed read keeps what is painted (the next tick
    /// tries again).
    private func reload() async {
        guard let read = try? await unsentChanges() else { return }
        changes = read
        if let armed = armedDiscard,
           !read.contains(where: { $0.intentId == armed && Self.isNotSent($0) })
        {
            armedDiscard = nil
        }
    }

    static func isNotSent(_ change: UnsentChange) -> Bool {
        if case .unsent = change.state { return true }
        return false
    }

    /// The row's kind and target in words — the shared describer's label.
    static func describe(_ change: UnsentChange) -> String {
        renderLocalizedText(change.description.label)
    }

    /// The row's state line: the shared state text, and on a not-sent row why
    /// (`settings.unsent_changes.status_with_reason` over both).
    static func status(_ change: UnsentChange) -> String {
        let state = renderLocalizedText(unsentChangeStateText(change: change))
        guard let reason = unsentChangeReasonText(change: change) else { return state }
        return L.settings.unsentChanges.statusWithReason(
            state: state, reason: renderLocalizedText(reason))
    }

    @ViewBuilder
    private func row(_ change: UnsentChange, index: Int) -> some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 4) {
                automationText(Ids.unsentChangeDescription, Self.describe(change))
                automationText(Ids.unsentChangeStatus, Self.status(change))
                    .font(.caption)
                    .foregroundStyle(Self.isNotSent(change) ? .red : .secondary)
            }
            Spacer()
            if Self.isNotSent(change) {
                actions(change)
            }
        }
        .padding(.vertical, 4)
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.unsentChangeItem)
        .automationValue(Ids.unsentChangeItem, text: { Self.describe(change) })
        .automationScope(Ids.unsentChangeItem, index: index)
    }

    @ViewBuilder
    private func actions(_ change: UnsentChange) -> some View {
        let id = change.intentId
        HStack(spacing: 8) {
            Button(L.settings.unsentChanges.retry) { retry(id) }
                .accessibilityIdentifier(Ids.unsentChangeRetryButton)
                .automationActivate(Ids.unsentChangeRetryButton) { retry(id) }
            Button(L.settings.unsentChanges.discard, role: .destructive) { armedDiscard = id }
                .accessibilityIdentifier(Ids.unsentChangeDiscardButton)
                .automationActivate(Ids.unsentChangeDiscardButton) { armedDiscard = id }
            if armedDiscard == id {
                Button(L.settings.unsentChanges.discardConfirm, role: .destructive) { discard(id) }
                    .accessibilityIdentifier(Ids.unsentChangeDiscardConfirmButton)
                    .automationActivate(Ids.unsentChangeDiscardConfirmButton) { discard(id) }
            }
        }
    }

    private func retry(_ id: String) {
        Task {
            _ = try? await retryUnsentChange(intentIdHex: id)
            await reload()
        }
    }

    private func discard(_ id: String) {
        armedDiscard = nil
        Task {
            _ = try? await discardUnsentChange(intentIdHex: id)
            await reload()
        }
    }
}
#endif // !FAUNA_KIDS
