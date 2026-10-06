import SwiftUI

/// The user-facing **Nests** page (`docs/goal/ui/nests.md`; renamed from
/// `linked-nests` 2026-07-07 — the ui.yaml page id + element ids moved to a
/// uniform `nests-*` prefix, the nav sub-page id followed 2026-10-02), shared
/// by macOS + iOS (one FaunaKit view, thin per-target call sites). A dumb
/// renderer of `LinkedNestsSnapshot` (the home/connected nest first, then
/// pairings) + dispatcher of `LinkedNestsAction` (Link / Unlink / Mint / Renew /
/// Revoke / SetLens) over the shared `LinkedNestsMachine` (via `LinkedNestsVM`);
/// no pairing or trust logic here. Below each nest's identity line sits a v1
/// **nest-trust facet**: a per-row Now/History lens over that nest's
/// content-processing grants, with mint (scope-first, holder-derived) / renew /
/// revoke. Element IDs match `tests/e2e-unified/ui.yaml` `nests` / `nests-item` /
/// `nest-trust-grant-item` exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/linked_nests.rs` (the lead app).
///
/// On macOS this renders as an inline section in `PreferencesView` (single-scroll
/// settings); on iOS it is a Settings sub-page (NavigationLink). The add-a-nest
/// surface is an **inline reveal** (not a sheet) — matching the ui.yaml IDs
/// (`nests-add-button` reveals it; no `-sheet-` IDs) and the linux idiom.
public struct LinkedNestsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = LinkedNestsVM()

    @State private var showingAddForm = false
    @State private var addInput = ""

    /// Each platform's own `appState.navGeneration`, bumped on every nav patch:
    /// a *re*-navigation to the already-mounted page re-reads it (the
    /// `DevicesView` idiom) — the custody rows' receipt freshness and the escrow
    /// badge are read on this hydrate edge, so a page that stayed mounted would
    /// otherwise never see a receipt the custodian nest deposited since.
    private let reloadToken: Int

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        // A plain, EAGER `ScrollView { VStack }` — NOT `Form(.grouped)`. A
        // `Form(.grouped)` Section backs a `ForEach` of MIXED-shape rows (the
        // home row omits Unlink/caps/expiry; pairing rows carry them) with
        // List-style (NSTableView) row recycling on macOS, which reordered a
        // nested leaf's `.onAppear` relative to its row's array position once
        // the row count changed (the `nests-item-nest-id` in-process-registry
        // occurrence index came out as [pairing, home] instead of array order
        // [home, pairing], corrupting `test_link_list_unlink_round_trip`'s
        // occurrence-index read). Same root cause + fix shape as
        // `MacCalendarListView`'s eager rewrite and
        // `BackupDestinationsView`'s frame-height fix: give every row an
        // EAGER, non-recycled mount so `.onAppear` fires exactly once, in
        // array order, and stays that way. Accepted minor production-UI
        // tradeoff: the page loses native Form/List grouped-row chrome.
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                headerSection
                if showingAddForm {
                    addFormSection
                }
                if let queue = vm.forwardQueue, ForwardQueueBlockView.isVisible(queue) {
                    ForwardQueueBlockView(status: queue, vm: vm)
                }
                listSection
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
        .pageTitle(L.nests.title)
        .task(id: reloadToken) {
            guard let client else { return }
            // Builds the machine on first mount and hydrates on EVERY run of the
            // task, so a bumped token re-reads the mounted page too.
            await vm.refresh(api: client.api)
        }
        // The machine is pull-based and the nest's outbox worker stamps a failed
        // send's reason (and later delivers) on its own clock, so while the
        // queue is non-empty keep re-reading it — otherwise a page the user
        // leaves open keeps painting the first frame it hydrated. Hosted on the
        // always-present scroll view (a `.task(id:)` on a conditionally-absent
        // view never fires); the id flips to false when the queue drains, which
        // cancels the loop.
        .task(id: (vm.forwardQueue?.queued ?? 0) > 0) {
            guard (vm.forwardQueue?.queued ?? 0) > 0 else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(Self.forwardQueueRefreshSeconds))
                if Task.isCancelled { break }
                await vm.hydrate()
            }
        }
    }

    /// How often the open page re-reads a non-empty forward queue.
    private static let forwardQueueRefreshSeconds = 5

    // MARK: - Header (description + add button)

    private var headerSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.nests.description)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button(L.nests.addButton) {
                beginAddForm()
            }
            .accessibilityIdentifier(Ids.nestsAddButton)
            .automationActivate(Ids.nestsAddButton, isEnabled: { !vm.isWorking }) { beginAddForm() }
            .disabled(vm.isWorking)
        }
    }

    // MARK: - Add form (inline reveal: input + submit/cancel)

    private var addFormSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField(L.nests.addInputPlaceholder, text: $addInput)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.nestsAddInput)
                .automationField(Ids.nestsAddInput, text: $addInput)
                .onSubmit { submit() }
            HStack {
                Button(L.nests.addSubmit) { submit() }
                    .disabled(addInput.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier(Ids.nestsAddSubmitButton)
                    .automationActivate(
                        Ids.nestsAddSubmitButton,
                        isEnabled: { !addInput.trimmingCharacters(in: .whitespaces).isEmpty }
                    ) { submit() }
                    // `LinkedNestsAction::Link` → `fauna.pair.add`. Arming is
                    // local: `nests-add-button` reveals this form and stays
                    // live, as do the input and the cancel beside it.
                    .faunaGate("fauna.pair.add")
                Button(L.nests.addCancel, role: .cancel) {
                    showingAddForm = false
                }
                .accessibilityIdentifier(Ids.nestsAddCancelButton)
                .automationActivate(Ids.nestsAddCancelButton) { showingAddForm = false }
            }
            .controlSize(.small)
        }
    }

    // MARK: - Nest list (home first, then pairings)

    private var listSection: some View {
        let rows = vm.rows
        return VStack(alignment: .leading, spacing: 12) {
            if rows.isEmpty && vm.custodyNestRows.isEmpty {
                Text(L.nests.empty).foregroundStyle(.secondary)
            } else if !rows.isEmpty {
                ForEach(Array(rows.enumerated()), id: \.element.nestId) { index, row in
                    NestItemRow(row: row, index: index, vm: vm, holdsEscrow: vm.holdsEscrow(row.nestId))
                }
            }
            // The custodian-NEST rows (`nests.md` § Trust facet — custody rows):
            // one `nests-item` per nest-anchored custody, AFTER the linked rows,
            // so the `nests-item[i]` scope step continues the linked rows' count.
            ForEach(Array(vm.custodyNestRows.enumerated()), id: \.offset) { offset, row in
                CustodyNestItemRow(row: row, index: rows.count + offset, vm: vm)
            }
        }
    }

    // MARK: - Helpers

    /// Reveal the inline add form. Shared by the header `Button` and its
    /// `.automationActivate` so the two never diverge (convention:
    /// apple-e2e-automation.md § registration ergonomics).
    private func beginAddForm() {
        addInput = ""
        showingAddForm = true
    }

    private func submit() {
        let raw = addInput.trimmingCharacters(in: .whitespaces)
        guard !raw.isEmpty else { return }
        Task {
            await vm.submitLink(raw)
            // Close the form only when the link succeeded (no error came back),
            // mirroring the linux render.
            if vm.errorMessage == nil { showingAddForm = false }
        }
    }
}

