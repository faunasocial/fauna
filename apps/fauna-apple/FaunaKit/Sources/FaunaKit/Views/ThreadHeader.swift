import SwiftUI

/// Thread header strip above the messages list — `thread-header` component.
/// Shared by the macOS + iOS conversations detail views.
///
/// Label + `protocol-icon`, participant chips (`thread-member-chip`, one per
/// participant — matches the Windows + Linux reference impls, which render the
/// full set; `test_thread_membership` asserts a chip per member; the chip's tap
/// is capability-split between reveal and remove — see `MemberChip`),
/// `thread-add-participant-button` (shown iff `capabilities.supportsMembershipChange`),
/// `thread-rename-button` (shown iff `capabilities.supportsRename` — MLS groups
/// only), and a `[⋯]` overflow with "Show full headers" for email-shaped
/// threads.
///
/// Note the asymmetry the spec calls out: capability-gated *compose* affordances
/// are *disabled* (never hidden, never rail-branched); the rename /
/// add-participant buttons are *hidden* when unsupported (a disabled "Rename" on
/// a 1:1 would be noise). The `protocol-icon` glyph is the only rail-derived
/// bit, and it's cosmetic.
///
/// **The room** (`conversation-rooms.md` § The three classes, § Roles and
/// authorization): where the rail models one (`detail.room` non-nil) the
/// header states its class (`thread-room-class`, with a `class` attribute) and
/// paints the policy editor's door (`thread-room-settings-button`, greyed —
/// never hidden — unless `capabilities.canSetPolicy`); each chip carries its
/// member's role (`role` attribute + the localized owner/admin mark). All of it
/// is read off the projected room and the shared-Rust label/token twins, never
/// computed here; the roles table itself becomes `capabilities.*` in shared Rust
/// (`RoomSnapshot::gate`), so the add-participant affordance and the chip's
/// remove grey off `canInvite` / `canRemoveMembers` without this view ever
/// branching on a role. Where the rail models no room, both room elements are
/// ABSENT (not emitted), which is what a driver reads as absence.
public struct ThreadHeader: View {
    public let detail: ThreadDetail
    /// The post-succession review mark each chip carries — index-parallel
    /// with `detail.participants`/`detail.participantDisplays`
    /// (`APIClient.memberReviewMarksForThread`'s own contract). The
    /// flagged person's actor id at a chip under open review, `nil`
    /// elsewhere. Empty (the default) paints no marks at all — the caller
    /// computes this off its own cached roster
    /// (`succession-aftermath.md` § Propagation).
    public var marks: [Data?]
    public var onAddParticipant: () -> Void
    public var onRename: () -> Void
    /// `thread-room-settings-button` — open the room policy editor
    /// (`RoomSettingsSheet`, hosted by `ThreadDetailView`).
    public var onRoomSettings: () -> Void
    /// `thread-member-keep-button` — closes every open review item for this
    /// person with no group changes (`fauna_client_config::decide_member_review`
    /// via `APIClient.memberReviewKeep`). Remove is not rendered beside it —
    /// the chip the button sits inside already IS the removal affordance on a
    /// membership-change-capable thread (`onRemoveMember` below), matching
    /// linux/tui/web/android's identical scope for this row.
    public var onKeepMember: (Data) -> Void
    /// `thread-member-chip[i]` on a `supports_membership_change` thread —
    /// remove that participant (`manager.remove_participant`, an MLS Commit
    /// with no Welcome). Never fired on a rail that cannot change membership:
    /// there the chip carries no removal gesture at all, so this is
    /// unreachable rather than merely disabled — linux's rule
    /// (`thread_header.rs::render`) and tui's (`Action::RemoveMember`).
    public var onRemoveMember: (TypedAddress) -> Void

    public init(
        detail: ThreadDetail,
        marks: [Data?] = [],
        onAddParticipant: @escaping () -> Void = {},
        onRename: @escaping () -> Void = {},
        onRoomSettings: @escaping () -> Void = {},
        onKeepMember: @escaping (Data) -> Void = { _ in },
        onRemoveMember: @escaping (TypedAddress) -> Void = { _ in }
    ) {
        self.detail = detail
        self.marks = marks
        self.onAddParticipant = onAddParticipant
        self.onRename = onRename
        self.onRoomSettings = onRoomSettings
        self.onKeepMember = onKeepMember
        self.onRemoveMember = onRemoveMember
    }

