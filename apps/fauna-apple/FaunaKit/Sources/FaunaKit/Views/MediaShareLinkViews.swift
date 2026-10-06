import SwiftUI

/// Share links on the Media page (`docs/goal/behavior/share-links.md` § Flows) —
/// shared macOS + iOS FaunaKit renderers for the `share-link-create-modal` a
/// detail's `share-link-button` opens, the page-level `share-link-list` with
/// its per-row copy / revoke, and the single `share-link-revoke-confirm-modal`.
///
/// Every piece of state — the create step, the URL revealed only after the
/// registration succeeded, the list's `loaded` bit and row states, the armed
/// revoke — lives in the shared `MediaMachine` (`share-links.md` § Where logic
/// lives); these views paint `MediaPageSnapshot.shareCreate` / `.shareLinks`
/// and forward gestures through `MediaMachineVM`. The surfaces are
/// **render-driven**: each is on screen exactly while its half of the snapshot
/// is open, so none outlives, nor runs ahead of, the machine state it shows.
/// tui's `share_create_elements` / `share_list_elements` and linux's
/// `views/media/share.rs` are the references.
///
/// Inline `ZStack` overlays, never `.sheet`s, so every id stays in the
/// in-process automation tree (`apple-e2e-automation.md` registration rule 3)
/// — the `MediaItemDetailView` idiom.

/// `share-link-button` — present only when the machine says the file is
/// eligible (`MediaItemSummary.shareLinkEligible`, already `false` for a
/// followed browse scope's items); ABSENT otherwise, never painted-but-inert
/// (`share-links.md` § Which files can be linked).
struct ShareLinkDetailButton: View {
    let vm: MediaMachineVM
    let item: MediaItemSummary

    var body: some View {
        if item.shareLinkEligible {
            Button(L.shareLink.button) { open() }
                .accessibilityIdentifier(Ids.shareLinkButton)
                .automationActivate(Ids.shareLinkButton) { open() }
        }
    }

    private func open() {
        vm.openShareCreate(folder: item.folder, path: item.path)
    }
}

/// The page-level `share-link-list-button` — open and load the caller's links
/// (`fauna.share.list`, a read, so no offline gate).
struct ShareLinkListButton: View {
    let vm: MediaMachineVM

    var body: some View {
        Button(L.shareLink.listButton) { open() }
            .accessibilityIdentifier(Ids.shareLinkListButton)
            .automationActivate(Ids.shareLinkListButton) { open() }
    }

    private func open() {
        Task { await vm.openShareLinks() }
    }
}

/// Every share surface the snapshot currently has open, stacked over the Media
/// page (and over an open `media-item-detail`, whose button opens the create
/// surface). Placed last in the page's `ZStack`.
struct ShareLinkSurfaces: View {
    let vm: MediaMachineVM

    var body: some View {
        let snap = vm.snapshot
        if let create = snap?.shareCreate {
            ShareLinkCreateModal(vm: vm, create: create,
                                 options: snap?.shareExpiryOptions ?? [])
        }
        if let list = snap?.shareLinks, list.open {
            ShareLinkListSurface(vm: vm, list: list)
        }
    }
}

// MARK: - Create

/// The `share-link-create-modal` (`share-links.md` § Flows → Create): the
/// expiry and Create until the registration succeeded, then the URL and its
/// Copy — never the URL before (the reveal-after-registration rule).
private struct ShareLinkCreateModal: View {
    let vm: MediaMachineVM
    let create: ShareCreateSnapshot
    let options: [String]