// MARK: - Forward queue (page-level block, after the add form)

/// The user's own post-forward queue (`nests-forward-queue`; `nests.md` § Forward
/// queue, ruling `private-mode.md` § Post Forwarding — the queue is the user's to
/// see). Rendered only while `queued > 0`: the count line (with the stuck clause
/// appended when `stuck > 0`), the relay's last failure reason (ABSENT, never
/// empty, when none was recorded), and Retry / Discard over the shared machine.
///
/// The reason is partly relay-chosen text, so it is painted as **plain text**:
/// `Text(String)` (the `StringProtocol` overload, verbatim) fed a runtime
/// `String` — never a `LocalizedStringKey` literal, which parses markdown and
/// auto-links, and never `AttributedString(markdown:)`. The shared
/// `ForwardQueueStatus` already control-strips it.
struct ForwardQueueBlockView: View {
    let status: ForwardQueueStatus
    let vm: LinkedNestsVM

    /// The block exists only while something is queued.
    static func isVisible(_ status: ForwardQueueStatus) -> Bool { status.queued > 0 }

    /// The count line, with the stuck clause appended when any entry is stuck.
    static func summaryText(of status: ForwardQueueStatus) -> String {
        let summary = L.nests.forwardQueueSummary(count: String(status.queued))
        guard status.stuck > 0 else { return summary }
        return "\(summary) \(L.nests.forwardQueueStuck(count: String(status.stuck)))"
    }

