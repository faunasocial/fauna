import SwiftUI

/// The user-facing **mail-aliases** page (`docs/goal/behavior/mail-aliases.md`),
/// shared by macOS + iOS (one FaunaKit view, thin per-target call sites —
/// A dumb renderer of `MailAliasesSnapshot` + dispatcher
/// of `MailAliasesAction` over the shared `MailAliasesMachine` (via
/// `MailAliasesVM`); no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `mail-aliases` / `mail-aliases-list` exactly.
/// Reference implementation: linux `apps/fauna-linux/src/settings/mail_aliases.rs`.
///
/// On macOS this is the `mail-aliases` sub-page of the Settings sidebar-swap shell
/// (`SettingsShellView`); on iOS it is a Settings sub-page (NavigationLink). The add sheet's
/// kind picker steps Exact → Wildcard → Disposable (Disposable takes a lifetime and
/// use count); the page's generate button mints one with the defaults.
public struct MailAliasesView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailAliasesVM()

    /// The bulk paste-import sheet (separate presentation from the add/edit one —
    /// mirrors `MailListMembersView`'s add + import pair).
    @State private var showingImportSheet = false
    /// Which add/edit sheet is open, and with what data. `.sheet(item:)`, not
    /// `.sheet(isPresented:)` + a separate `editing` var: the two-state-var
    /// form races on first presentation — confirmed live on the identical
    /// `MailListsView` pattern (a debug log showed the sheet's own `onAppear`
    /// still reading `editing == nil` right after `beginEdit` set it) — a real
    /// SwiftUI content-closure/state-propagation race, not a hydrate-timing
    /// issue. `.sheet(item:)` guarantees the content closure receives the
    /// exact value that triggered presentation.
    private enum AliasSheetMode: Identifiable {
        case add
        case edit(AliasView)

        var id: String {
            switch self {
            case .add: return "add"
            case .edit(let alias): return alias.aliasIdHex
            }
        }

        var editing: AliasView? {
            if case .edit(let alias) = self { return alias }
            return nil
        }
    }

    @State private var sheetMode: AliasSheetMode?
    /// The alias whose destructive Delete is currently armed (two-click inline
    /// confirm, no modal — `nil` = nothing armed). Mirrors linux `wire_two_click`.
    @State private var armedDeleteId: String?
    /// The exact address last written to the clipboard by a mint, `nil` before
    /// any mint this page-visit — the generate button's `copied` attr (the
    /// `CopyButton`/`profile-actor-id-copy-btn` contract: the exact string it
    /// put on the clipboard). Set from `vm.snapshot?.lastMintedAddress` the
    /// moment it changes, since the mint's address isn't known at click time.
    @State private var copiedMintedAddress: String?

    public init() {}

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): an iOS `Form` lazily
        // realizes AND POOLS its rows, so an alias row deleted from
        // `vm.snapshot?.aliases` (`mail-aliases-list-item-overflow-menu` two-click
        // delete, `test_mail_aliases_revoke_then_delete`) can linger past its real
        // removal (`wait_for_pattern_gone` never resolves) — the same delete-zombie
        // class rule 6 fixed for iOS Events. Cost: rows lose the grouped `Form`
        // styling (accepted rule-6 production-UI tradeoff). The add/import SHEETS
        // keep their `Form` — a modal presentation isn't the pooled scroll list.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                headerSection
                listSection
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.mailAliases.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
        .sheet(item: $sheetMode) { mode in
            MailAliasAddSheet(vm: vm, editing: mode.editing)
        }
        .sheet(isPresented: $showingImportSheet) {
            MailAliasImportSheet(vm: vm)
        }
        .onChange(of: vm.snapshot?.lastMintedAddress) {
            if let minted = vm.snapshot?.lastMintedAddress {
                Pasteboard.copy(minted)
                copiedMintedAddress = minted
            }
        }
    }

    // MARK: - Header (description + add / generate buttons)

    private var headerSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.mailAliases.description)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button(L.mailAliases.addButton) {
                beginAdd()
            }
            .accessibilityIdentifier(Ids.mailAliasesAddButton)
            .automationActivate(
                Ids.mailAliasesAddButton,
                isEnabled: { vm.defaultDomain != nil }
            ) { beginAdd() }
            .disabled(vm.defaultDomain == nil)
            // Bulk paste-import (the recipient-whitelist import path — a user
            // migrating a ~100-address allowlist pastes it in one shot).
            // Domain-gated exactly like add/generate (linux `mail_aliases.rs:695`).
            Button(L.mailAliases.importButton) {
                showingImportSheet = true
            }
            .accessibilityIdentifier(Ids.mailAliasesImportButton)
            .automationActivate(
                Ids.mailAliasesImportButton,
                isEnabled: { vm.defaultDomain != nil }
            ) { showingImportSheet = true }
            .disabled(vm.defaultDomain == nil)
            Button(L.mailAliases.generateButton) {
                generateDisposable()
            }
            .accessibilityIdentifier(Ids.mailAliasesGenerateDisposableButton)
            .automationActivate(
                Ids.mailAliasesGenerateDisposableButton,
                isEnabled: { vm.defaultDomain != nil },
                attributes: { ["copied": copiedMintedAddress ?? ""] }
            ) { generateDisposable() }
            .disabled(vm.defaultDomain == nil)
            // The page's one header control that COMMITS: add and import above
            // only open a sheet (arming is local), while Generate mints the
            // alias on the spot. `MailAliasesAction::GenerateDisposable` →
            // `MailAliasesNest::generate_disposable_alias`.
            .faunaGate("fauna.bridges.generate_disposable_alias")
            if vm.defaultDomain == nil {
                Text(L.mailAliases.noDefaultDomain)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The full handle-bearing disposable address is shown once at mint so
            // the user can copy it (the list row only carries the `-temp-` tag).
            if let minted = vm.snapshot?.lastMintedAddress {
                Text("\(L.mailAliases.copied): \(minted)")
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Aliases list

    private var listSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            let aliases = vm.snapshot?.aliases ?? []
            // Loading-is-not-empty (`ui/README.md` § List pages): an empty
            // `aliases` pre-hydrate must not read as "no aliases yet".
            if !vm.loaded {
                Text(L.mailAliases.loading).foregroundStyle(.secondary)
            } else if aliases.isEmpty {
                Text(L.mailAliases.empty).foregroundStyle(.secondary)
            } else {
                ForEach(Array(aliases.enumerated()), id: \.element.aliasIdHex) { index, alias in
                    aliasRow(alias, index: index)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier(Ids.mailAliasesList)
    }

    @ViewBuilder
    private func aliasRow(_ alias: AliasView, index: Int) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            // Per-row pattern label — `row_count()`/`patterns()` count + read
            // this id (one registry entry per row, flat-registry indexed).
            automationText(Ids.mailAliasesListItemPattern, alias.address)
                .font(.headline)
                .textSelection(.enabled)
            automationText(Ids.mailAliasesListItemKind, renderLocalizedText(aliasKindBadge(kind: alias.kind)))
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.mailAliasesListItemLabel, alias.label)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.mailAliasesListItemHits, hitsText(alias))
                .font(.caption)
                .foregroundStyle(.secondary)
            if alias.isCanonical {
                // The canonical `<handle>@<domain>` row is the user's primary
                // mailbox + AUTH-login identity; the nest rejects disabling,
                // renaming, or deleting it (`canonical_alias_protected`). Render
                // it read-only — no toggle/edit/revoke/delete, marked as the
                // primary address — so the protection is visible rather than
                // surfacing only as an error on attempt (`mail-aliases.md`
                // § Aliases UX). The per-row control IDs simply don't appear on
                // this row; the badge carries no test id (matches linux). The
                // inert audit disclosure is kept for parity.
                HStack {
                    Text(L.mailAliases.primaryAddressBadge)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .help(L.mailAliases.primaryAddressTooltip)
                    Button(L.mailAliases.showAudit) {}
                        .disabled(true)
                        .accessibilityIdentifier(Ids.mailAliasesListItemShowAudit)
                }
                .controlSize(.small)
            } else {
                // Two-way "Active" toggle (mail-aliases.md § Disable): ON = the
                // address receives mail; OFF dispatches Revoke (soft-off), ON
                // dispatches Enable (re-enable). The shared MailAliasesMachine's
                // Enable action flips disabled=false, so disabling is no longer a
                // one-way trap. Mirrors linux mail_aliases.rs. A snapshot refresh
                // updates the binding's value, not the user-interaction setter, so
                // it never re-fires.
                Toggle(L.mailAliases.activeToggleLabel, isOn: Binding(
                    get: { !alias.disabled },
                    set: { on in setAliasActive(alias, active: on) }
                ))
                .help(L.mailAliases.activeToggleTooltip)
                .accessibilityIdentifier(Ids.mailAliasesListItemDisabledToggle)
                // The two-way "Active" switch reads ON when the alias receives
                // mail (not disabled). Activating flips it (linux idiom: one
                // click toggles enable↔revoke).
                .automationActivate(
                    Ids.mailAliasesListItemDisabledToggle,
                    value: { alias.disabled ? "off" : "on" }
                ) { setAliasActive(alias, active: alias.disabled) }
                // The two directions ride DISTINCT kinds, so the declaration
                // takes the same discriminant `setAliasActive` itself takes
                // (`admin-dns-cert-issue-button`'s precedent) rather than
                // answering one of two. tui leaves its twin undeclared because
                // its action carries no mode; the SwiftUI row has the row's own
                // state in hand, so the exact kind is knowable here. Both arms
                // are OnlineOnly, so the toggle greys either way — but the
                // *contract* is the exact kind, and a later reclassification of
                // one arm now reaches this control for free.
                .faunaGate(alias.disabled
                           ? "fauna.bridges.enable_account_alias"
                           : "fauna.bridges.revoke_account_alias")
                HStack {
                    Button(L.mailAliases.edit) {
                        beginEdit(alias)
                    }
                    .accessibilityIdentifier(Ids.mailAliasesListItemEditButton)
                    .automationActivate(Ids.mailAliasesListItemEditButton) {
                        beginEdit(alias)
                    }
                    Button(L.mailAliases.revoke, role: .destructive) {
                        revokeAlias(alias)
                    }
                    .accessibilityIdentifier(Ids.mailAliasesListItemRevokeButton)
                    .automationActivate(Ids.mailAliasesListItemRevokeButton) {
                        revokeAlias(alias)
                    }
                    .faunaGate("fauna.bridges.revoke_account_alias")
                    // Destructive delete: two-click inline arm-then-confirm (no
                    // modal, no dropdown — the e2e clicks this same id twice).
                    // A SwiftUI Menu can't satisfy that contract (its inner item
                    // carries no id and the second tap just closes the popup);
                    // mirror linux `wire_two_click` instead. First tap arms +
                    // relabels "Confirm?"; second tap dispatches Delete.
                    Button(deleteLabel(alias), role: .destructive) {
                        tapDelete(alias)
                    }
                    .accessibilityIdentifier(Ids.mailAliasesListItemOverflowMenu)
                    // `text:` reports the armed/unarmed label — without it
                    // `/element/text` reads `""` whatever the button renders
                    // (`apple-e2e-automation.md` rule 11), so the confirm's
                    // relabel, its only visible affordance, was unreadable.
                    .automationActivate(
                        Ids.mailAliasesListItemOverflowMenu,
                        text: { deleteLabel(alias) }
                    ) {
                        tapDelete(alias)
                    }
                    // "Arming is local" does NOT split this one: arm and confirm
                    // are the same control (the two-click inline pattern), so the
                    // control that issues Delete is this one, and arming a
                    // confirm that cannot fire is worse than not arming it.
                    .faunaGate("fauna.bridges.delete_account_alias")
                    // Inert disclosure — per-alias audit listing is not yet exposed
                    // by the nest (linux renders the same disabled affordance).
                    Button(L.mailAliases.showAudit) {}
                        .disabled(true)
                        .accessibilityIdentifier(Ids.mailAliasesListItemShowAudit)
                }
                .controlSize(.small)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailAliasesListItem)
        // OUTERMOST, so every per-row control registers under
        // `(mail-aliases-list-item, index)` — the row index `patterns()` reads.
        // Without it a scoped query fell back to the control's flat index, which
        // compresses behind the canonical row (it paints no revoke/edit/delete),
        // so `mail-aliases-list-item[1]/…-revoke-button` found nothing. Same
        // idiom as `account-switcher-item`.
        .automationScope(Ids.mailAliasesListItem, index: index)
    }

    // MARK: - Helpers (shared by Buttons and their `.automationActivate`)

    /// Open the add-sheet (new alias). Shared by the header add `Button` and its
    /// `.automationActivate` so the two never diverge.
    private func beginAdd() {
        sheetMode = .add
    }

    /// Mint a disposable alias. Shared by the generate `Button` and its
    /// `.automationActivate`.
    private func generateDisposable() {
        Task { await vm.dispatch(.generateDisposable(ttlDays: nil, uses: nil, label: "")) }
    }

    /// Open the add-sheet pre-populated for `alias` (edit). Shared by the row's
    /// edit `Button` and its `.automationActivate`.
    private func beginEdit(_ alias: AliasView) {
        sheetMode = .edit(alias)
    }

    /// Flip the row's two-way "Active" switch. Shared by the `Toggle`'s setter and
    /// its `.automationActivate`: active ⇒ Enable, inactive ⇒ Revoke (soft-off).
    private func setAliasActive(_ alias: AliasView, active: Bool) {
        Task {
            await vm.dispatch(active
                ? .enable(aliasIdHex: alias.aliasIdHex)
                : .revoke(aliasIdHex: alias.aliasIdHex))
        }
    }

    /// Soft-revoke `alias`. Shared by the row's revoke `Button` and its
    /// `.automationActivate`.
    private func revokeAlias(_ alias: AliasView) {
        Task { await vm.dispatch(.revoke(aliasIdHex: alias.aliasIdHex)) }
    }

    /// The row's delete-button label: `Delete`, relabelled `Confirm?` once armed.
    /// Shared by the `Button` title and its `.automationActivate` `text:` so what
    /// the driver reads is what is rendered. Reads `armedDeleteId` live.
    private func deleteLabel(_ alias: AliasView) -> String {
        armedDeleteId == alias.aliasIdHex ? L.common.confirmQ : L.mailAliases.delete
    }

    /// Two-click inline delete arm-then-confirm (no modal). Shared by the row's
    /// delete `Button` and its `.automationActivate` so the e2e's two taps on the
    /// same id arm then dispatch Delete (mirrors linux `wire_two_click`). Reads
    /// `armedDeleteId` live.
    private func tapDelete(_ alias: AliasView) {
        tapArmedDelete(alias.aliasIdHex, armed: $armedDeleteId) {
            await vm.dispatch(.delete(aliasIdHex: alias.aliasIdHex))
        }
    }

    private func hitsText(_ alias: AliasView) -> String {
        renderLocalizedText(aliasHitsLabel(
            hitCount: alias.hitCount,
            lastHitDate: alias.lastHitAtMs.map { ValueFormat.absoluteDate(epochMs: $0) }))
    }
}