    private var caps: ThreadCapabilities { detail.capabilities }
    private var isEmailShaped: Bool { detail.rail == .smtp }

    /// The member's role on a governed room — the chip's `role` attribute and
    /// its owner/admin mark. `nil` on a policy-less room and on every non-room
    /// thread: there are no roles to mark, not "everyone is a member".
    private func memberRole(_ index: Int) -> RoomRole? {
        guard let members = detail.room?.members, index < members.count else { return nil }
        return members[index].role
    }

    /// The chip label for participant `index`: its contact display name when we
    /// have one, else the canonical address. The full address is always revealed
    /// on tap (`MemberChip`), so a contact-name chip stays informative.
    private func memberDisplay(_ index: Int, _ addr: TypedAddress) -> String {
        let displays = detail.participantDisplays
        if index < displays.count, !displays[index].isEmpty { return displays[index] }
        return ConversationsUI.display(addr)
    }

    /// Display derivation of the thread label — a blank/whitespace-only label
    /// renders the canonical localized `conversations.detail.no_subject`, a real
    /// label rides verbatim (resolves to itself on an i18n miss). The raw
    /// `detail.label` stays the rename value. Shared `thread_label_display`
    /// (conversations.md § Where logic lives) — one source of truth, replacing the
    /// untranslated `"(no label)"` literal this client hardcoded.
    private var displayLabel: String {
        renderLocalizedText(threadLabelDisplay(label: detail.label))
    }

    public var body: some View {
        HStack(spacing: 8) {
            Text(ConversationsUI.glyphEmoji(detail.glyph))
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.protocolIcon)
                // Read-only glyph — expose the D5 concept emoji as a stable
                // presence/value read.
                .automationValue(Ids.protocolIcon, text: { ConversationsUI.glyphEmoji(detail.glyph) })

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(displayLabel)
                        .font(.headline)
                        .lineLimit(1)
                    // The room's class, stated on the header ("the class is on
                    // the thread header", `conversation-rooms.md` § The three
                    // classes). Not emitted at all where the rail models no room.
                    if let room = detail.room {
                        let classLabel = roomClassLabel(class: room.class)
                        let classToken = roomClassAttrToken(class: room.class)
                        Text(classLabel)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .accessibilityIdentifier(Ids.threadRoomClass)
                            // `text` = the stated sentence; the `class` attribute
                            // (like every attribute a driver names) reads `value`.
                            .automationValue(Ids.threadRoomClass, text: { classLabel }, value: { classToken })
                    }
                }
                memberChips
            }

            Spacer()

            if caps.supportsMembershipChange {
                // Shown by the rail's capability, GREYED by the viewer's role in
                // a governed room (`canInvite` — the roles table applied in shared
                // Rust; `ui/conversations.md` § Architectural rules 5).
                Button {
                    onAddParticipant()
                } label: { Image(systemName: "person.badge.plus") }
                    .buttonStyle(.borderless)
                    .help(L.conversations.unified.threadAddParticipant)
                    .disabled(!caps.canInvite)
                    .accessibilityIdentifier(Ids.threadAddParticipantButton)
                    .automationActivate(Ids.threadAddParticipantButton, isEnabled: { caps.canInvite }) {
                        onAddParticipant()
                    }
            }

            // The policy editor's door — painted whenever the thread is a room,
            // greyed unless the viewer may set policy (a policy-less room greys it
            // too, having no policy to edit).
            if detail.room != nil {
                Button {
                    onRoomSettings()
                } label: { Image(systemName: "slider.horizontal.3") }
                    .buttonStyle(.borderless)
                    .help(L.conversations.unified.threadRoomSettings)
                    .disabled(!caps.canSetPolicy)
                    .accessibilityIdentifier(Ids.threadRoomSettingsButton)
                    .automationActivate(Ids.threadRoomSettingsButton, isEnabled: { caps.canSetPolicy }) {
                        onRoomSettings()
                    }
            }

            if caps.supportsRename {
                Button {
                    onRename()
                } label: { Image(systemName: "pencil") }
                    .buttonStyle(.borderless)
                    .help(L.conversations.unified.threadRename)
                    .accessibilityIdentifier(Ids.threadRenameButton)
                    .automationActivate(Ids.threadRenameButton) { onRename() }
            }

            if isEmailShaped {
                Menu {
                    Button(L.conversations.unified.showFullHeaders) { /* per-message raw headers — follow-on */ }
                } label: { Image(systemName: "ellipsis") }
                    .modifier(BorderlessMenuOnMac())
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.threadHeader)
        // Container presence anchor — read the painted thread label (the same
        // shared display derivation as the title Text, so the automation read
        // matches what's shown).
        .automationValue(Ids.threadHeader, text: { displayLabel })
    }

    private var memberChips: some View {
        // Full participant set — no cap. The Windows + Linux reference impls
        // render every member, and `test_thread_membership` asserts as much.
        //
        // The chip's gesture is CAPABILITY-SPLIT, never rail-branched — linux's
        // exact rule (`views/conversations/thread_header.rs::render`) and tui's
        // (`conversations/mod.rs`: a `gesture_button` under the capability, a
        // plain `label` otherwise). `supports_membership_change` is false for
        // mail — you cannot un-send who an email went to — so a mail chip is
        // informational and tapping reveals the full address; on FaunaMls
        // membership is real, so the chip IS the removal affordance
        // (`conversations.md` § Participants vs reply recipients, and the
        // `thread-member-chip[i]` row of § the element table).
        //
        // The removal half is additionally ROLE-gated: on a governed room a
        // plain member's chip is still the Remove control, but greyed
        // (`canRemoveMembers` false) — never demoted to the address popover,
        // which a user and a driver both read as a live control
        // (`ui/conversations.md` § Architectural rules 5; linux's
        // `thread_header.rs::render` `greyed` split, the fix its tier_3 run
        // found).
        HStack(spacing: 4) {
            ForEach(Array(detail.participants.enumerated()), id: \.offset) { i, addr in
                let role = memberRole(i)
                MemberChip(
                    text: roomMemberChipText(display: memberDisplay(i, addr), role: role),
                    roleToken: role.map { roomRoleAttrToken(role: $0) },
                    address: ConversationsUI.display(addr),
                    removal: !caps.supportsMembershipChange
                        ? .none
                        : (caps.canRemoveMembers ? .live : .greyed),
                    flaggedPerson: i < marks.count ? marks[i] : nil,
                    onKeep: onKeepMember,
                    onRemove: { onRemoveMember(addr) }
                )
                // Scope path for `thread-member-unattested-mark`/
                // `thread-member-keep-button`, scoped INSIDE thread-member-chip[i]
                // (ui.yaml's own words) so scoped child reads resolve.
                .automationScope(Ids.threadMemberChip, index: i)
            }
        }
    }
}