    /// The reason line, or `nil` (element absent) when no failure is recorded.
    static func reasonText(of status: ForwardQueueStatus) -> String? {
        guard let error = status.lastError, !error.isEmpty else { return nil }
        return L.nests.forwardQueueLastError(error: error)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            automationText(Ids.nestsForwardQueue, Self.summaryText(of: status))
                .font(.callout)
            if let reason = Self.reasonText(of: status) {
                automationText(Ids.nestsForwardQueueReason, reason)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            HStack(spacing: 8) {
                Button(L.nests.forwardRetry) {
                    Task { await vm.dispatch(.retryForwards) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestsForwardRetryButton)
                .automationActivate(Ids.nestsForwardRetryButton) {
                    Task { await vm.dispatch(.retryForwards) }
                }
                .faunaGate("fauna.pair.forward_retry")
                Button(L.nests.forwardDiscard, role: .destructive) {
                    Task { await vm.dispatch(.discardForwards) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestsForwardDiscardButton)
                .automationActivate(Ids.nestsForwardDiscardButton) {
                    Task { await vm.dispatch(.discardForwards) }
                }
                .faunaGate("fauna.pair.forward_discard")
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
    }
}

// MARK: - One nest row (identity line + trust facet)

/// One `nests-item` row: the identity line (label, abbreviated nest id, sync
/// capabilities, expiry, Unlink — all skipped for the home row, which is the
/// user's own connected nest, not a pairing), then the trust facet
/// (`NestTrustFacetView`). `nests.md` § Layout.
private struct NestItemRow: View {
    let row: LinkedNestRow
    /// Position in the rendered list — the `nests-item[i]` scope step.
    let index: Int
    let vm: LinkedNestsVM
    /// This nest holds the account's generation-key escrow (recorded escrow
    /// receipts, never the nest's own assertion) — paints the role badge.
    let holdsEscrow: Bool

    var body: some View {
        // Shared short-id formatter (UniFFI `shortId` — matches the Rust
        // `fauna_core::format::short_id` linux renders).
        let abbreviated = shortId(hex: row.nestId)
        let labelText = row.label.flatMap { $0.isEmpty ? nil : $0 } ?? abbreviated
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.nestsItemLabel, labelText)
                .font(.headline)
            automationText(Ids.nestsItemNestId, abbreviated)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .foregroundStyle(.secondary)
            // The escrow-holder role badge (`participants.md` § The participant
            // model → Roles): a text badge, derived from recorded escrow
            // receipts read on the hydrate edge.
            if holdsEscrow {
                automationText(Ids.participantEscrowHolderBadge, L.nests.escrowHolderBadge)
                    .font(.caption)
                    .foregroundStyle(.tint)
            }

            if !row.isHome {
                let caps = row.capabilities.joined(separator: ", ")
                automationText(Ids.nestsItemCapabilities, "\(L.nests.capabilitiesLabel) \(caps)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                let expiry = row.expiresAt == nil ? L.nests.expiryNever : L.nests.expiryLabel
                automationText(Ids.nestsItemExpiry, expiry)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button(L.nests.unlink, role: .destructive) {
                    Task { await vm.dispatch(.unlink(nestId: row.nestId)) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestsItemUnlinkButton)
                .automationActivate(Ids.nestsItemUnlinkButton) {
                    Task { await vm.dispatch(.unlink(nestId: row.nestId)) }
                }
                .faunaGate("fauna.pair.revoke")
            }

            NestTrustFacetView(row: row, vm: vm)
        }
        .padding(10)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nestsItem)
        // Per-row presence entry so the in-process registry can `count`
        // `nests-item` rows (the container isn't a single control; the flat
        // registry needs one entry keyed by the row id per row).
        .automationValue(Ids.nestsItem, text: { labelText })
        // The first step of the two-step scope chain a grant leaf lives under
        // (`nests-item[i]/nest-trust-grant-item[j]`, `nests.md` § Layout; the
        // harness's `_grant_scope`). Without it a scoped read of a CONDITIONAL
        // leaf — the review mark, rendered only on raised grants — falls back
        // to a flat occurrence index and answers for the wrong grant.
        .automationScope(Ids.nestsItem, index: index)
    }
}

// MARK: - Trust facet (per-row Now/History lens + grants/history + mint)

/// The trust facet for one nest row: the always-present Now/History lens toggle
/// (`nests.md` § Layout — "a per-row facet, not a page-level toggle"), then the
/// active lens's content — the grant list (Now, with a `nest-trust-empty` state
/// when the nest holds none) plus the mint flow when the row's mint-option
/// catalog is non-empty, or the grant-event timeline (History). `SetLens` is a
/// real machine dispatch (no nest round-trip; local log re-fold) so `row.lens`
/// always reflects the last dispatched lens.
private struct NestTrustFacetView: View {
    let row: LinkedNestRow
    let vm: LinkedNestsVM

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Button(L.nests.viewNow) {
                    Task { await vm.dispatch(.setLens(nestId: row.nestId, lens: .now)) }
                }
                .controlSize(.small)
                .fontWeight(row.lens == .now ? .semibold : .regular)
                .accessibilityIdentifier(Ids.nestTrustViewNow)
                .automationActivate(Ids.nestTrustViewNow) {
                    Task { await vm.dispatch(.setLens(nestId: row.nestId, lens: .now)) }
                }
                Button(L.nests.viewHistory) {
                    Task { await vm.dispatch(.setLens(nestId: row.nestId, lens: .history)) }
                }
                .controlSize(.small)
                .fontWeight(row.lens == .history ? .semibold : .regular)
                .accessibilityIdentifier(Ids.nestTrustViewHistory)
                .automationActivate(Ids.nestTrustViewHistory) {
                    Task { await vm.dispatch(.setLens(nestId: row.nestId, lens: .history)) }
                }
            }

            switch row.lens {
            case .now:
                // `nest-trust-empty` means "this nest is trusted with nothing", so a
                // backup row suppresses it even with zero content grants
                // (`nests.md:99`, ratified + user-approved) — a nest that seals and
                // uploads your messages is plainly trusted, and rendering "not
                // trusted to read anything" directly above "Backs up your messages
                // for you" would state the opposite of the row beneath it.
                if row.trustGrants.isEmpty && row.trustBackups.isEmpty {
                    automationText(Ids.nestTrustEmpty, L.nests.notTrusted)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else if !row.trustGrants.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(row.trustGrants.enumerated()), id: \.element.grantId) { index, grant in
                            TrustGrantItemView(grant: grant, vm: vm)
                                .automationScope(Ids.nestTrustGrantItem, index: index)
                        }
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.nestTrustGrantList)
                    .automationValue(Ids.nestTrustGrantList, text: { String(row.trustGrants.count) })
                }
                // Backup trust rows (nests.md § Trust facet — backup rows) — AFTER
                // the content-processing grant rows, home row only (the shared
                // machine populates them nowhere else, since both grants empower
                // the source nest). Rendered alongside EITHER arm above: next to
                // the grant list when content grants exist, and on their own when
                // none do (the empty state is suppressed in that case).
                ForEach(Array(row.trustBackups.enumerated()), id: \.offset) { _, backup in
                    TrustBackupItemView(backup: backup, vm: vm)
                }
                // Retained generations (nests.md § Trust facet — generation
                // recovery, ratified 2026-07-29) — AFTER the backup trust
                // rows, home row only (same both-arms placement as the backup
                // rows: the shared machine populates them nowhere else).
                ForEach(Array(row.trustGenerations.enumerated()), id: \.offset) { _, generation in
                    GenerationItemView(generation: generation, vm: vm)
                }
                // The restore-outcome notice (`nest-trust-generation-notice`) —
                // page-scoped, NOT per-row: a restore's outcome describes the
                // page's last action, not any one generation row. Registered
                // whenever the home row's Now lens renders, EMPTY until a
                // restore resolves — the leaf set does not vary. Distinct
                // from `error-message`: only a genuinely failed call reaches
                // that; `pastRecoveryWindow` is a product state, never an error.
                if row.isHome {
                    automationText(Ids.nestTrustGenerationNotice, generationNoticeText(vm.restoreOutcome))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                // The per-nest blessing (`nest-trust-blessed-toggle`), home row
                // only, like the rest of the facet; `state` mirrors the switch
                // for a driver. `SetBlessed` is a `fauna.state.blessed-nests` write (offline-safe).
                if row.isHome {
                    BlessedToggleView(row: row, vm: vm)
                }
                // Mint flow (scope-first picker, nests.md § Mint, ratified
                // 2026-07-13) — rendered after the grant list / empty state, and
                // only when the shared option catalog is non-empty (an empty
                // catalog means nothing derivable or no discoverable holder —
                // never a picker that can only error).
                if !row.mintOptions.isEmpty {
                    MintFlowView(row: row, vm: vm)
                }
            case .history:
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(row.trustHistory.enumerated()), id: \.offset) { _, h in
                        automationText(Ids.nestTrustHistoryItem, historyLine(h))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.nestTrustHistoryList)
                .automationValue(Ids.nestTrustHistoryList, text: { String(row.trustHistory.count) })
            }
        }
        .padding(.top, 4)
    }
}