// MARK: - Add / edit sheet (`mail-aliases-add-sheet-*`)

/// The add sheet's three-way kind choice (`mail-aliases.md` § Layout): the picker
/// steps Exact → Wildcard → Disposable, carried as its `kind` attr.
private enum AliasKindChoice: String, CaseIterable {
    case exact, wildcard, disposable

    var label: String {
        switch self {
        case .exact: return L.mailAliases.kindExact
        case .wildcard: return L.mailAliases.kindWildcard
        case .disposable: return L.mailAliases.kindDisposable
        }
    }

    var next: AliasKindChoice {
        switch self {
        case .exact: return .wildcard
        case .wildcard: return .disposable
        case .disposable: return .exact
        }
    }
}

/// Add a new exact/wildcard alias (`Create`), mint a disposable one
/// (`GenerateDisposable` with the chosen lifetime and use count — the pattern is
/// minted, not typed), or edit an existing one (`Update`, full-overwrite — kind
/// is fixed).
struct MailAliasAddSheet: View {
    let vm: MailAliasesVM
    let editing: AliasView?

    @Environment(\.dismiss) private var dismiss
    @State private var kind: AliasKindChoice = .exact
    @State private var pattern = ""
    @State private var label = ""
    @State private var spamThreshold = ""
    @State private var ratePerHour = ""
    @State private var ttl = ""
    @State private var uses = ""
    @State private var submitting = false
    @State private var error: String?