    var body: some View {
        let revealed = create.url
        ShareLinkPanel(onDismiss: { vm.closeShareCreate() }) {
            Text(L.shareLink.createTitle(name: create.name))
                .font(.headline)
                .lineLimit(2)
                .truncationMode(.middle)
            Text(L.shareLink.createBody)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)

            if let revealed {
                automationText(Ids.shareLinkUrl, revealed)
                    .font(.callout.monospaced())
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                HStack {
                    Text(L.shareLink.expiryLabel)
                    Spacer(minLength: 8)
                    expiryPicker
                }
            }

            ShareLinkOwnError(vm: vm, keys: ["share_link.error_create"])

            HStack {
                Spacer()
                Button(revealed == nil ? L.shareLink.cancel : L.shareLink.close) {
                    vm.closeShareCreate()
                }
                .accessibilityIdentifier(Ids.shareLinkCancelButton)
                .automationActivate(Ids.shareLinkCancelButton) { vm.closeShareCreate() }

                if let revealed {
                    CopyButton(Ids.shareLinkCopyButton, text: revealed)
                } else {
                    Button(create.busy ? L.shareLink.creating : L.shareLink.create) {
                        submit()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(create.busy)
                    .accessibilityIdentifier(Ids.shareLinkCreateButton)
                    .automationActivate(Ids.shareLinkCreateButton,
                                        isEnabled: { !create.busy }) { submit() }
                    .faunaGate("fauna.share.create")
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.shareLinkCreateModal)
        .automationValue(Ids.shareLinkCreateModal, text: { create.name })
    }

    /// `share-link-expiry-select` over the machine's option values, each
    /// labelled through the shared `share_link_expiry_label` map; the value
    /// stays the model key so the cross-app `select(id, "<value>")` holds.
    private var expiryPicker: some View {
        Picker("", selection: Binding(
            get: { create.expiry },
            set: { vm.setShareExpiry($0) }
        )) {
            ForEach(options, id: \.self) { value in
                Text(Self.expiryLabel(value)).tag(value)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.shareLinkExpirySelect)
        .automationSelect(Ids.shareLinkExpirySelect,
                          value: { create.expiry },
                          options: { options },
                          set: { vm.setShareExpiry($0) })
    }

    private func submit() {
        Task { await vm.createShareLink() }
    }

    static func expiryLabel(_ value: String) -> String {
        shareLinkExpiryLabel(value: value).map(renderLocalizedText) ?? value
    }
}

// MARK: - List

/// The `share-link-list` (`share-links.md` § Flows → List): three states off
/// one `loaded` bit (`ui/README.md` § List pages — loading is not empty), each
/// row with Copy where the shared re-derivation verified the URL and Revoke on
/// an Active row; the single revoke confirm sits inside it.
private struct ShareLinkListSurface: View {
    let vm: MediaMachineVM
    let list: ShareLinksSnapshot

    var body: some View {
        ShareLinkPanel(onDismiss: { vm.closeShareLinks() }) {
            HStack {
                Text(L.shareLink.listTitle)
                    .font(.headline)
                Spacer()
                Button(L.shareLink.close) { vm.closeShareLinks() }
                    .accessibilityIdentifier(Ids.shareLinkListCloseButton)
                    .automationActivate(Ids.shareLinkListCloseButton) { vm.closeShareLinks() }
            }

            if !list.loaded {
                Text(L.shareLink.listLoading)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else if list.rows.isEmpty {
                automationText(Ids.shareLinkEmptyState, L.shareLink.empty)
            } else {
                ScrollView {
                    VStack(spacing: 4) {
                        ForEach(Array(list.rows.enumerated()), id: \.offset) { offset, row in
                            ShareLinkRow(vm: vm, row: row)
                                .automationScope(Ids.shareLinkItem, index: offset)
                        }
                    }
                }
                .frame(maxHeight: 320)
            }

            ShareLinkOwnError(vm: vm, keys: ["share_link.error_list", "share_link.error_revoke"])

            if let armed = list.revokeConfirm {
                ShareLinkRevokeConfirm(
                    vm: vm,
                    name: list.rows.first(where: { $0.tokenId == armed })?.name ?? "")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.shareLinkList)
        .automationValue(Ids.shareLinkList, text: { L.shareLink.listTitle })
    }
}

/// One `share-link-item` row: name, expiry, state — plus Copy where the URL
/// re-derived and verified (absent otherwise, never a wrong link) and Revoke
/// on an Active row.
private struct ShareLinkRow: View {
    let vm: MediaMachineVM
    let row: ShareLinkSummary

    var body: some View {
        HStack(spacing: 12) {
            automationText(Ids.shareLinkItemName, row.name)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 8)
            automationText(Ids.shareLinkItemExpires,
                           L.shareLink.expires(date: formatUnixLocalDate(secs: row.expiresAt)))
                .font(.caption)
                .foregroundStyle(.secondary)
            // The label is paint-only; the `state` attribute carries the stable
            // value (`active` / `expired` / `revoked`) a test asserts on.
            Text(Self.stateLabel(row.state))
                .font(.caption)
                .accessibilityIdentifier(Ids.shareLinkItemState)
                .automationValue(Ids.shareLinkItemState,
                                 text: { Self.stateLabel(row.state) },
                                 value: { row.state },
                                 attributes: { ["state": row.state] })
            if let url = row.url {
                CopyButton(Ids.shareLinkItemCopyButton, text: url)
            }
            if row.state == "active" {
                Button(L.shareLink.revoke, role: .destructive) { arm() }
                    .accessibilityIdentifier(Ids.shareLinkRevokeButton)
                    .automationActivate(Ids.shareLinkRevokeButton) { arm() }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.shareLinkItem)
        // Presence anchor for the indexed row (one Entry per link, so
        // `count("share-link-item")` is the link count).
        .automationValue(Ids.shareLinkItem, text: { row.name })
    }

    private func arm() {
        vm.armShareRevoke(tokenId: row.tokenId)
    }

    static func stateLabel(_ state: String) -> String {
        shareLinkStateLabel(state: state).map(renderLocalizedText) ?? state
    }
}

/// The single `share-link-revoke-confirm-modal` — the file-delete confirm is
/// the precedent; a revoke destroys nothing but the link. Confirm leaves the
/// surface for the render to close: the gesture takes the armed token first
/// thing, so it is consumed by `confirmShareRevoke`, never cleared by a cancel.
private struct ShareLinkRevokeConfirm: View {
    let vm: MediaMachineVM
    let name: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.shareLink.revokeConfirmTitle)
                .font(.headline)
            Text(L.shareLink.revokeConfirmBody(name: name))
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button(L.common.cancel) { vm.cancelShareRevoke() }
                    .accessibilityIdentifier(Ids.shareLinkRevokeCancelButton)
                    .automationActivate(Ids.shareLinkRevokeCancelButton) { vm.cancelShareRevoke() }
                Button(L.shareLink.revokeConfirm, role: .destructive) { confirm() }
                    .accessibilityIdentifier(Ids.shareLinkRevokeConfirmButton)
                    .automationActivate(Ids.shareLinkRevokeConfirmButton) { confirm() }
                    .faunaGate("fauna.share.revoke")
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.shareLinkRevokeConfirmModal)
        .automationValue(Ids.shareLinkRevokeConfirmModal,
                         text: { L.shareLink.revokeConfirmTitle })
    }

    private func confirm() {
        Task { await vm.confirmShareRevoke() }
    }
}

// MARK: - Shared chrome

/// The dimmed backdrop + card every share surface paints in — the
/// `MediaItemDetailView` overlay shape. A backdrop tap is a user dismiss and
/// forwards the machine's own close gesture.
private struct ShareLinkPanel<Content: View>: View {
    let onDismiss: () -> Void
    @ViewBuilder let content: () -> Content

    var body: some View {
        ZStack {
            Color.black.opacity(0.35)
                .ignoresSafeArea()
                .onTapGesture { onDismiss() }
            VStack(alignment: .leading, spacing: 12) {
                content()
            }
            .padding()
            .frame(maxWidth: 460)
            .background(.background)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .shadow(radius: 20)
            .padding()
        }
    }
}

/// A surface's own error line. The share errors land in the page's
/// `error-message` (the machine's contract), but that banner sits behind these
/// overlays where it cannot be read — so each surface repeats ITS errors here,
/// untagged (the id stays the page's), and never an unrelated page error it did
/// not cause. linux's `own_error` is the same rule.
private struct ShareLinkOwnError: View {
    let vm: MediaMachineVM
    let keys: [String]

    var body: some View {
        if let error = vm.snapshot?.error, keys.contains(error.key) {
            Text(renderLocalizedText(error))
                .font(.caption)
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