/// One current-grant row (`nest-trust-grant-item`) for the Now lens: the scope
/// line ("Trusted to read: Mail, Calendar"), lasts-until, liveness status, the
/// REQUIRED honest-bound copy, and per-grant renew/revoke. `grantId` round-trips
/// unchanged into the Renew/Revoke dispatch.
private struct TrustGrantItemView: View {
    let grant: TrustGrantRow
    let vm: LinkedNestsVM

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            automationText(
                Ids.nestTrustGrantScope,
                "\(L.nests.trustedToRead) \(scopeLine(grant.scope))"
            )
            .font(.subheadline)
            .fontWeight(.medium)
            automationText(
                Ids.nestTrustGrantLastsUntil,
                "\(L.nests.lastsUntil) \(ValueFormat.absoluteDate(epochMs: grant.lastsUntil * 1000, withTime: true))"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            automationText(Ids.nestTrustGrantStatus, statusLabel(grant.liveness))
                .font(.caption)
                .foregroundStyle(.secondary)
            // REQUIRED honest-bound copy (nests.md § Honest bound) — never
            // over-promise past the honest bound. A bounded (content-sealing-epochs)
            // mail grant gets the stronger, crypto-bounded wording WITH the honest
            // INFO-A caveat; every other kind/regime keeps the standing
            // trust-until-revoke wording (flip-checklist line 6). Never re-derive
            // the (class, kind, tier) check here — the shared predicate is the
            // single source of truth (priority #2).
            automationText(
                Ids.nestTrustGrantBoundNote,
                trustScopeIsBoundedMailGrant(scope: grant.scope)
                    ? L.nests.boundNoteBoundedMail
                    : L.nests.boundNoteStanding
            )
                .font(.caption2)
                .foregroundStyle(.secondary)
            // The post-succession review mark (`succession-aftermath.md`
            // § Re-key scope → *Adjudicating what the aftermath carries
            // across*; web is the prior art). `grant.unattested` is resolved in
            // shared Rust (`project_grant`) from the config's mark plane.
            // ABSENT, not empty, on an ordinary grant, and Remove is NOT
            // re-rendered: `nest-trust-grant-revoke` below already is it.
            if grant.unattested {
                automationText(Ids.nestTrustGrantUnattestedMark, L.nests.grantUnattestedMark)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            HStack(spacing: 8) {
                if grant.unattested {
                    // No confirm: Keep is non-destructive and re-decidable.
                    Button(L.nests.grantKeepButton) {
                        Task { await vm.dispatch(.keepGrant(grantId: grant.grantId)) }
                    }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.nestTrustGrantKeepButton)
                    .automationActivate(Ids.nestTrustGrantKeepButton) {
                        Task { await vm.dispatch(.keepGrant(grantId: grant.grantId)) }
                    }
                }
                Button(L.nests.renew) {
                    Task { await vm.dispatch(.renew(grantId: grant.grantId)) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestTrustGrantRenew)
                .automationActivate(Ids.nestTrustGrantRenew) {
                    Task { await vm.dispatch(.renew(grantId: grant.grantId)) }
                }
                .faunaGate("fauna.capabilities.renew")
                Button(L.nests.revoke, role: .destructive) {
                    Task { await vm.dispatch(.revoke(grantId: grant.grantId)) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestTrustGrantRevoke)
                .automationActivate(Ids.nestTrustGrantRevoke) {
                    Task { await vm.dispatch(.revoke(grantId: grant.grantId)) }
                }
                .faunaGate("fauna.capabilities.revoke")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nestTrustGrantItem)
        // Per-row presence entry so the in-process registry can `count`
        // `nest-trust-grant-item` rows (mirrors the `nests-item` container
        // pattern — a bare `.accessibilityIdentifier` alone never registers).
        .automationValue(Ids.nestTrustGrantItem, text: { scopeLine(grant.scope) })
    }
}

/// One backup trust row (`nest-trust-backup-item`) for the Now lens on the home
/// nest's row: the scope line, when the trust was given, the row state, the
/// REQUIRED honest-bound copy, and the freeze-the-backup affordance (`nests.md`
/// § Trust facet — backup rows).
///
/// Two row kinds share this view (`nests.md:63`): the seal grant, revoked at
/// the source nest, and one writer row per destination, revoked **at the
/// destination** — the shared machine routes each `nest-trust-backup-revoke`
/// press to the right nest, so this layer only names which row was pressed.
///
/// Deliberately no lasts-until / renew / History twin: both backup grants are
/// standing live nest reads, not folds of the signed grant-event log
/// (`nests.md:99`).
private struct TrustBackupItemView: View {
    let backup: TrustBackupRow
    let vm: LinkedNestsVM

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            automationText(Ids.nestTrustBackupScope, scopeText)
                .font(.subheadline)
                .fontWeight(.medium)
            // `nest-trust-backup-since` renders EMPTY on the seal row — that
            // grant carries no timestamp on the wire (`nests.md:67`). The
            // element still exists so the row's leaf set doesn't vary by kind.
            automationText(Ids.nestTrustBackupSince, sinceText)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.nestTrustBackupStatus, statusText)
                .font(.caption)
                .foregroundStyle(.secondary)
            // REQUIRED honest-bound copy — revoking freezes only NEW writes;
            // custody already held remains until the holder reclaims it. Never
            // over-promise.
            automationText(Ids.nestTrustBackupBoundNote, boundNoteText)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Button(L.nests.backupRevoke, role: .destructive) {
                Task { await vm.dispatch(revokeAction) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.nestTrustBackupRevoke)
            .automationActivate(Ids.nestTrustBackupRevoke) {
                Task { await vm.dispatch(revokeAction) }
            }
            // The row decides which grant it revokes, and the view can observe
            // the discriminant — so the declaration rides the same `backup.kind`
            // switch `revokeAction` takes (the cert-issue button's
            // observable-discriminant shape). Declaring only the seal grant left
            // `fauna.backup.writer_grant.revoke` covered nowhere on apple.
            .faunaGate(backup.kind == .seal
                       ? "fauna.backup.nest_key.revoke"
                       : "fauna.backup.writer_grant.revoke")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nestTrustBackupItem)
        // Per-row presence entry so the in-process registry can `count`
        // `nest-trust-backup-item` rows (mirrors the `nest-trust-grant-item`
        // pattern — a bare `.accessibilityIdentifier` alone never registers).
        .automationValue(Ids.nestTrustBackupItem, text: { scopeText })
    }