    private var isEditing: Bool { editing != nil }

    var body: some View {
        Form {
            Section {
                Picker(selection: $kind) {
                    ForEach(AliasKindChoice.allCases, id: \.self) { Text($0.label).tag($0) }
                } label: {
                    Text(L.mailAliases.kindWildcardLabel)
                }
                .pickerStyle(.segmented)
                .disabled(isEditing)
                .accessibilityIdentifier(Ids.mailAliasesAddSheetKindPicker)
                // One click steps Exact → Wildcard → Disposable (tui and android's
                // shape); the selection reads back as the `kind` attr. Disabled
                // while editing (kind is fixed on update).
                .automationActivate(
                    Ids.mailAliasesAddSheetKindPicker,
                    isEnabled: { !isEditing },
                    value: { kind.rawValue },
                    attributes: { ["kind": kind.rawValue] }
                ) { kind = kind.next }
                if kind != .disposable {
                    TextField(L.mailAliases.patternPlaceholder, text: $pattern)
                        .accessibilityIdentifier(Ids.mailAliasesAddSheetPatternInput)
                        .automationField(Ids.mailAliasesAddSheetPatternInput, text: $pattern)
                }
                TextField(L.mailAliases.labelPlaceholder, text: $label)
                    .accessibilityIdentifier(Ids.mailAliasesAddSheetLabelInput)
                    .automationField(Ids.mailAliasesAddSheetLabelInput, text: $label)
                // The mint takes neither a spam threshold nor an hourly limit, so
                // the Disposable choice hides both (as tui does).
                if kind != .disposable {
                    TextField(L.mailAliases.spamThresholdPlaceholder, text: $spamThreshold)
                        .accessibilityIdentifier(Ids.mailAliasesAddSheetSpamThresholdInput)
                        .automationField(Ids.mailAliasesAddSheetSpamThresholdInput, text: $spamThreshold)
                    TextField(L.mailAliases.ratePerHourPlaceholder, text: $ratePerHour)
                        .accessibilityIdentifier(Ids.mailAliasesAddSheetRatePerHourInput)
                        .automationField(Ids.mailAliasesAddSheetRatePerHourInput, text: $ratePerHour)
                }
            }

            // Disposable lifetime and use count (`mail-aliases.md` § Disposable);
            // an empty field means the per-user default.
            if kind == .disposable {
                Section {
                    TextField(L.mailAliases.ttlPlaceholder, text: $ttl)
                        .accessibilityIdentifier(Ids.mailAliasesAddSheetTtlInput)
                        .automationField(Ids.mailAliasesAddSheetTtlInput, text: $ttl)
                    TextField(L.mailAliases.usesPlaceholder, text: $uses)
                        .accessibilityIdentifier(Ids.mailAliasesAddSheetUsesInput)
                        .automationField(Ids.mailAliasesAddSheetUsesInput, text: $uses)
                }
            }

            if let error {
                Section { ErrorBanner(message: error) }
            }

            Section {
                Button(submitting ? "…" : (isEditing ? L.common.save : L.mailAliases.submit)) {
                    Task { await submit() }
                }
                .disabled(!canSubmit)
                .accessibilityIdentifier(Ids.mailAliasesAddSheetSubmitButton)
                .automationActivate(
                    Ids.mailAliasesAddSheetSubmitButton,
                    isEnabled: { canSubmit }
                ) { Task { await submit() } }
                // The sheet's one commit, and the page's second state-dependent
                // gesture: `submit()` branches on exactly this `editing != nil`,
                // so the declaration reads the same discriminant rather than
                // picking one of the two kinds (tui's `MailAliasesSubmit` is
                // left `None` for want of the mode on its action; SwiftUI has it
                // right here). Both arms OnlineOnly — the fields and Cancel
                // beside it are the buffer and stay live.
                .faunaGate(isEditing
                           ? "fauna.bridges.update_account_alias"
                           : (kind == .disposable
                              ? "fauna.bridges.generate_disposable_alias"
                              : "fauna.bridges.create_account_alias"))
                Button(L.mailAliases.cancel, role: .cancel) { dismiss() }
                    .accessibilityIdentifier(Ids.mailAliasesAddSheetCancelButton)
                    .automationActivate(Ids.mailAliasesAddSheetCancelButton) { dismiss() }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            if let editing {
                kind = editing.kind == .wildcard ? .wildcard : .exact
                pattern = editing.pattern
                label = editing.label
                spamThreshold = editing.spamThresholdOverride.map(String.init) ?? ""
                ratePerHour = editing.rateLimitPerHour.map(String.init) ?? ""
            }
        }
    }

    /// A disposable address is minted, so its pattern is not required.
    private var canSubmit: Bool {
        guard !submitting else { return false }
        return kind == .disposable || !pattern.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        if kind == .disposable && !isEditing {
            await vm.dispatch(.generateDisposable(
                ttlDays: parseCount(input: ttl), uses: parseCount(input: uses), label: label))
            if let e = vm.errorMessage { error = e } else { dismiss() }
            return
        }
        let spam = parseCount(input: spamThreshold)
        let rate = parseCountI64(input: ratePerHour)
        let action: MailAliasesAction
        if let editing {
            action = .update(
                aliasIdHex: editing.aliasIdHex, pattern: pattern, label: label,
                spamThresholdOverride: spam, rateLimitPerHour: rate
            )
        } else {
            action = .create(
                kind: kind == .wildcard ? .wildcard : .exact, pattern: pattern, label: label,
                spamThresholdOverride: spam, rateLimitPerHour: rate
            )
        }
        await vm.dispatch(action)
        if let e = vm.errorMessage { error = e } else { dismiss() }
    }
}

// MARK: - Bulk paste-import sheet (`mail-aliases-import-*`)

/// The recipient-whitelist bulk import (`mail-aliases.md` § Bulk import): paste
/// one full address per line → `MailAliasesAction::Import` →
/// `fauna.bridges.import_account_aliases`. Best-effort per line — a malformed,
/// duplicate, or foreign-domain line is *reported*, never aborting the batch, so
/// one typo can't block the other 99. Structurally mirrors
/// `MailListMemberImportSheet` (the sheet ui.yaml's `mail-aliases-import-sheet`
/// points at) and the linux lead `settings/mail_aliases.rs`.
///
/// **The sheet stays open on submit** and renders the outcome in place; only
/// Cancel closes it. That is load-bearing, not cosmetic: `mail-aliases-import-result`
/// lives inside the sheet, and the e2e's `read_import_result()` polls
/// `is_visible(...)` — dismissing on success would hide the very element the
/// assertion reads (the windows leg's latent bug, `MailAliasesPanel.xaml.cs:230`).
struct MailAliasImportSheet: View {
    let vm: MailAliasesVM

