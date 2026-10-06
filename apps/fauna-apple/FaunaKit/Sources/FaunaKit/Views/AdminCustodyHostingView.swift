import SwiftUI

/// The admin **`admin-custody-hosting`** page (`docs/goal/architecture/account-data-plane.md`
/// § Two-sided bounds), shared by
/// macOS + iOS (one FaunaKit view, thin per-target mount points): the nest-wide
/// custody-hosting registry, with honest metering and a remove behind an
/// arm/confirm addressing the `(host, grant)` pair. A CONTEXTUAL detail page
/// like `admin-dns`/`admin-logs`/`admin-bridges-pending` — absent from
/// ui.yaml's `navigation.admin_pages`, present directly on apple's own admin
/// nav rail (`AdminPage.built`) so an admin can reach it.
///
/// Dumb renderer of the shared `AdminHostingClient` + `admin_hosting_rows` fold
/// over UniFFI (`AdminCustodyHostingVM` → `FfiAdminClient.custodyHostingList`/
/// `custodyHostingRemove`) — no projection logic here (priority #2). Mirrors
/// the linux leg (`apps/fauna-linux/src/views/admin.rs::build_custody_hosting_page`),
/// tui's lead (`apps/fauna-tui/src/admin/custody_hosting.rs`), and android's
/// `AdminCustodyHostingScreen.kt`.
///
/// **Two honesty properties, easy to get subtly wrong (carried from the tui
/// lead's own tests):** (1) an un-hydrated page paints NO count and NO empty
/// state, because "nobody asked this nest to hold anything" must never stand
/// in for "the read has not answered yet" (`vm.rows == nil` vs `.isEmpty`);
/// (2) a `retainedBytesCap` of `0` renders *Default* — the row carries no cap
/// and the pump substitutes one, so a printed `0 B` states the opposite of the
/// truth.
///
/// ⚠ **The Devices custody-held card + its remove button are NOT built here**
/// (the owning row's own text names both as a
/// separate piece) — same store-gated reason linux/web/android left it undone: the held-for-others
/// card the button would sit on needs the W3 (account-data-plane.md § Workstreams) account store, which no apple UI
/// reads yet (verified: zero `HeldCustody`/`custody_held` references in apple
/// app source outside generated FFI/i18n). This page — the ADMIN registry — is
/// independent of that and does not need the store.
public struct AdminCustodyHostingView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminCustodyHostingVM()
    /// The `(host, grant)` key of the row whose remove confirm is armed —
    /// mirrors android's single page-local `armedKey` (not per-row indexed
    /// state): opening a new row's confirm silently retargets it, and only the
    /// currently-armed row's own remove button disables itself.
    @State private var armedKey: String?
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.pageHeading, L.admin.custodyHosting.title)
                    .font(.title)

                Text(L.admin.custodyHosting.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)

                // Pre-hydrate paints neither count nor empty state.
                if let rows = vm.rows {
                    automationText(Ids.adminCustodyHostingCount, L.admin.custodyHosting.count(count: "\(rows.count)"))
                        .font(.headline)

                    if rows.isEmpty {
                        automationText(Ids.adminCustodyHostingEmpty, L.admin.custodyHosting.empty)
                            .font(.headline)
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 8)
                    } else {
                        ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                            hostingRow(row, index: index)
                        }
                    }

                    if let status = vm.status {
                        Text(status).font(.callout)
                    }
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    private static func key(hostActorId: String, grantId: Data) -> String {
        "\(hostActorId):\(grantId.hexString)"
    }

    // MARK: - One hosting-registry row

    @ViewBuilder
    private func hostingRow(_ row: FfiAdminHostingRow, index: Int) -> some View {
        let rowKey = Self.key(hostActorId: row.hostActorId, grantId: row.grantId)
        VStack(alignment: .leading, spacing: 6) {
            LabeledFieldRow(L.admin.custodyHosting.host, shortId(hex: row.hostActorId),
                  id: Ids.adminCustodyHostingHost, monospaced: true)
            LabeledFieldRow(L.admin.custodyHosting.owner, shortId(hex: row.ownerActorId),
                  id: Ids.adminCustodyHostingOwner, monospaced: true)
            LabeledFieldRow(L.admin.custodyHosting.url, row.ownerNestUrl,
                  id: Ids.adminCustodyHostingUrl, monospaced: true)
            // `0` renders *Default*, never `0 B` — the row carries no cap and
            // the pump substitutes one.
            LabeledFieldRow(L.admin.custodyHosting.budget,
                  row.retainedBytesCap == 0 ? L.admin.custodyHosting.budgetDefault
                    : ValueFormat.byteSize(UInt64(row.retainedBytesCap)),
                  id: Ids.adminCustodyHostingBudget)
            LabeledFieldRow(L.admin.custodyHosting.held, ValueFormat.byteSize(UInt64(row.heldBytes)),
                  id: Ids.adminCustodyHostingHeld)
            // A stopped row still holds its bytes — remove exists precisely
            // because stop alone does not free them.
            LabeledFieldRow("", row.stopped ? L.admin.custodyHosting.stopped : L.admin.custodyHosting.active,
                  id: Ids.adminCustodyHostingStopped)
            LabeledFieldRow("", receiptText(row.receiptState),
                  id: Ids.adminCustodyHostingReceipt)

            if armedKey == rowKey {
                removeConfirm(row, rowKey: rowKey)
            } else {
                HStack {
                    Spacer()
                    Button(L.admin.custodyHosting.remove, role: .destructive) {
                        armedKey = rowKey
                    }
                    .disabled(vm.isBusy)
                    .accessibilityIdentifier(Ids.adminCustodyHostingRemoveButton)
                    .automationActivate(Ids.adminCustodyHostingRemoveButton,
                                        isEnabled: { !vm.isBusy }) {
                        armedKey = rowKey
                    }
                }
                .padding(.top, 4)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        // The shared indexed-id shape (`ui.yaml`'s `admin-custody-hosting-row`,
        // read by index): the bare id plus the scope step, never a `-<index>`
        // suffix a shared driver cannot resolve.
        .accessibilityIdentifier(Ids.adminCustodyHostingRow)
        .automationValue(Ids.adminCustodyHostingRow, text: { rowKey })
        .automationScope(Ids.adminCustodyHostingRow, index: index)
    }

    /// The armed remove confirm. Only the CONFIRM dispatches
    /// `fauna.admin.custody_hosting.remove`; arming and cancelling touch
    /// nothing on the wire (tui maps both to no kind at all).
    @ViewBuilder
    private func removeConfirm(_ row: FfiAdminHostingRow, rowKey: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.admin.custodyHosting.removeConfirmTitle).font(.headline)
            Text(L.admin.custodyHosting.removeConfirmBody).font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Button(L.admin.custodyHosting.removeConfirm, role: .destructive) {
                    confirmRemove(row)
                }
                .accessibilityIdentifier(Ids.adminCustodyHostingRemoveConfirmButton)
                .automationActivate(Ids.adminCustodyHostingRemoveConfirmButton,
                                    isEnabled: { !vm.isBusy }) {
                    confirmRemove(row)
                }
                .faunaGate("fauna.admin.custody_hosting.remove")
                Button(L.admin.custodyHosting.removeCancel, role: .cancel) { armedKey = nil }
                    .accessibilityIdentifier(Ids.adminCustodyHostingRemoveCancelButton)
                    .automationActivate(Ids.adminCustodyHostingRemoveCancelButton,
                                        isEnabled: { !vm.isBusy }) { armedKey = nil }
            }
        }
        .padding(.top, 4)
    }

    private func confirmRemove(_ row: FfiAdminHostingRow) {
        armedKey = nil
        Task { await vm.remove(hostActorId: row.hostActorId, grantId: row.grantId) }
    }

    private func receiptText(_ state: FfiReceiptState) -> String {
        switch state {
        case .fresh: L.admin.custodyHosting.receiptFresh
        case .stale: L.admin.custodyHosting.receiptStale
        case .noReceiptYet: L.admin.custodyHosting.receiptNone
        }
    }

}