    private var scopeText: String {
        switch backup.kind {
        case .seal: L.nests.backupScopeSeal
        case .writer: L.nests.backupScopeWriter(destination: backup.destinationLabel)
        }
    }

    private var sinceText: String {
        guard let since = backup.since else { return "" }
        return "\(L.nests.backupSince) \(ValueFormat.absoluteDate(epochMs: since * 1000, withTime: true))"
    }

    private var statusText: String {
        backupStatusLabel(backup.status)
    }

    private var boundNoteText: String {
        switch backup.kind {
        case .seal: L.nests.backupBoundNoteSeal
        case .writer: L.nests.backupBoundNoteWriter
        }
    }

    private var revokeAction: LinkedNestsAction {
        switch backup.kind {
        case .seal: .revokeBackupSeal
        case .writer: .revokeBackupWriter(destinationId: backup.destinationId)
        }
    }
}

/// One retained-generation row (`nest-trust-generation-item`) in the Now lens
/// on the home nest's row: what the owner can roll back to inside the custody
/// grace window `T` (`nests.md` § Trust facet — generation recovery). Same
/// three ratified honesty requirements as linux's `build_generation_item`:
///
/// - An `.unreachable` row renders **no** `nest-trust-generation-restore`
///   (`nests.md:122`) — there is no address to restore, and offering the
///   affordance would imply we knew something we do not.
/// - A row with no plaintext `path` renders its **hash** rather than being
///   hidden or skipped (`nests.md:123`) — the rows a rogue source produced
///   are exactly the ones a user needs to see.
/// - The three value leaves (superseded/expires/size) render EMPTY on an
///   unreachable row rather than a zero timestamp or "0 B", which would read
///   as fact — the leaves stay registered so the row's leaf set does not vary
///   by status.
///
/// The address triple (`folderName`/`pathHash`/`manifestHash`) round-trips
/// off the row unchanged into `RestoreGeneration` — never a row index, which
/// would promote the wrong generation the moment this flattened list is
/// filtered or re-ordered.
private struct GenerationItemView: View {
    let generation: TrustGenerationRow
    let vm: LinkedNestsVM

