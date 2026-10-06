import SwiftUI

/// Add-participant overlay — a `recipient-picker` plus `add-participant-confirm`
/// (a *distinct* id from the compose bar's `dm-send-button`: the detail view's
/// compose bar stays in the a11y tree behind the sheet, so reusing the id would
/// be ambiguous). The manager owns the state (`snapshot.addParticipant`); this
/// view just wires the picker + confirm/cancel back to `ConversationsVM`. Shared
/// by the macOS + iOS conversations views.
///
/// Add-participant on a Fauna 1:1 forks a new `MlsGroup` thread (the manager
/// returns its id, which we select); on every other `(rail, flavor)` it adds in
/// place. Chip-accept routes through `acceptCurrentRecipientChip` so the manager
/// decides which picker is active (add-participant takes priority over new-thread
/// when both are open).
public struct AddParticipantSheet: View {
    public let state: AddParticipantState

    public init(state: AddParticipantState) { self.state = state }

    @Environment(ConversationsVM.self) private var vm

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.conversations.unified.threadAddParticipant).font(.headline)
            RecipientPicker(
                state: state.picker,
                // Live re-read off the @Observable vm (manager snapshot) — see the
                // matching note in NewThreadComposeForm; the add-participant picker
                // routes through the manager identically.
                liveResolveState: { vm.addParticipant?.picker.resolveState ?? .idle },
                // Typing owes a probe; Enter resolves first, then commits what the
                // probe confirmed — see the matching note in NewThreadComposeForm.
                onInputChange: { text in
                    vm.setAddParticipantRecipientInput(text)
                    Task { await vm.resolveRecipient() }
                },
                onAcceptCurrent: {
                    Task {
                        await vm.resolveRecipient()
                        _ = vm.acceptCurrentRecipientChip()
                    }
                },
                onAcceptSuggestion: { vm.acceptAddParticipantChip($0) }
            )
            HStack {
                Spacer()
                Button(L.common.cancel) { vm.cancelAddParticipant() }
                confirmButton
            }
        }
        .padding(20)
        .frame(minWidth: 360)
    }

    /// The confirm, gated only on the arm that actually reaches the nest.
    ///
    /// Adding to an **in-place MLS group** opens the add commit by fetching the
    /// newcomer's key package — `fauna.conversations.keypackage.fetch`, which is
    /// `OnlineOnly` — so that arm declares. A **fork** (a FaunaMls 1:1) and every
    /// non-FaunaMls rail issue no membership op at all and work with no nest, so
    /// gating them would grey a gesture that succeeds — the over-claim
    /// `docs/goal/architecture/account-data-plane.md` § The offline-mutation
    /// contract forbids.
    ///
    /// The discriminant is **not** re-derived here: `AddParticipantState.
    /// inPlaceMlsGroup` is stamped by the manager when it opens the overlay, off
    /// the single shared predicate
    /// `fauna_conversations::capabilities::is_in_place_mls_group` that
    /// `confirm_add_participant` also derives the wire decision from (priority
    /// #2 — the `(rail, flavor)` test is shared logic, and tui reads the same
    /// field). ⚠ It is not the fork-vs-mutate test: an SMTP 1:1 also adds in
    /// place, and issues nothing.
    @ViewBuilder
    private var confirmButton: some View {
        let button = Button(L.common.add) { confirm() }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.addParticipantConfirm)
            // A bare `.accessibilityIdentifier` is invisible to the in-process
            // driver (it only sees `automation*` modifiers) — without this the
            // confirm button 404s even though the sibling RecipientPicker
            // registers (it uses `automationField`). The `perform` mirrors the
            // Button's own action via the shared `confirm()` method.
            .automationActivate(Ids.addParticipantConfirm) { confirm() }

        if state.inPlaceMlsGroup {
            button.faunaGate("fauna.conversations.keypackage.fetch")
        } else {
            button
        }
    }

    private func confirm() {
        Task {
            if let newThreadId = await vm.confirmAddParticipant() {
                vm.selectThread(newThreadId)
            }
        }
    }
}
