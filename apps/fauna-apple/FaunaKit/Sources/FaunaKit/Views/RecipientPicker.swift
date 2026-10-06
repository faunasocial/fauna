import SwiftUI

/// Recipient picker — `recipient-picker` component (ui.yaml). Used by the
/// new-thread compose form and the add-participant overlay. Dumb view: it
/// renders a `RecipientPickerState` and reports events; the manager owns the
/// state (resolution, chip list, rail-lock all live in shared Rust). Shared by
/// the macOS + iOS conversations views.
///
/// Element IDs: `recipient-picker-input`, `recipient-picker-chip` (indexed),
/// `recipient-picker-suggestion` (indexed), `recipient-resolve-status` — a
/// single element whose state is carried via `.accessibilityValue`
/// (`idle` / `resolving` / `resolved` / `error`) — and `recipient-picker-class`,
/// the class statement of the room about to be created (`class` attribute).
public struct RecipientPicker: View {
    public let state: RecipientPickerState
    /// Live re-read of the picker's resolve state, closing over the parent's
    /// `@Observable ConversationsVM` (a reference). The registry read closure for
    /// `recipient-resolve-status` calls this on every in-process query instead of
    /// the value-type `state` snapshot it captured at `.onAppear`: that single,
    /// in-place-updating element registers its read ONCE and never re-registers,
    /// so a captured-snapshot read stays `.idle` forever even after the manager
    /// flips to `.resolved` (the backup-destination rename bug class).
    /// Rendering still reads `state` (fresh on every body pass).
    public var liveResolveState: () -> ResolveState
    public var onInputChange: (String) -> Void
    public var onAcceptCurrent: () -> Void
    public var onAcceptSuggestion: (TypedAddress) -> Void

    public init(
        state: RecipientPickerState,
        liveResolveState: @escaping () -> ResolveState,
        onInputChange: @escaping (String) -> Void = { _ in },
        onAcceptCurrent: @escaping () -> Void = {},
        onAcceptSuggestion: @escaping (TypedAddress) -> Void = { _ in }
    ) {
        self.state = state
        self.liveResolveState = liveResolveState
        self.onInputChange = onInputChange
        self.onAcceptCurrent = onAcceptCurrent
        self.onAcceptSuggestion = onAcceptSuggestion
    }

    @State private var text: String = ""

    // State→(token, label) map is shared Rust (`recipientResolveStatus`) — do not
    // re-derive the text/token per arm here; only styling stays per-app.
    private func resolveToken(_ rs: ResolveState) -> String {
        recipientResolveStatus(state: rs).token
    }

    private func resolveText(_ rs: ResolveState) -> String {
        recipientResolveStatus(state: rs).label.map(renderLocalizedText) ?? ""
    }

    private func resolveColor(_ rs: ResolveState) -> Color {
        switch rs {
        case .error: .red
        case .resolved: .green
        default: .secondary
        }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                TextField(L.conversations.unified.recipientPickerPlaceholder, text: $text)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.recipientPickerInput)
                    .automationField(Ids.recipientPickerInput, text: $text)
                    // Report USER edits only. `text` is also written from the
                    // model (`.onAppear` and the `rawInput` resync below), and
                    // echoing that value back is not a no-op: the manager's
                    // `set_*_recipient_input` clears `resolved` and parks the
                    // picker on Resolving, so an echo landing after a probe
                    // has already confirmed the input silently un-resolves it
                    // and the accept that follows commits nothing. A value
                    // equal to the model's own is, by construction, the model's.
                    .onChange(of: text) { _, newValue in
                        if newValue != state.rawInput { onInputChange(newValue) }
                    }
                    .onSubmit { onAcceptCurrent() }
                Button(L.common.add) { onAcceptCurrent() }
                    .disabled(text.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            if !state.chips.isEmpty {
                HStack(spacing: 6) {
                    ForEach(Array(state.chips.enumerated()), id: \.offset) { _, chip in
                        automationText(Ids.recipientPickerChip, ConversationsUI.display(chip))
                            .font(.caption)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Color.accentColor.opacity(0.15), in: Capsule())
                    }
                }
            }

            // `recipient-picker-class` — the class of the room about to be
            // created, stated once a chip is committed and before the first
            // message goes out (`conversation-rooms.md` § The three classes).
            // Derived in shared Rust from the committed chips and the home-nest
            // choice; this only paints it, and emits nothing before a chip.
            if let prospective = roomProspectiveClass(chips: state.chips, includeHomeNest: state.includeHomeNest) {
                let classLabel = roomClassLabel(class: prospective)
                let classToken = roomClassAttrToken(class: prospective)
                Text(classLabel)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(Ids.recipientPickerClass)
                    .automationValue(Ids.recipientPickerClass, text: { classLabel }, value: { classToken })
            }

            ForEach(Array(state.suggestions.enumerated()), id: \.offset) { _, sugg in
                Button {
                    onAcceptSuggestion(sugg)
                } label: {
                    HStack {
                        Text(ConversationsUI.rail(for: sugg).map { ConversationsUI.glyphEmoji(railGlyph(rail: $0)) } ?? "")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        Text(ConversationsUI.display(sugg)).font(.caption)
                        Spacer()
                    }
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier(Ids.recipientPickerSuggestion)
                .automationActivate(Ids.recipientPickerSuggestion, value: { ConversationsUI.display(sugg) }) {
                    onAcceptSuggestion(sugg)
                }
            }

            Text(resolveText(state.resolveState))
                .font(.caption)
                .foregroundStyle(resolveColor(state.resolveState))
                .frame(minHeight: 14, alignment: .leading)
                .accessibilityElement()
                .accessibilityIdentifier(Ids.recipientResolveStatus)
                .accessibilityValue(resolveToken(state.resolveState))
                .accessibilityLabel(resolveText(state.resolveState))
                // Complex/selectable label carrying `.accessibilityValue` — keep the
                // Text + id and register the read: `text` = human label, `value` =
                // the machine state the driver reads off `.accessibilityValue`.
                // The registered closures MUST re-read the LIVE resolve state via
                // `liveResolveState()` (the parent's @Observable-vm read), NOT the
                // captured `state` snapshot: this single element updates IN PLACE
                // (Idle→Resolved), but `.onAppear` registers the closure ONCE, so a
                // snapshot read would report 'idle' forever in-process — the
                // backup-destination rename bug class.
                .automationValue(Ids.recipientResolveStatus,
                                 text: { resolveText(liveResolveState()) },
                                 value: { resolveToken(liveResolveState()) })
        }
        .onAppear { text = state.rawInput }
        .onChange(of: state.rawInput) { _, newValue in
            // Manager reset the input (e.g. after accepting a chip) — resync.
            if newValue != text { text = newValue }
        }
    }
}