    private var unreachable: Bool { generation.status == .unreachable }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            automationText(Ids.nestTrustGenerationPath, pathText)
                .font(.subheadline)
                .fontWeight(.medium)
            automationText(Ids.nestTrustGenerationSuperseded, supersededText)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.nestTrustGenerationExpires, expiresText)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.nestTrustGenerationSize, sizeText)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.nestTrustGenerationStatus, statusText)
                .font(.caption)
                .foregroundStyle(.secondary)
            if !unreachable {
                Button(L.nests.generationRestore) {
                    Task { await vm.dispatch(restoreAction) }
                }
                .controlSize(.small)
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.nestTrustGenerationRestore)
                .automationActivate(Ids.nestTrustGenerationRestore) {
                    Task { await vm.dispatch(restoreAction) }
                }
                .faunaGate("fauna.backup.generation.restore")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nestTrustGenerationItem)
        // Per-row presence entry so the in-process registry can `count`
        // `nest-trust-generation-item` rows (mirrors `nest-trust-backup-item`).
        .automationValue(Ids.nestTrustGenerationItem, text: { pathText })
    }

    /// On an `.unreachable` row this names the DESTINATION that went dark,
    /// since there is no generation to identify; on a `.listed` row with no
    /// plaintext `path` (a sealed custody row with its path scrubbed),
    /// this renders the `pathHash` rather than hiding or skipping the row.
    private var pathText: String {
        if unreachable { return generation.destinationLabel }
        if let path = generation.path { return L.nests.generationPath(path: path) }
        return L.nests.generationPathUnknown(hash: generation.pathHash)
    }

    private var supersededText: String {
        guard !unreachable else { return "" }
        let when = ValueFormat.absoluteDate(epochMs: generation.supersededAt * 1000, withTime: true)
        return "\(L.nests.generationSuperseded) \(when)"
    }

    private var expiresText: String {
        guard !unreachable else { return "" }
        let when = ValueFormat.absoluteDate(epochMs: generation.expiresAt * 1000, withTime: true)
        return L.nests.generationExpires(when: when)
    }

    private var sizeText: String {
        guard !unreachable else { return "" }
        return ValueFormat.byteSize(UInt64(max(generation.sizeBytes, 0)))
    }

    private var statusText: String {
        unreachable ? L.nests.generationStatusUnreachable : L.nests.generationStatusListed
    }

    private var restoreAction: LinkedNestsAction {
        .restoreGeneration(
            destinationId: generation.destinationId,
            folderName: generation.folderName,
            pathHash: generation.pathHash,
            manifestHash: generation.manifestHash)
    }
}

/// The mint flow for one nest row (`nest-trust-grant-mint-button` →
/// `nest-trust-mint-scope-select` [→ `nest-trust-mint-holder-select`] →
/// `nest-trust-mint-confirm-button`; `nests.md` § Mint, scope-first design
/// ratified 2026-07-13). The scope select's options are the shared
/// `LinkedNestRow.mintOptions` catalog verbatim (one use-case option each,
/// labeled shell-side — priority #2); the holder is derived from the chosen
/// option, and the holder select renders only when an option lists more than
/// one candidate (the ambiguity case; every option derives exactly one today).
/// Confirm dispatches `Mint{nestId, holderBridgeId, scope}`; the reveal + picks
/// reset on a successful mint (the machine re-render collapses the form).
struct MintFlowView: View {
    let row: LinkedNestRow
    let vm: LinkedNestsVM

    @State private var showForm = false
    /// "" = no option chosen (the placeholder); else the option's localized
    /// label — unique per row (Mail/Calendar are singletons, PaywalledPosts is
    /// one per held tier), so it doubles as a stable selection key.
    @State private var selectedOptionLabel = ""
    @State private var selectedHolderIndex = 0
    /// "" = the row's default (`mintDefaultDuration`: standard on a blessed
    /// nest, one-off otherwise) until the user picks; else the option's label.
    @State private var selectedDurationLabel = ""

    private var durationOptions: [TrustGrantDuration] { mintDurationOptions() }

    private var selectedDuration: TrustGrantDuration {
        Self.duration(picked: selectedDurationLabel, default: row.mintDefaultDuration)
    }

    /// The duration a mint sends: the option the user picked (by its rendered
    /// label), else the row's default until they pick.
    static func duration(picked label: String, default fallback: TrustGrantDuration) -> TrustGrantDuration {
        mintDurationOptions().first { durationText($0) == label } ?? fallback
    }