/// One `thread-member-chip`. Shows the contact display name (or address) —
/// with the owner/admin mark on a governed room (`roomMemberChipText`). Its
/// tap is **capability-split**, and the halves are mutually exclusive by
/// design (`conversations.md` § the `thread-member-chip[i]` row):
///
/// - `.live` (`supports_membership_change` — FaunaMls, where group membership
///   is real — AND `can_remove_members`): the tap REMOVES that participant
///   through `manager.remove_participant`.
/// - `.greyed` (membership is real but the viewer's ROLE may not remove — a
///   plain member of a governed room): still the Remove control, disabled —
///   the automation gate refuses to drive it (convention 11's named 409), and
///   it is never demoted to the address reveal below.
/// - `.none` (mail: you cannot un-send who an email went to): the tap reveals
///   the full canonical address in a popover (`.popover` adapts to a popover
///   even in iOS compact width, matching the linux GTK popover) and never
///   removes.
///
/// ⚠ **The reveal is not offered on a removal chip, and that is the point.**
/// One target cannot carry both a benign inspect and a destructive eviction —
/// whichever fires second would be a surprise. linux and tui make the same
/// either/or choice; this is parity with them, not an apple simplification.
///
/// The chip's automation read: `text` is the painted chip text; the `role`
/// attribute reads `value` — the role token on a governed room, `nil` (so the
/// read falls back to the text, as before) elsewhere.
///
/// While `flaggedPerson` is non-nil the chip additionally carries the
/// post-succession review pair, scoped INSIDE it (ui.yaml's own words):
/// `thread-member-unattested-mark` (a caption, never an alert — this is a
/// review, not an accusation) + `thread-member-keep-button`. Keep is a
/// `Button`, so its tap is consumed there and never reaches the chip's own
/// removal gesture — the Keep half of the pair must never evict the person it
/// is keeping. Remove is deliberately not re-rendered beside it: the chip
/// already is it (`succession-aftermath.md` § Propagation → *MLS groups*).
/// `.accessibilityElement(children: .contain)` keeps both child ids
/// queryable alongside the chip's own container id (the memory'd
/// Section-clobbers-children rule).
private struct MemberChip: View {
    enum Removal { case live, greyed, none }