    @Environment(\.dismiss) private var dismiss
    @State private var lines = ""
    @State private var submitting = false
    @State private var error: String?
    /// Gates the outcome render to *this* presentation, so reopening the sheet
    /// starts clean instead of showing the previous batch's summary (the machine
    /// keeps `last_import_result` until the next dispatch — linux clears the same
    /// way on sheet-open, `mail_aliases.rs:606`).
    @State private var didSubmit = false

    private var result: ImportResultView? {
        didSubmit ? vm.snapshot?.lastImportResult : nil
    }

    var body: some View {
        Form {
            Section(L.mailAliases.importTitle) {
                Text(L.mailAliases.importSubtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // One full address per line. A multi-line editor — the bound
                // storage is what the driver's clear_and_type writes.
                TextEditor(text: $lines)
                    .frame(minHeight: 140)
                    .font(.body.monospaced())
                    .accessibilityIdentifier(Ids.mailAliasesImportTextarea)
                    .automationField(Ids.mailAliasesImportTextarea, text: $lines)
            }

            if let result {
                Section {
                    // Dynamic label — test-id only, no static label text.
                    // `resultText` joins the summary with one
                    // `import_invalid_line` row per invalid outcome inside
                    // this SAME registered string — a sibling Text with no
                    // automation id is invisible to the in-process
                    // automation registry (confirmed by e2e: the reasons
                    // never appeared under a separate untagged ForEach row),
                    // so the reason must live in the one registered value,
                    // mirroring linux's single `import_result` gtk::Label and
                    // web's single `result-block` <p> (mail-aliases.md:199-201
                    // requires the reason be rendered, not just tallied).
                    automationText(Ids.mailAliasesImportResult, resultText(result))
                        .font(.callout)
                        // Re-submitting inside the SAME presentation changes the
                        // summary without re-creating the Section, so the
                        // `.onAppear`-once registration would keep serving the
                        // FIRST batch's text. Keying identity on the rendered
                        // string forces `_AutomationRegister` to re-register the
                        // current closure. See AdminMailView's
                        // publish-spam-baseline-result (same remedy) and
                        // apple-e2e-automation.md § Stale-capture discipline.
                        .id("mail-aliases-import-result:\(resultText(result))")
                }
            }

            if let error {
                Section { ErrorBanner(message: error) }
            }

            Section {
                Button(submitting ? "…" : L.mailAliases.importSubmit) {
                    Task { await submit() }
                }
                .disabled(submitting || trimmedLines.isEmpty)
                .accessibilityIdentifier(Ids.mailAliasesImportSubmitButton)
                .automationActivate(
                    Ids.mailAliasesImportSubmitButton,
                    isEnabled: { !submitting && !trimmedLines.isEmpty }
                ) { Task { await submit() } }
                .faunaGate("fauna.bridges.import_account_aliases")
                Button(L.mailAliases.importCancel, role: .cancel) { dismiss() }
                    .accessibilityIdentifier(Ids.mailAliasesImportCancelButton)
                    .automationActivate(Ids.mailAliasesImportCancelButton) { dismiss() }
            }
        }
        .formStyle(.grouped)
    }

    /// Blank lines are trimmed and produce no outcome (`mail-aliases.md`
    /// § Bulk import) — the machine trims too; doing it here also keeps Submit
    /// disabled on a whitespace-only paste.
    private var trimmedLines: [String] {
        lines.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    private func summary(_ r: ImportResultView) -> String {
        L.mailAliases.importResult(
            created: String(r.created),
            existed: String(r.skippedDuplicate),
            invalid: String(r.invalid))
    }

    /// The full `mail-aliases-import-result` text: the summary plus one
    /// `import_invalid_line` row per invalid outcome, newline-joined —
    /// mirrors linux `mail_aliases.rs`'s `import_result.set_text` build.
    private func resultText(_ r: ImportResultView) -> String {
        var text = summary(r)
        for outcome in invalidOutcomes(r) {
            text += "\n" + L.mailAliases.importInvalidLine(
                address: outcome.address,
                reason: outcome.reason ?? "")
        }
        return text
    }

    private func invalidOutcomes(_ r: ImportResultView) -> [ImportAliasOutcomeView] {
        r.outcomes.filter { $0.status == .invalid }
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        // `import` is a Swift keyword — UniFFI emits the case backticked
        // (`case \`import\`(lines: [String])`), so the call site needs them too.
        await vm.dispatch(.`import`(lines: trimmedLines))
        didSubmit = true
        error = vm.errorMessage
    }
}