    private var selectedOption: TrustMintOption? {
        row.mintOptions.first { mintOptionLabel($0) == selectedOptionLabel }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(L.nests.mintButton) { showForm = true }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.nestTrustGrantMintButton)
                .automationActivate(Ids.nestTrustGrantMintButton) { showForm = true }

            if showForm {
                Picker(L.nests.mintScopePlaceholder, selection: $selectedOptionLabel) {
                    Text(L.nests.mintScopePlaceholder).tag("")
                    ForEach(row.mintOptions, id: \.self) { option in
                        Text(mintOptionLabel(option)).tag(mintOptionLabel(option))
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .accessibilityIdentifier(Ids.nestTrustMintScopeSelect)
                .automationSelect(
                    Ids.nestTrustMintScopeSelect, value: { selectedOptionLabel }
                ) { newValue in
                    selectedOptionLabel = newValue
                    selectedHolderIndex = 0
                }

                if let option = selectedOption, option.holderCandidates.count > 1 {
                    Picker(L.nests.mintHolderPlaceholder, selection: $selectedHolderIndex) {
                        ForEach(Array(option.holderCandidates.enumerated()), id: \.offset) { idx, candidate in
                            Text(candidate).tag(idx)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .accessibilityIdentifier(Ids.nestTrustMintHolderSelect)
                    .automationSelect(
                        Ids.nestTrustMintHolderSelect, value: { String(selectedHolderIndex) }
                    ) { newValue in
                        if let idx = Int(newValue) { selectedHolderIndex = idx }
                    }
                }

                // The mint duration (`nests.md` § Expiry / renewal → *Duration and
                // blessing*): the shared option list, pre-selecting the row's default.
                Picker(durationText(selectedDuration), selection: Binding(
                    get: { durationText(selectedDuration) },
                    set: { selectedDurationLabel = $0 }
                )) {
                    ForEach(durationOptions, id: \.self) { d in
                        Text(durationText(d)).tag(durationText(d))
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .accessibilityIdentifier(Ids.nestTrustMintDurationSelect)
                .automationSelect(
                    Ids.nestTrustMintDurationSelect,
                    value: { durationText(selectedDuration) },
                    options: { durationOptions.map(durationText) }
                ) { newValue in
                    selectedDurationLabel = newValue
                }

                Button(L.nests.mintConfirm) { confirmMint() }
                    .controlSize(.small)
                    .disabled(selectedOption == nil)
                    .accessibilityIdentifier(Ids.nestTrustMintConfirmButton)
                    .automationActivate(
                        Ids.nestTrustMintConfirmButton, isEnabled: { selectedOption != nil }
                    ) { confirmMint() }
                    // Arming is local: `nest-trust-grant-mint-button` reveals
                    // this form and the two scope/holder pickers are buffer —
                    // only the confirm deposits the sealed grant.
                    .faunaGate("fauna.capabilities.mint")
            }
        }
        .padding(.top, 2)
    }

    /// Derived holder: the single candidate; ambiguity → the holder select's
    /// pick (visible iff >1 candidate). Resets the form on dispatch — the
    /// machine re-render supplies the fresh grant list.
    private func confirmMint() {
        guard let option = selectedOption else { return }
        let holderBridgeId = option.holderCandidates.count > 1
            ? option.holderCandidates[selectedHolderIndex]
            : option.holderCandidates[0]
        let duration = selectedDuration
        Task {
            await vm.dispatch(.mint(nestId: row.nestId, holderBridgeId: holderBridgeId, scope: option.scope, duration: duration))
        }
        showForm = false
        selectedDurationLabel = ""
        selectedOptionLabel = ""
        selectedHolderIndex = 0
    }
}

/// One custodian-**nest** `nests-item` (`nests.md` § Trust facet — custody
/// rows): a nest that holds sealed copies of this account's planes. The row is
/// the label plus one `nest-trust-custody-item` container carrying the family —
/// scope, receipt status, held bytes, the honest-bound note and the revoke
/// button — exactly as linux's `build_custody_nest_item`. The Devices page's
/// `custody-holder-card` family renders the complement (device-bound rows).
struct CustodyNestItemRow: View {
    let row: CustodyHolderRowView
    /// Position in the rendered list — the `nests-item[i]` scope step.
    let index: Int
    let vm: LinkedNestsVM

    /// A pending ceremony has minted nothing to revoke: the control is disabled
    /// and the honest-bound note (which would over-promise there) is absent.
    static func isRevocable(_ row: CustodyHolderRowView) -> Bool { !row.pending }

    /// The row's `nests-item-label`: the shared short id of the host's hex.
    static func labelText(for row: CustodyHolderRowView) -> String {
        L.nests.custodyNestLabel(host: shortId(hex: row.host.hexString))
    }

    var body: some View {
        let labelText = Self.labelText(for: row)
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.nestsItemLabel, labelText)
                .font(.headline)
            VStack(alignment: .leading, spacing: 4) {
                automationText(Ids.nestTrustCustodyScope, L.devices.custodyHolderScope)
                    .font(.subheadline)
                    .fontWeight(.medium)
                // Three states, three strings — never collapsed, never empty.
                automationText(Ids.nestTrustCustodyReceiptStatus, ValueFormat.custodyReceiptStatusText(row.receipt))
                    .font(.caption)
                    .foregroundStyle(row.receiptState == .stale ? .red : .secondary)
                automationText(Ids.nestTrustCustodyHeldBytes, ValueFormat.custodyHeldBytesText(row.receipt))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // The honest bound beside the control it bounds (REQUIRED). A
                // pending ceremony has minted nothing to revoke, so the note
                // would over-promise there and the control is disabled instead.
                if Self.isRevocable(row) {
                    Text(L.devices.custodyRevokeBoundNote)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Button(L.devices.custodyRevoke, role: .destructive) { revoke() }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(!Self.isRevocable(row))
                    .accessibilityIdentifier(Ids.nestTrustCustodyRevokeButton)
                    .automationActivate(Ids.nestTrustCustodyRevokeButton, isEnabled: { Self.isRevocable(row) }) { revoke() }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.nestTrustCustodyItem)
            .automationValue(Ids.nestTrustCustodyItem, text: { row.host.hexString })
        }
        .padding(10)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nestsItem)
        .automationValue(Ids.nestsItem, text: { labelText })
        .automationScope(Ids.nestsItem, index: index)
    }

    private func revoke() {
        Task { await vm.revokeCustody(grantId: row.grantId, holder: row.custodianKey) }
    }
}

/// The per-nest blessing switch (`nest-trust-blessed-toggle`): while on, the
/// nest's standing grants renew themselves. `state` mirrors the switch for a
/// driver (the toggle convention). Toggling dispatches `SetBlessed`.
private struct BlessedToggleView: View {
    let row: LinkedNestRow
    let vm: LinkedNestsVM

    var body: some View {
        Toggle(L.nests.blessedToggle, isOn: Binding(
            get: { row.blessed },
            set: { set($0) }
        ))
        .controlSize(.small)
        .accessibilityIdentifier(Ids.nestTrustBlessedToggle)
        .automationActivate(
            Ids.nestTrustBlessedToggle,
            value: { row.blessed ? "on" : "off" },
            attributes: { ["state": row.blessed ? "on" : "off"] }
        ) { set(!row.blessed) }
    }

    private func set(_ blessed: Bool) {
        Task { await vm.dispatch(.setBlessed(nestId: row.nestId, blessed: blessed)) }
    }
}

// MARK: - Label mapping (thin wrappers over the shared `fauna_client_pair`
// label functions, resolved through the apple i18n pipeline — mirrors linux's
// own `mint_option_label`/`scope_label`/`status_label` wrappers, which each
// do the identical `fauna_client_pair::foo_label(...).resolve(lookup)`
// one-liner; android's `LinkedNestsScreen.kt` calls the same shared doors
// directly. These three used to hand-roll the match arms locally (pre-dating
// the lift of that cluster into `fauna_client_pair::trust`) —
// switched to the shared door so a future scope/use-case/liveness variant
// only needs updating once.

private func mintOptionLabel(_ option: TrustMintOption) -> String {
    renderLocalizedText(FaunaFFISwift.mintOptionLabel(o: option))
}

func durationText(_ duration: TrustGrantDuration) -> String {
    renderLocalizedText(FaunaFFISwift.durationLabel(d: duration))
}

private func scopeLabel(_ scope: TrustScope) -> String {
    renderLocalizedText(FaunaFFISwift.scopeLabel(s: scope))
}

private func scopeLine(_ scope: [TrustScope]) -> String {
    scope.map(scopeLabel).joined(separator: ", ")
}

private func statusLabel(_ liveness: TrustLiveness) -> String {
    renderLocalizedText(FaunaFFISwift.statusLabel(l: liveness))
}

private func backupStatusLabel(_ status: TrustBackupStatus) -> String {
    renderLocalizedText(FaunaFFISwift.backupStatusLabel(status: status))
}

/// One History-lens row's self-describing line ("Trusted to read ‹scope› ·
/// ‹when›" etc.). Trust timestamps (`at`) are epoch **seconds** (the shared
/// row's wire unit); `ValueFormat.absoluteDate` takes epoch milliseconds.
private func historyLine(_ h: TrustHistoryRow) -> String {
    let scope = scopeLine(h.scope)
    let when = ValueFormat.absoluteDate(epochMs: h.at * 1000, withTime: true)
    switch h.kind {
    case .mint: return L.nests.historyMinted(scope: scope, when: when)
    case .renew: return L.nests.historyRenewed(scope: scope, when: when)
    case .revoke: return L.nests.historyRevoked(scope: scope, when: when)
    }
}

/// The `nest-trust-generation-notice` text for the page's last restore action
/// (`nests.md` § Trust facet — generation recovery). Never an error —
/// `pastRecoveryWindow` is a product state, not a failure — so this never
/// rides `error-message`.
private func generationNoticeText(_ outcome: TrustRestoreOutcome?) -> String {
    switch outcome {
    case .restored: return L.nests.generationRestored
    case .pastRecoveryWindow: return L.nests.generationPastWindow
    case nil: return ""
    }
}