    let text: String
    let roleToken: String?
    let address: String
    let removal: Removal
    let flaggedPerson: Data?
    let onKeep: (Data) -> Void
    let onRemove: () -> Void
    @State private var showAddress = false

    /// The chip's painted content — identical on every half of the split, so
    /// the capability decides only the GESTURE, never how a member looks; a
    /// greyed Remove dims its own text only. NOT `.disabled`: that would
    /// disable the review pair's Keep inside it too, which is a succession-ledger
    /// write rather than a removal and stays live (linux's rule).
    private var chipContent: some View {
        HStack(spacing: 4) {
            Text(text)
                .font(.caption2)
                .foregroundStyle(removal == .greyed ? .secondary : .primary)
            if flaggedPerson != nil {
                automationText(
                    Ids.threadMemberUnattestedMark, L.conversations.detail.memberUnattestedMark)
                    .font(.caption2)
                    .foregroundStyle(.orange)
                Button(L.conversations.detail.memberKeep) {
                    if let person = flaggedPerson { onKeep(person) }
                }
                .font(.caption2)
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.threadMemberKeepButton)
                .automationActivate(Ids.threadMemberKeepButton) {
                    if let person = flaggedPerson { onKeep(person) }
                }
            }
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Color.secondary.opacity(0.15), in: Capsule())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.threadMemberChip)
    }

    @ViewBuilder
    var body: some View {
        switch removal {
        case .live, .greyed:
            let live = removal == .live
            chipContent
                .onTapGesture { if live { onRemove() } }
                // Indexed chip — read the chip text (and the role token);
                // activation mirrors the tap, which on this half is the
                // REMOVAL. That equivalence is what lets an e2e journey drive
                // the eviction the way a user does (convention 8) instead of
                // reaching past the UI — and, greyed, what makes the gate
                // refuse it the way the paint does.
                .automationActivate(
                    Ids.threadMemberChip,
                    isEnabled: { live },
                    text: { text },
                    value: { roleToken }
                ) { onRemove() }
                // ⚠ **Deliberately NOT `.faunaGate`d.** `remove_participant`
                // posts its MLS Commit through `fauna.conversations.channel.send`,
                // which is **OfflineQueued** — and `account-data-plane.md`
                // § The offline-mutation contract, ruling 1 greys only the
                // OnlineOnly class, precisely because classes 1 and 2 are the
                // ones that work without a nest. So this chip must stay LIVE
                // offline, exactly as the send button beside it does; the
                // removal queues. Gating it here would be a no-op that reads
                // as protection (`offline-gate-check` rejects the declaration
                // for that reason), and a *working* gate would be worse — it
                // would block a mutation the outbox is built to carry. linux's
                // `declare_wire_kind` on the same chip is a different
                // mechanism: it registers the kind for the shared rule, which
                // then decides; apple's modifier asserts the control should
                // desensitize.
        case .none:
            chipContent
                .onTapGesture { showAddress = true }
                // `revealed`: the full address the popover is showing, empty while it
                // is closed — the read that lets the chip witness assert the tap
                // REVEALS the address, where a chip naming a contact does not paint it.
                .automationActivate(
                    Ids.threadMemberChip,
                    text: { text },
                    value: { roleToken },
                    attributes: { ["revealed": showAddress ? address : ""] }
                ) {
                    showAddress = true
                }
                .popover(isPresented: $showAddress) {
                    Text(address)
                        .font(.caption)
                        .textSelection(.enabled)
                        .padding(8)
                        .presentationCompactAdaptation(.popover)
                }
        }
    }
}

/// `.menuStyle(.borderlessButton)` is macOS-only (`BorderlessButtonMenuStyle`);
/// on iOS the default menu style is right. Folded into a modifier so the call
/// site stays one expression on both platforms.
private struct BorderlessMenuOnMac: ViewModifier {
    func body(content: Content) -> some View {
        #if os(macOS)
        content.menuStyle(.borderlessButton).fixedSize()
        #else
        content
        #endif
    }
}
