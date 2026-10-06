import SwiftUI

/// The admin **`admin-bridges-pending`** page (`docs/goal/behavior/mail-bridge-lifecycle.md`
/// § Pending approval), shared by macOS + iOS (one FaunaKit view, thin per-target
/// mount points). A dumb renderer of
/// `BridgeApprovalSnapshot.pending` / `.approved` + dispatcher of
/// `BridgeApprovalAction` over the shared `BridgeApprovalMachine` (via
/// `BridgeApprovalVM`); no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `admin-bridges-pending` / `admin-bridges-pending-card`
/// / `admin-bridges-approved-card` / `admin-bridges-rotate-confirm` exactly.
/// Reference renderers: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_pending_bridge_card`) + web
/// (`routes/admin/bridges-pending/+page.svelte`) + windows
/// (`AdminBridgesPendingPage`).
///
/// Each pending bridge renders one approval card: the requested role, the
/// hex pubkey (admin verifies it against the admin's expected fingerprint),
/// the source IP (advisory — a nest-side wire gap today, so it shows a dash), the
/// first-seen timestamp, and **Approve** / **Reject** buttons. Approve confirms
/// the enrolled role and lifts the bridge into the running phase; Reject drops it
/// (the bridge can re-connect with a fresh keypair) — a direct dispatch, matching
/// linux/web/windows (no confirmation dialog).
///
/// Below the pending cards, the **Approved bridges** roster (`admin.md`
/// § Approved-bridges roster) lists running-phase bridges, each with a rotate
/// button that reveals the `admin-bridges-rotate-confirm` inline confirmation
/// (an `.accessibilityElement(children: .contain)` overlay, NOT `.sheet`/`.alert`
/// — those system presentation layers don't reliably `.onAppear`-register in the
/// in-process e2e driver; `apple-e2e-automation.md` § Registration rules #3,
/// same shape as `MailSettingsView.disableConfirmSection`).
///
/// `admin-nav-back` is provided by the admin shell rail (macOS), not this page.
public struct AdminBridgesPendingView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = BridgeApprovalVM()
    /// The approved-bridge card whose rotate confirmation is open (`nil` = none).
    @State private var rotateTarget: ApprovedBridgeView?
    /// Reload trigger — macOS passes the shell's `navGeneration` so each
    /// (re)navigation re-reads the feed; iOS leaves it 0 (the NavigationLink
    /// re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.admin.bridgesPending.title)
                    .font(.title)
                    .accessibilityIdentifier(Ids.pageHeading)

                Text(L.admin.bridgesPending.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)

                sectionHeader(L.admin.bridgesPending.pendingSection)
                if vm.pending.isEmpty {
                    emptyState
                } else {
                    ForEach(Array(vm.pending.enumerated()), id: \.element.pubkeyHex) { _, view in
                        card(view)
                    }
                }

                sectionHeader(L.admin.bridgesPending.approvedSection)
                if vm.approved.isEmpty {
                    Text(L.admin.bridgesPending.approvedEmpty)
                        .font(.headline)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 8)
                } else {
                    ForEach(Array(vm.approved.enumerated()), id: \.element.pubkeyHex) { _, view in
                        approvedCard(view)
                    }
                }

                rotateConfirmSection

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

    // MARK: - Section headers

    /// One subheading separating the pending-approval cards from the approved
    /// roster ("Pending approval" / "Approved bridges"). No test id — a plain
    /// visual label, matching linux's `bridges_section_label` (not part of the
    /// spec's element set).
    private func sectionHeader(_ text: String) -> some View {
        Text(text)
            .font(.headline)
            .padding(.top, 4)
    }

    // MARK: - Empty state

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L.admin.bridgesPending.empty)
                .font(.headline)
                .foregroundStyle(.secondary)
            Text(L.admin.bridgesPending.emptyDesc)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 8)
    }

    // MARK: - One approval card

    @ViewBuilder
    private func card(_ view: PendingBridgeView) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            // Friendly per-role display name atop the card (admin.md § Bridge
            // display naming; bridges.md § Active bridges). The MDA serves IMAP +
            // CalDAV, the MTA serves SMTP only — so only the MDA names calendar.
            // The technical role string still renders below (it drives the
            // per-role allowlist applied on approve).
            automationText(Ids.adminBridgesPendingCardName,
                           renderLocalizedText(bridgeDisplayName(role: view.requestedRole)))
                .font(.headline)
            LabeledFieldRow(L.admin.bridgesPending.role, view.requestedRole,
                  id: "admin-bridges-pending-requested-role")
            LabeledFieldRow(L.admin.bridgesPending.pubkey, view.pubkeyHex,
                  id: "admin-bridges-pending-pubkey-hex", monospaced: true)
            // `source_ip` is a nest-side wire gap today (`ServiceUserInfo` carries
            // none) → render a placeholder dash; the ID stays so ui.yaml conforms.
            LabeledFieldRow(L.admin.bridgesPending.sourceIp,
                  view.sourceIp ?? L.admin.bridgesPending.sourceIpUnknown,
                  id: "admin-bridges-pending-source-ip")
            LabeledFieldRow(L.admin.bridgesPending.firstSeen, Self.formatFirstSeen(view.firstSeenAt),
                  id: "admin-bridges-pending-first-seen-at")

            HStack(spacing: 12) {
                Button(L.admin.bridgesPending.approve) {
                    Task { await vm.approve(pubkeyHex: view.pubkeyHex, role: view.requestedRole) }
                }
                .accessibilityIdentifier(Ids.adminBridgesPendingApproveButton)
                .automationActivate(Ids.adminBridgesPendingApproveButton,
                                    isEnabled: { !vm.isBusy }) {
                    Task { await vm.approve(pubkeyHex: view.pubkeyHex, role: view.requestedRole) }
                }
                .faunaGate("fauna.bridges.approve_pending_bridge")

                Button(L.admin.bridgesPending.reject, role: .destructive) {
                    Task { await vm.reject(pubkeyHex: view.pubkeyHex) }
                }
                .accessibilityIdentifier(Ids.adminBridgesPendingRejectButton)
                .automationActivate(Ids.adminBridgesPendingRejectButton,
                                    isEnabled: { !vm.isBusy }) {
                    Task { await vm.reject(pubkeyHex: view.pubkeyHex) }
                }
                .faunaGate("fauna.bridges.reject_pending_bridge")
            }
            .disabled(vm.isBusy)
            .padding(.top, 4)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminBridgesPendingCard)
        // Per-card presence entry so the flat in-process registry can `count`
        // pending cards (the container isn't a single control). The pubkey is
        // identity-stable per card (`ForEach(id: \.pubkeyHex)`), so the read is
        // an immutable per-row value, not a frozen mutable snapshot.
        .automationValue(Ids.adminBridgesPendingCard, text: { view.pubkeyHex })
    }

    // MARK: - One approved-bridge roster card

    @ViewBuilder
    private func approvedCard(_ view: ApprovedBridgeView) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            // Same friendly per-role display name as the pending card (shared
            // `bridge_display_name`).
            automationText(Ids.adminBridgesApprovedCardName,
                           renderLocalizedText(bridgeDisplayName(role: view.role)))
                .font(.headline)
            LabeledFieldRow(L.admin.bridgesPending.role, view.role,
                  id: "admin-bridges-approved-role")
            LabeledFieldRow(L.admin.bridgesPending.pubkey, view.pubkeyHex,
                  id: "admin-bridges-approved-pubkey-hex", monospaced: true)
            LabeledFieldRow(L.admin.bridgesPending.approvedAt, Self.formatApprovedAt(view.approvedAt),
                  id: "admin-bridges-approved-approved-at")

            HStack {
                Spacer()
                Button(L.admin.bridgesPending.rotate, role: .destructive) {
                    rotateTarget = view
                }
                .accessibilityIdentifier(Ids.adminBridgesApprovedRotateButton)
                .automationActivate(Ids.adminBridgesApprovedRotateButton,
                                    isEnabled: { !vm.isBusy }) {
                    rotateTarget = view
                }
            }
            .disabled(vm.isBusy)
            .padding(.top, 4)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminBridgesApprovedCard)
        .automationValue(Ids.adminBridgesApprovedCard, text: { view.pubkeyHex })
    }

    // MARK: - Rotate-service-user-key confirm (`admin-bridges-rotate-confirm`)

    /// Inline destructive confirm, opened by an approved card's rotate button
    /// (`rotateTarget` set). Option (b) — an inline overlay group rather than a
    /// system `.confirmationDialog`/`.sheet`, so every ui.yaml id attaches to a
    /// real, registered element (`apple-e2e-automation.md` § Registration rules
    /// #3; same shape as `MailSettingsView.disableConfirmSection`). Confirming dispatches the
    /// shared `BridgeApprovalAction.rotate` (`revoke_service_user`); cancel just
    /// closes the reveal.
    @ViewBuilder
    private var rotateConfirmSection: some View {
        if let target = rotateTarget {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.bridgesRotate.title)
                    .font(.headline)
                automationText(Ids.adminBridgesRotateWarningText, L.admin.bridgesRotate.warning)
                    .font(.callout)
                HStack(spacing: 12) {
                    Button(L.admin.bridgesRotate.confirm, role: .destructive) {
                        confirmRotate(pubkeyHex: target.pubkeyHex)
                    }
                    .accessibilityIdentifier(Ids.adminBridgesRotateConfirmButton)
                    .automationActivate(Ids.adminBridgesRotateConfirmButton) {
                        confirmRotate(pubkeyHex: target.pubkeyHex)
                    }
                    // Rotating a service-user key IS a revoke — the bridge exits
                    // and re-enrolls with the fresh key. Opening this confirm is
                    // local, so `admin-bridges-approved-rotate-button` stays live.
                    .faunaGate("fauna.bridges.revoke_service_user")
                    Button(L.admin.bridgesRotate.cancel, role: .cancel) { rotateTarget = nil }
                        .accessibilityIdentifier(Ids.adminBridgesRotateCancelButton)
                        .automationActivate(Ids.adminBridgesRotateCancelButton) { rotateTarget = nil }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
            // `.contain` keeps BOTH this container id AND the child warning/button
            // ids queryable — a bare container id on a VStack otherwise clobbers
            // every child id (same clobber guard as `disableConfirmSection`).
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.adminBridgesRotateConfirm)
        }
    }

    /// Dispatch the rotate action, then close the reveal. Shared by the confirm
    /// `Button` and its `.automationActivate` so the two never diverge (mirrors
    /// `MailSettingsView.confirmDisableMail`).
    private func confirmRotate(pubkeyHex: String) {
        Task {
            await vm.rotate(pubkeyHex: pubkeyHex)
            rotateTarget = nil
        }
    }

    // MARK: - Helpers

    /// Format an epoch-millis timestamp for `first-seen-at` (mirrors linux's
    /// `%Y-%m-%d %H:%M` / windows' `yyyy-MM-dd HH:mm` — a locale date+time).
    /// Routes through `ValueFormat.absoluteDate` — the same medium-date/short-time
    /// door every other apple absolute-timestamp surface uses, rather than a
    /// second hand-rolled `DateFormatter.localizedString` call.
    static func formatFirstSeen(_ unixMillis: UInt64) -> String {
        ValueFormat.absoluteDate(epochMs: Int64(unixMillis), withTime: true)
    }

    /// Format an optional epoch-millis `approved_at` — a dash for an absent value
    /// (a non-conforming nest; mirrors linux's reuse of the same `SOURCE_IP_UNKNOWN` dash).
    static func formatApprovedAt(_ unixMillis: UInt64?) -> String {
        guard let unixMillis else { return L.admin.bridgesPending.sourceIpUnknown }
        return formatFirstSeen(unixMillis)
    }
}
