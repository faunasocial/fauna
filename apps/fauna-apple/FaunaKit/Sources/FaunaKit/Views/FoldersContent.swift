import SwiftUI

/// Shared (macOS + iOS) renderer for the **Settings → Folders** page — the
/// folder **control plane**: the folder list with per-set in-place config
/// (selective-sync paths + the conflict-policy picker), the create wizard, the
/// per-set conflict surface, and — Apple-only — the "Photo Library" Backup-mode
/// set's photo-backup controls. All state is read off `DevicesMachineVM.snapshot`
/// with no client-side folder / conflict / wizard logic.
/// Target state: `docs/goal/ui/folders.md`.
///
/// **2026-06-28 sync/folder UI unification:** this surface is split out of the
/// former combined "Peers" / Devices page (the roster stays on `DevicesContent`,
/// devices.md) and absorbs the former `Settings → Sync` local-folder binding. The
/// desktop local-folder binding (`folder-location-*`, `platform_elements` —
/// macOS/Windows/Linux) is **nested under each folder** and injected by the
/// platform shell via `perSetBinding` (iOS/web have no local folders, so the
/// default is empty). The Apple photo-backup controls (`photo-backup-*`) are
/// re-homed here from the old Media tab / standalone settings page as the "Photo
/// Library" set's config (media.md § Apple photo-backup reframe).
public struct FoldersContent<BindingContent: View>: View {
    let vm: DevicesMachineVM
    /// Desktop-only local-folder binding for one folder, nested under its row.
    /// macOS fills this with the `LocationsModel`-backed `folder-location-*` UI;
    /// iOS passes the default empty view (no user-bindable local folder tree).
    @ViewBuilder let perSetBinding: (FolderSummary) -> BindingContent
    /// The refs (`FolderRef` wire strings) of the sets whose local presence is a
    /// bound always-resident folder — the
    /// `folder-on-demand-toggle` arbitration input (bound folder > FP domain;
    /// `file-sync.md` § Apple File Provider binding). macOS reads
    /// `LocationsModel`; iOS (no binding surface) keeps the empty default.
    let boundSets: () -> Set<String>
    /// Shared-set names the agent has PARKED (the owner revoked this actor's
    /// write grant mid-life) — the `parked` input to `bindingSection`, which
    /// keeps a demoted writer's binding and heads it with
    /// `folder-access-revoked-warning`. macOS reads
    /// `LocationsModel.revokedFolders`; iOS (no binding surface, so nothing
    /// to park) keeps the empty default.
    let revokedSets: () -> Set<String>
    /// Re-converge the FP domains after an on-demand toggle flip (the app's
    /// `FileProviderCoordinator.reconcile` hook); `nil` defers to the next
    /// launch reconcile.
    let onDemandChanged: (() -> Void)?

    /// Name of the single expanded folder row (only one open at a time so the
    /// unindexed body widgets — `folder-include-paths`, `folder-delete-button`,
    /// … — resolve uniquely to it).
    @State private var expandedFolder: String?

    public init(
        vm: DevicesMachineVM,
        boundSets: @escaping () -> Set<String> = { [] },
        revokedSets: @escaping () -> Set<String> = { [] },
        onDemandChanged: (() -> Void)? = nil,
        @ViewBuilder perSetBinding: @escaping (FolderSummary) -> BindingContent
    ) {
        self.vm = vm
        self.boundSets = boundSets
        self.revokedSets = revokedSets
        self.onDemandChanged = onDemandChanged
        self.perSetBinding = perSetBinding
    }

    // Rendered as an eager `ScrollView { VStack }` rather than a lazy `List`
    // (apple-e2e-automation.md registration rule 6, extended to macOS `List`
    // 2026-07-30): a macOS `List` is an NSTableView that realizes rows lazily,
    // so a `folder-row` appended to `vm.snapshot.folders` can go unbuilt
    // indefinitely — no view, no `.onAppear`, no registration (measured on the
    // Events agenda; `test_delete_folder` polls the row count/identity the
    // same way this page would be tested). Shared with iOS — both apple
    // targets render this eagerly (rule 6's own statement: "on BOTH apple
    // apps"). Cost: rows lose native List section-separator styling, the
    // accepted rule-6 tradeoff every other converted page already pays.
    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                photoLibrarySection
                pendingSharesSection
                #if !FAUNA_EXCISE_P2P_SHARE
                offlineShareSection
                sharePlaneSection
                #endif
                conflictSection
                folderSection
                // Folders the user FOLLOWS — their own list, never
                // `folder-row`s (ui/folders.md § Following a public folder).
                // Always offered, even with none: the button is how a user gets
                // their first one.
                FollowedFoldersSection(vm: vm)
                Divider()
                SyncDefaultsSection(vm: vm)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .safeAreaInset(edge: .bottom) {
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
                    .padding()
            }
        }
        .sheet(isPresented: Binding(
            get: { vm.snapshot?.wizard != nil },
            set: { open in if !open { vm.closeWizard() } }
        )) {
            FolderWizardSheetView(vm: vm)
        }
    }

    // MARK: - Photo Library (Apple-only — the photo-backup ingress set's config)

    /// The "Photo Library" Backup-mode set's configuration — the shared FaunaKit
    /// `PhotoBackupControlsView` (`photo-backup-*`), re-homed here from the former
    /// Apple Media tab / standalone `photo-backup` settings page (media.md § Apple
    /// photo-backup reframe; ui.yaml `photo-backup-controls` `used_in: [folders]`).
    @ViewBuilder
    private var photoLibrarySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.folders.photoLibrarySection)
                .font(.headline)
            PhotoBackupControlsView()
        }
    }

    // MARK: - Conflicts (candidate model — rendered per set when present)

    @ViewBuilder
    private var conflictSection: some View {
        let conflicts = vm.snapshot?.conflicts ?? []
        if !conflicts.isEmpty {
            Divider()
            VStack(alignment: .leading, spacing: 8) {
                Text(L.devices.conflicts.sectionTitle)
                    .font(.headline)
                ForEach(Array(conflicts.enumerated()), id: \.offset) { _, conflict in
                    ConflictRow(vm: vm, conflict: conflict)
                }
            }
        }
    }

    // MARK: - Shared with you (recipient-side pending knocks)

    /// The recipient-side "Shared with you" pending area — a stranger's staged shares
    /// (`folder-pending-share`) awaiting accept/decline. Present only when there are
    /// knocks (a contact's share auto-joins via the B2 gate, so it never knocks).
    /// `docs/goal/ui/folders.md` § Sharing — Recipient side.
    ///
    /// The co-present ceremony's own consent-card invitations
    /// (`GroupInvitationRow`) continue this SAME indexed list, appended after
    /// the M2 shares (`p2p.md` § Offline share initiation → *Built — the
    /// affordance, both roles*: "the consent card mints no ids... a user sees
    /// one list of things awaiting an answer").
    @ViewBuilder
    private var pendingSharesSection: some View {
        let pending = vm.pendingShares
        if !pending.isEmpty || ceremonyInvitationCount > 0 {
            Divider()
            VStack(alignment: .leading, spacing: 8) {
                Text(L.devices.sharedWithYou)
                    .font(.headline)
                ForEach(Array(pending.enumerated()), id: \.offset) { offset, share in
                    FolderPendingShareRow(vm: vm, share: share)
                        // Scope path for the indexed `folder-pending-share` (mirrors
                        // `media-item` / `folder-member-item`) so scoped child reads resolve.
                        .automationScope(Ids.folderPendingShare, index: offset)
                }
                #if !FAUNA_EXCISE_P2P_SHARE
                ForEach(Array(vm.groupShareViews.invitations.enumerated()), id: \.offset) {
                    offset, invitation in
                    GroupInvitationRow(vm: vm, invitation: invitation)
                        // Continues the M2 shares' own index — one indexed list.
                        .automationScope(Ids.folderPendingShare, index: pending.count + offset)
                }
                #endif
            }
        }
    }

    // MARK: - Offline co-present share (initiate/receive ceremony)

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the `p2p-share` member's ceremony half
    // (the reason is written once, at the top of `SharePlaneModel.swift`). The two
    // shared lists it extends (`pendingSharesSection`, `folderSection`) read only
    // these counts outside the condition, so an excised build lists no ceremony row.
    #if !FAUNA_EXCISE_P2P_SHARE
    private var ceremonyInvitationCount: Int { vm.groupShareViews.invitations.count }
    private var ceremonyScopeCount: Int { vm.groupShareViews.scopes.count }

    /// The co-present offline-share panel — the two entry buttons + the open
    /// initiator/recipient panel (`OfflineShareSectionView`). Hidden entirely
    /// while the affordance is unavailable (no usable identity secret, or the
    /// nest does not advertise `p2p-share` — `docs/goal/ui/folders.md`
    /// § Offline share initiation).
    @ViewBuilder
    private var offlineShareSection: some View {
        if let view = vm.offlineShareView, view.available {
            Divider()
            OfflineShareSectionView(vm: vm)
        }
    }
    #else
    private var ceremonyInvitationCount: Int { 0 }
    private var ceremonyScopeCount: Int { 0 }
    #endif

    // MARK: - Peer-transfer surface

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE`: the store-safe FFI exports no share plane
    // (the reason is written once, at the top of `SharePlaneModel.swift`).
    #if !FAUNA_EXCISE_P2P_SHARE

    /// The peer-transfer plane's readings, **directly under the co-present offline-share
    /// panel** — tui's and linux's placement (`p2p.md` § Cross-user shared-set transfer).
    /// Renders nothing until a plane is running this session (`SharePlaneSectionView`).
    @ViewBuilder
    private var sharePlaneSection: some View {
        if SharePlaneModel.shared.view != nil {
            Divider()
            SharePlaneSectionView(model: .shared)
        }
    }

    #endif

    // MARK: - Folders

    @ViewBuilder
    private var folderSection: some View {
        let folders = vm.snapshot?.folders ?? []
        // The co-present ceremony's own group-scope listing — sets like any
        // other, so they are `folder-row`s like any other, appended AFTER the
        // M2 sets and continuing the SAME index (`p2p.md` § Offline share
        // initiation → *Built — the affordance, both roles*: "a landed scope
        // lists as an ordinary set row — no new ids"). Mirrors linux's
        // `update_folder_list` appending `build_group_scope_row` after the
        // M2 rows.
        Divider()
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(L.devices.folders)
                Spacer()
                Button {
                    vm.openWizard()
                } label: {
                    Image(systemName: "plus")
                }
                .accessibilityIdentifier(Ids.folderAddButton)
                // Same `openWizard()` the Button action runs. Env-gated no-op.
                .automationActivate(Ids.folderAddButton) { vm.openWizard() }
            }
            if folders.isEmpty, ceremonyScopeCount == 0 {
                Text(L.devices.noFolders)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(folders.enumerated()), id: \.element.name) { offset, folder in
                    // A set shared WITH us (`role == "member"`) is READ-ONLY by
                    // default — no owner affordances (no paths, policy, delete,
                    // share). A `writer`-access member additionally gets the ONE
                    // management affordance a member ever gets: the local-folder
                    // binding (`perSetBinding` — the writer half of the share; a
                    // reader stays fully read-only, file-sync.md's iron rule). Only
                    // sets this client has actually MLS-joined reach us: the
                    // machine's injected `MlsQuery` join-filter drops
                    // rostered-but-un-joined ones, so a stranger's knock can never
                    // appear here (folders.md § Sharing).
                    //
                    // Which member rows carry the binding, and whether the
                    // revoked warning heads it, is the ONE shared decision
                    // (`bindingSection`, file-sync.md § Multi-writer shared sets
                    // → *Revocation*) — never the access alone: a demotion is
                    // exactly what turns a parked writer's access to `reader`,
                    // and keying on it hid the park on the first refresh after.
                    Group {
                        if folder.role == "member" {
                            let section = bindingSection(
                                role: folder.role, access: folder.access,
                                parked: revokedSets().contains(folder.name))
                            if section.shown {
                                writerMemberFolderRow(folder, revokedWarning: section.revokedWarning)
                            } else {
                                memberFolderRow(folder)
                            }
                        } else {
                            folderRow(folder)
                        }
                    }
                    // Scope path for the indexed `folder-row` (mirrors `folder-pending-
                    // share` / `media-item`) so the per-row controls rendered above the
                    // expander gate — `folder-conflict-policy-select`,
                    // `folder-webdav-toggle` — resolve by real subtree
                    // containment (`scope="folder-row[i]"`) instead of a global
                    // occurrence-index that silently mismatches once rows render
                    // different per-row controls (folders.md § Element IDs; e2e
                    // rule 1 — scoped queries).
                    .automationScope(Ids.folderRow, index: offset)
                }
                #if !FAUNA_EXCISE_P2P_SHARE
                ForEach(Array(vm.groupShareViews.scopes.enumerated()), id: \.offset) { offset, scope in
                    GroupScopeRow(scope: scope)
                        // Continues the M2 folders' own index — one indexed list.
                        .automationScope(Ids.folderRow, index: folders.count + offset)
                }
                #endif
            }
        }
    }

    @ViewBuilder
    private func folderRow(_ folder: FolderSummary) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button {
                    toggleFolder(folder.name)
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: expandedFolder == folder.name
                              ? "chevron.down" : "chevron.right")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(folder.name)
                            .font(.body)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                Spacer()

                // `folder-shared-badge` ("Shared · N") — marks a cross-user shared
                // set; visible even collapsed (folders.md § Sharing). N = member
                // count from the eager-loaded actor roster.
                sharedBadge(folder)
                // (The per-set scan-frequency picker that used to sit here retired
                // with folders re-model phase 5: the cadence is a constant, not a
                // choice — file-sync.md § Config, the phase-5 block.)
                // Per-set conflict policy (`folder-conflict-policy-select`) — every
                // owner row: a folder has no type (folders.md § Conflicts).
                FolderConflictPolicyPicker(vm: vm, folder: folder)
            }

            if expandedFolder == folder.name {
                FolderRowBody(
                    vm: vm, folder: folder,
                    isBoundToLocation: folder.folderRef.map { boundSets().contains($0) } ?? false,
                    onDemandChanged: onDemandChanged
                ) {
                    expandedFolder = nil
                }
                // Per-set device activity (file-sync.md § Implementation status
                // today) — the ordinary sync-mode change signal, web/tui/linux/
                // android/windows' last remaining leg. Owner rows only (mirrors
                // the "Shared with" section below it — a writer-member row
                // carries none of these row-detail reads).
                FolderDeviceActivitySection(vm: vm, folder: folder)
                // Owner-side cross-user "Shared with" section (folders.md § Sharing;
                // the linux LEAD shape) — share affordance + member roster.
                FolderSharedWithSection(vm: vm, folder: folder)
                // Owner-side "Destination places" section (backup-destinations.md
                // § Ordinary-folder coverage) — attached
                // backup destinations for this folder + the attach affordance.
                FolderDestinationPlacesSection(vm: vm, folder: folder)
                // Desktop local-folder binding, nested under this set (no-op on
                // iOS/web). The set is contextual — the binding records only the
                // folder path (no free-text set name; D3 / O-1).
                perSetBinding(folder)
            }
        }
        .padding(.vertical, 2)
        // Eager-load the actor roster for a SHARED set (`mlsGroupId != nil`) so the
        // `folder-shared-badge` renders on the collapsed row; owner-only sets need
        // no read (no badge, and their read would answer `not_shared`). `id:` re-runs
        // when a set flips nil→shared after a share.
        .task(id: folder.mlsGroupId) {
            if folder.mlsGroupId != nil {
                await vm.loadFolderActors(name: folder.name)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderRow)
        // Presence + click toggle for the indexed `folder-row` (one Entry per row);
        // reads the row name for presence. Env-gated no-op in production. `.contain`
        // (above) keeps the row's child ids (`folder-conflict-policy-select`, the expander
        // body, the Shared-with section) queryable alongside this container id.
        .automationActivate(Ids.folderRow, value: { folder.name }) {
            toggleFolder(folder.name)
        }
    }

    // MARK: - Shared-with-me row (recipient side)

    /// One set shared WITH us — the recipient counterpart of `folderRow`. Read-only
    /// by construction (members read + decrypt, never write — folders.md § Sharing),
    /// so it does not expand: no path editors, no delete, no share, no local
    /// folder binding. It carries the recipient `folder-shared-badge` variant
    /// ("Shared by ‹handle›"), the `folder-leave-button`, and the hide-only
    /// `folder-on-demand-toggle`.
    @ViewBuilder
    private func memberFolderRow(_ folder: FolderSummary) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(folder.name)
                    .font(.body)

                Spacer()

                // `folder-shared-badge`, recipient variant — the SAME id the owner's
                // "Shared · N" uses, by design (folders.md § Element IDs: one id, two
                // variants by context). `ownerDisplay` is the shared precomputed label
                // (handle, else canonical short id — `value-formatting.md` § Account
                // display label); non-empty on every member row.
                automationText(Ids.folderSharedBadge, L.devices.sharedBy(who: folder.ownerDisplay))
                    .font(.caption2)
                    .foregroundStyle(.tint)

                // Leave — self-scoped: drops only our roster row + forgets the group. No
                // key rotation (a voluntary leaver keeps the generations they held). The
                // row disappears on the refresh that follows. `mlsGroupId` is always
                // present on a member row (the join-filter proves we joined the group);
                // the guard is belt-and-braces.
                if let groupId = folder.mlsGroupId, !groupId.isEmpty {
                    Button(L.common.leave) { leave(groupId) }
                        .font(.caption)
                        .buttonStyle(.borderless)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier(Ids.folderLeaveButton)
                        // Same `leave(...)` the Button action runs — a bare id is invisible
                        // to the in-process driver. Env-gated no-op in production.
                        .automationActivate(Ids.folderLeaveButton) { leave(groupId) }
                }
            }
            // A reader's row has nothing to expand, so its one control — the
            // hide-only on-demand toggle — sits on the row itself.
            memberOnDemandToggle(folder)
        }
        .padding(.vertical, 2)
        // `.contain` keeps the child ids (badge, leave button, on-demand toggle)
        // queryable alongside this container id (the Section-clobbers-children rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderRow)
        // Presence + name read for the indexed `folder-row`. Unlike the owner row
        // there is no activate — nothing to expand.
        .automationValue(Ids.folderRow, text: { folder.name })
    }

    // MARK: - Shared-with-me row, writer access (recipient side)

    /// A `role == "member"` row where `access == "writer"` — the ONLY management
    /// affordance a member ever gets is the local-folder binding (the writer half
    /// of the share; a reader stays fully read-only per `memberFolderRow` above,
    /// file-sync.md's iron rule). Expandable (mirrors the owner `folderRow`
    /// interaction, reusing the same `expandedFolder` state) so the binding has
    /// somewhere to live without cluttering the collapsed row; unlike the owner
    /// row, expanding reveals ONLY the binding (+ the revoked warning below) and
    /// the hide-only on-demand toggle —
    /// never paths/delete/share, which stay owner-only. Linux reference:
    /// `build_writer_member_folder_row`. Also the row a DEMOTED writer's parked
    /// binding keeps (`bindingSection` shows it whatever the access now reads),
    /// with `revokedWarning` heading the binding.
    @ViewBuilder
    private func writerMemberFolderRow(_ folder: FolderSummary, revokedWarning: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button {
                    toggleFolder(folder.name)
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: expandedFolder == folder.name
                              ? "chevron.down" : "chevron.right")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(folder.name)
                            .font(.body)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                Spacer()

                // Same recipient `folder-shared-badge` variant as the reader row.
                automationText(Ids.folderSharedBadge, L.devices.sharedBy(who: folder.ownerDisplay))
                    .font(.caption2)
                    .foregroundStyle(.tint)

                if let groupId = folder.mlsGroupId, !groupId.isEmpty {
                    Button(L.common.leave) { leave(groupId) }
                        .font(.caption)
                        .buttonStyle(.borderless)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier(Ids.folderLeaveButton)
                        .automationActivate(Ids.folderLeaveButton) { leave(groupId) }
                }
            }

            if expandedFolder == folder.name {
                // The owner withdrew this actor's write grant mid-life and the
                // agent parked the binding (D4, fail-closed AND loud). The row —
                // and the binding widget below it — stays visible and removable:
                // a park is not a deletion, and the user still needs to see what
                // was bound. Clears again the moment the grant returns.
                if revokedWarning {
                    automationText(Ids.folderAccessRevokedWarning, L.devices.accessRevokedWarning)
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                perSetBinding(folder)
                memberOnDemandToggle(folder)
            }
        }
        .padding(.vertical, 2)
        // `.contain` keeps the child ids (badge, leave button, the expander body's
        // binding controls) queryable alongside this container id.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderRow)
        .automationActivate(Ids.folderRow, value: { folder.name }) {
            toggleFolder(folder.name)
        }
    }

    /// `folder-on-demand-toggle` on a row of a folder shared WITH the account —
    /// hide-only (the membership is the seat; no place is written), and, for a
    /// writer whose set is bound to a local folder, the same arbitration
    /// caption an owner's bound row shows (`on-demand-files.md` § Shared sets
    /// on a capability host, decision 3; ui/folders.md § Element IDs).
    @ViewBuilder
    private func memberOnDemandToggle(_ folder: FolderSummary) -> some View {
        #if canImport(FileProvider)
            FolderOnDemandToggle(
                vm: vm, folder: folder,
                isBound: folder.folderRef.map { boundSets().contains($0) } ?? false,
                onChanged: onDemandChanged, hideOnly: true)
        #endif
    }

    private func leave(_ groupIdHex: String) {
        Task { await vm.leaveFolder(groupIdHex: groupIdHex) }
    }

    /// The `role == "member"` actors a set is shared with (owner excluded), from the
    /// VM's eager-loaded roster — drives both the badge count and the member list.
    /// The filter is shared Rust (`folderMemberActors`) — never re-derive it locally.
    private func memberActors(_ folder: FolderSummary) -> [FfiFolderActorMember] {
        folderMemberActors(actors: vm.folderActors[folder.name] ?? [])
    }

    /// `folder-shared-badge` — "Shared · N" for a shared set with ≥1 member; hidden
    /// for an owner-only set or before the async roster read returns (matches linux).
    @ViewBuilder
    private func sharedBadge(_ folder: FolderSummary) -> some View {
        let count = memberActors(folder).count
        if folder.mlsGroupId != nil, count > 0 {
            automationText(Ids.folderSharedBadge, L.devices.sharedBadge(count: String(count)))
                .font(.caption2)
                .foregroundStyle(.tint)
        }
    }

    /// Expand the named folder row (collapsing any other), or collapse it if it
    /// is already open. Shared by the row Button action and its automation Entry.
    private func toggleFolder(_ name: String) {
        expandedFolder = (expandedFolder == name) ? nil : name
    }
}

/// Convenience init for clients with no local-folder binding (iOS / web have no
/// user-bindable local folder tree) — the per-set binding is an empty view and
/// no set is ever folder-bound (the on-demand toggle is the only presence
/// affordance there).
extension FoldersContent where BindingContent == EmptyView {
    public init(vm: DevicesMachineVM, onDemandChanged: (() -> Void)? = nil) {
        self.init(
            vm: vm, onDemandChanged: onDemandChanged, perSetBinding: { _ in EmptyView() })
    }
}

// MARK: - Conflict policy (per set + the page-level default)
//
// Option set + labels come from the shared catalog (`conflictPolicyOptions()` /
// `conflictPolicyLabel(value:)`, `libs/fauna-folders-machine` via fauna-ffi —
// `folders.md` § Where logic lives) — the catalog, never a hand-rolled map.
// `auto` (merge text-like files,
// else latest-wins) is the wire/nest-column default an unset policy means.

/// In-place per-set conflict policy (`folder-conflict-policy-select`, indexed) on a
/// owner row. Applies via the shared `DevicesMachine::set_folder_conflict_policy`
/// (`fauna.folders.update`) — the nest row is authoritative and the engine reads it
/// at resolution time (file-sync.md § Conflicts, policy).
private struct FolderConflictPolicyPicker: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    /// An unset policy means the nest column default — `auto`.
    private var current: String { folder.conflictPolicy ?? "auto" }

    var body: some View {
        Picker("", selection: Binding(
            get: { current },
            set: { newVal in apply(newVal) }
        )) {
            ForEach(conflictPolicyOptions(), id: \.value) { option in
                Text(renderLocalizedText(option.label)).tag(option.value)
            }
        }
        .labelsHidden()
        .controlSize(.small)
        .accessibilityIdentifier(Ids.folderConflictPolicySelect)
        // Drives the same `set_folder_conflict_policy` the Picker does; the value is
        // the wire string. Env-gated no-op in production.
        .automationSelect(
            Ids.folderConflictPolicySelect,
            value: { current },
            set: { v in apply(v) }
        )
    }

    private func apply(_ policy: String) {
        Task { await vm.setFolderConflictPolicy(name: folder.name, policy: policy) }
    }
}

/// The `folder-webdav-toggle`'s hint. The "set up mail first" line **replaces** the
/// usual explainer (rather than joining it) while the actor holds no MSEK — serving
/// seals the `WebdavKeysBlob` under the mail encryption key, so the flip cannot succeed
/// without mail (webdav-server.md § Independent enablement point 2).
func serveWebdavHintText(canServe: Bool) -> String {
    canServe ? L.devices.serveWebdavHint : L.devices.serveWebdavNeedsMail
}

/// Per-set "serve over WebDAV" opt-in (`folder-webdav-toggle`, indexed) — default OFF.
/// ON drives content-key genesis/migration + the `WebdavKeysBlob` provision; OFF rotates
/// the content key + re-provisions the blob without the set (`FoldersAuthor::serve_set`).
///
/// **Disabled — not merely error-on-click — while the actor holds no MSEK.** `serve_set`
/// flips the nest `webdav_enabled` flag BEFORE it re-provisions the blob, so a doomed
/// enable would commit the flag and only then fail `NoMsek`, leaving the nest marked as
/// serving a set whose keys the MDA never received. The capability rides the key-bearing
/// `folders_can_serve_webdav` face, cached per page-visit on the VM (it is NOT on the
/// keyless `DevicesMachine` snapshot).
private struct FolderWebdavToggle: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var isOn: Bool { folder.webdavEnabled }
    private var canServe: Bool { vm.canServeWebdav }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Toggle(isOn: Binding(
                get: { isOn },
                set: { on in apply(on) }
            )) {
                Text(L.devices.serveWebdav)
            }
            .disabled(!canServe)
            .accessibilityIdentifier(Ids.folderWebdavToggle)
            // One Entry carries the click, the "on"/"off" read, AND the enabled state —
            // the driver asserts the no-MSEK case via `is_disabled` (no separate ID;
            // ui.yaml `folder-webdav-toggle`). `isEnabled` mirrors the `.disabled(...)`
            // predicate above so the two can never silently diverge. Env-gated no-op.
            .automationActivate(
                Ids.folderWebdavToggle,
                isEnabled: { canServe },
                value: { isOn ? "on" : "off" }
            ) { apply(!isOn) }
            // BOTH directions run `serve_set`, whose binding leg re-provisions
            // the `WebdavKeysBlob` — the flip's cheap `OfflineSafe` flag is not
            // the kind that matters here (tui declares the same one on
            // `ToggleFolderWebdav`, for the reason its type doc gives: a
            // half-applied flip is exactly what a no-nest enable leaves behind).
            .faunaGate("fauna.bridges.provision_webdav_keys_blob")

            Text(serveWebdavHintText(canServe: canServe))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// Guarded by the same `canServe` predicate the toggle is `.disabled(...)` on, so a
    /// synthesized activate on a disabled control can never reach `serve_set` either.
    private func apply(_ enable: Bool) {
        guard canServe else { return }
        Task {
            await vm.serveFolderWebdav(
                name: folder.name, mlsGroupIdHex: folder.mlsGroupId, enable: enable)
        }
    }
}

/// Per-set "paywall to tier" select (`folder-paywall-tier-select`) — website-enabled
/// OWNER rows only, the structural sibling of the `FolderWebdavToggle` above
/// (folders.md § Web paywall / monetization.md § Pillar 2; the linux
/// reference). NOT a `DevicesMachine` config write: picking a tier runs the full
/// paywall orchestration via the shared `FoldersAuthor::paywall_set`
/// (`APIClient.paywallFolder`).
///
/// Same value/label split as the conflict select: the tag/wire values the
/// cross-app `select(id, value)` e2e contract drives are each own-tier's NAME, plus
/// an empty-string sentinel for the "Not paywalled (public)" placeholder. **v1 is
/// SET-ONLY** (ratified 2026-07-13): the placeholder is offered only while the set is
/// still public — once paywalled there is no clear affordance (the nest-side
/// revoke/rotation leg is not shipped), so picking the placeholder never dispatches.
/// A tier the set is already paywalled to stays offered even if since deleted from
/// the tier list, so the row still shows its state. No tiers ⇒ nothing to paywall
/// to: the select is disabled with a "create a tier first" hint, mirroring the
/// webdav "set up mail first" gate.
private struct FolderPaywallTierPicker: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    /// `nil` = public (never paywalled). The select renders the empty sentinel then.
    private var current: String { folder.webPaywallTier ?? "" }
    private var hasTiers: Bool { !vm.ownTierNames.isEmpty }

    /// The offered wire values, mirroring linux's model construction: the empty
    /// placeholder only while public, every own tier, plus the already-set tier if
    /// it no longer exists in the tier list.
    private var values: [String] {
        var values: [String] = []
        if folder.webPaywallTier == nil { values.append("") }
        values.append(contentsOf: vm.ownTierNames)
        if let cur = folder.webPaywallTier, !vm.ownTierNames.contains(cur) {
            values.append(cur)
        }
        return values
    }

    private func label(_ value: String) -> String {
        value.isEmpty ? L.devices.paywallTierNone : value
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(L.devices.paywallTier)
                    .font(.caption)
                Spacer()
                Picker("", selection: Binding(
                    get: { current },
                    set: { newVal in apply(newVal) }
                )) {
                    ForEach(values, id: \.self) { value in
                        Text(label(value)).tag(value)
                    }
                }
                .labelsHidden()
                .controlSize(.small)
                .disabled(!hasTiers)
                .accessibilityIdentifier(Ids.folderPaywallTierSelect)
                // Drives the same `paywall_set` the Picker does; the value is the
                // tier's wire NAME. `isEnabled` mirrors the `.disabled(...)`
                // predicate so the two can never silently diverge. Env-gated no-op.
                .automationSelect(
                    Ids.folderPaywallTierSelect,
                    value: { current },
                    isEnabled: { hasTiers },
                    set: { v in apply(v) }
                )
            }

            Text(hasTiers ? L.devices.paywallTierHint : L.devices.paywallTierNeedsTier)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// The empty placeholder is not a write — v1 is set-only, there is no "clear the
    /// paywall" path yet. Only a real tier dispatches; guarded by the same `hasTiers`
    /// predicate the Picker is `.disabled(...)` on, so a synthesized select on a
    /// disabled control can never reach `paywall_set` either.
    private func apply(_ tier: String) {
        guard hasTiers, !tier.isEmpty, tier != current else { return }
        Task {
            await vm.paywallFolder(
                name: folder.name, mlsGroupIdHex: folder.mlsGroupId, tier: tier)
        }
    }
}

/// The page-level **Sync defaults** section — `sync-default-conflict-policy-select`,
/// the global default stamped onto NEWLY created sets (existing sets keep their own
/// per-set policy). Persisted in the owner's encrypted `fauna.state.sync-prefs` via the
/// shared `default_conflict_policy_{get,set}` free fns; injected into the create wizard
/// on open (`DevicesMachineVM.openWizard`). Unindexed — one global control.
private struct SyncDefaultsSection: View {
    let vm: DevicesMachineVM

    /// Never set ⇒ the nest column default, `auto`.
    private var current: String { vm.defaultConflictPolicy ?? "auto" }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.devices.syncDefaults)
                .font(.headline)
            HStack {
                Text(L.devices.defaultConflictPolicy)
                    .font(.caption)
                Spacer()
                Picker("", selection: Binding(
                    get: { current },
                    set: { newVal in apply(newVal) }
                )) {
                    ForEach(conflictPolicyOptions(), id: \.value) { option in
                        Text(renderLocalizedText(option.label)).tag(option.value)
                    }
                }
                .labelsHidden()
                .controlSize(.small)
                .accessibilityIdentifier(Ids.syncDefaultConflictPolicySelect)
                // Drives the same persisted write the Picker does. Env-gated no-op.
                .automationSelect(
                    Ids.syncDefaultConflictPolicySelect,
                    value: { current },
                    set: { v in apply(v) }
                )
            }
        }
    }

    private func apply(_ policy: String) {
        Task { await vm.setDefaultConflictPolicy(policy) }
    }
}

// MARK: - The nest place's snapshot policy (per folder)

/// The nest place's per-folder snapshot policy — `folder-nest-snapshots-select`
/// (three-state) + `-quiet-input` + `-retention-snapshots` + `-retention-days`,
/// saved together by `-save-button` (`backup-restore.md` § 8b). Renders on **any**
/// folder: this is what replaced the wizard's Backup-mode-only retention step,
/// because a folder no longer has a mode to gate it on.
///
/// Every format/parse rule lives in shared Rust
/// (`fauna_folders_machine::nest_place` via fauna-ffi's `nestPlaceEditFromRow` /
/// `nestPlaceWrite`), so this view stages four strings and nothing else. The two
/// rules that are traps rather than details, both unrepresentable here as a
/// result:
///
/// - **Blank is a VALUE**, the resting state of every folder and where a knob
///   RETURNS. An unset knob — and a *zero* retention bound, which is the nest's
///   own spelling of unset — prefills blank, never `"0"`.
/// - **Retention inverts the omission rule**: `retention_policy`'s wire `nil`
///   means *leave unchanged*, so a cleared retention rides as the canonical
///   binds-nothing policy. `nestPlaceWrite` is the only sanctioned way to build
///   the call.
///
/// Staged rather than applied-on-change (unlike `folder-conflict-policy-select`)
/// because the policy is sent WHOLE: an apply-on-change control would commit the
/// half-typed values of its three siblings.
private struct FolderNestPlaceEditor: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    /// Collapse the row after a successful save — the same callback
    /// `savePaths` / `confirmDelete` take, and for the same reason: the row's
    /// expander is a TOGGLE (`toggleFolder`), so a body that stays open after a
    /// write turns the user's (and the e2e's) next "expand this row" into a
    /// CLOSE, and every control in the body reads as gone. Leaving this out is
    /// what made `test_folder_nest_place.py`'s step 4 fail with "element not
    /// found" on `folder-nest-snapshots-select` after step 3 had saved happily.
    let collapse: () -> Void

    @State private var snapshots: String = ""
    @State private var quiet: String = ""
    @State private var retentionSnapshots: String = ""
    @State private var retentionDays: String = ""
    @State private var versionRetentionCount: String = ""
    @State private var versionRetentionDays: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.devices.nestPlaceSection)
                .font(.caption)
                .foregroundStyle(.secondary)

            Picker(L.devices.nestSnapshots, selection: Binding(
                get: { snapshots },
                set: { snapshots = $0 }
            )) {
                // Built from the shared catalog (value + i18n label), the same
                // value/display split every sibling picker uses — no per-app
                // option order or label map.
                ForEach(nestSnapshotsOptions(), id: \.value) { option in
                    Text(renderLocalizedText(option.label)).tag(option.value)
                }
            }
            .accessibilityIdentifier(Ids.folderNestSnapshotsSelect)
            // Round-trips the raw wire value (`default` | `on` | `off`), which is
            // what the cross-app `select(id, value)` contract drives. Staging
            // only — the write happens on save. Env-gated no-op in production.
            .automationSelect(
                Ids.folderNestSnapshotsSelect,
                value: { snapshots },
                set: { v in snapshots = v }
            )

            TextField(L.devices.nestQuiet, text: $quiet)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderNestQuietInput)
                .automationField(Ids.folderNestQuietInput, text: $quiet)

            TextField(L.devices.nestRetentionSnapshots, text: $retentionSnapshots)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderNestRetentionSnapshots)
                .automationField(Ids.folderNestRetentionSnapshots, text: $retentionSnapshots)

            TextField(L.devices.nestRetentionDays, text: $retentionDays)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderNestRetentionDays)
                .automationField(Ids.folderNestRetentionDays, text: $retentionDays)

            // The version-retention SIBLING pair (file-versions.md § Retention
            // ruling 1): bounds file-version history, never snapshots — its own
            // `folders.version_retention` column, riding the same save.
            TextField(L.devices.versionRetentionCount, text: $versionRetentionCount)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderVersionRetentionCount)
                .automationField(Ids.folderVersionRetentionCount, text: $versionRetentionCount)

            TextField(L.devices.versionRetentionDays, text: $versionRetentionDays)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderVersionRetentionDays)
                .automationField(Ids.folderVersionRetentionDays, text: $versionRetentionDays)

            // Not decoration: the only on-screen statement that emptying a box is
            // a real choice rather than a no-op.
            Text(L.devices.nestPlaceBlankHint)
                .font(.caption)
                .foregroundStyle(.secondary)

            Button(L.devices.nestSave) { save() }
                .accessibilityIdentifier(Ids.folderNestSaveButton)
                // Same `save()` the Button action runs. Env-gated no-op.
                .automationActivate(Ids.folderNestSaveButton) { save() }
        }
        // Seed on appear and re-seed whenever the row's stored policy changes —
        // the machine refreshes after a successful save, so the controls always
        // show what the nest actually holds rather than what was last typed.
        // One key over the three source values, because `onChange` takes a
        // single `Equatable` (a tuple of them does not conform).
        .onChange(of: rowPolicyKey, initial: true) { _, _ in prefill() }
    }

    /// The row's stored policy as one `Equatable` — the change signal for the
    /// re-seed above, never rendered.
    private var rowPolicyKey: String {
        let snapshots = folder.nestSnapshots.map(String.init) ?? ""
        let quiet = folder.nestSnapshotQuietSecs.map(String.init) ?? ""
        return "\(snapshots)|\(quiet)|\(folder.retentionPolicy ?? "")|"
            + "\(folder.versionRetentionMaxVersions)|\(folder.versionRetentionMaxAgeDays)"
    }

    /// Seed the six buffers from the row. Every prefill rule lives in shared
    /// Rust, so this is a transcription and nothing else.
    private func prefill() {
        let edit = nestPlaceEditFromRow(
            nestSnapshots: folder.nestSnapshots,
            nestSnapshotQuietSecs: folder.nestSnapshotQuietSecs,
            retentionPolicy: folder.retentionPolicy
        )
        snapshots = edit.snapshots
        quiet = edit.quietSecs
        retentionSnapshots = edit.retentionSnapshots
        retentionDays = edit.retentionDays
        let versionEdit = versionRetentionEditFromBounds(
            maxVersionsPerPath: folder.versionRetentionMaxVersions,
            maxAgeDays: folder.versionRetentionMaxAgeDays
        )
        versionRetentionCount = versionEdit.count
        versionRetentionDays = versionEdit.days
    }

    private func save() {
        let write = nestPlaceWrite(edit: NestPlaceEdit(
            snapshots: snapshots,
            quietSecs: quiet,
            retentionSnapshots: retentionSnapshots,
            retentionDays: retentionDays
        ))
        // Always non-nil on this call — the boxes are on screen (`nil` stays reserved for an app that has not built the editor at all).
        let versionRetention = versionRetentionWrite(edit: VersionRetentionEdit(
            count: versionRetentionCount,
            days: versionRetentionDays
        ))
        Task {
            await vm.setFolderNestPlace(
                name: folder.name,
                snapshots: write.snapshots,
                quietSecs: write.quietSecs,
                retention: write.retention,
                versionRetention: versionRetention
            )
            collapse()
        }
    }
}

// MARK: - Content residency (per folder)
//
// The nest place's own content property (folders re-model phase 5; file-sync.md
// § Content residency owns the model) — orthogonal to mode/audience/roles: Full
// (default) keeps chunk bytes on the nest; Metadata-only keeps them on the
// user's devices alone. IDs rule-A user-approved 2026-08-20.

/// `folder-nest-residency-select` — applies ON CHANGE like
/// `FolderConflictPolicyPicker`, EXCEPT picking Metadata-only ARMS `armed`
/// (owned by the caller — `FolderRowBody`'s shared `folder-residency-confirm`
/// alert) rather than writing. **While armed the select keeps painting the
/// CURRENT value**: the `Picker`'s binding always reads off `current` (the
/// live snapshot), never a locally-cached pick, so SwiftUI's next render
/// after tapping Metadata-only shows Full again until the confirm is
/// answered — no manual "put it back" needed (unlike GTK's immediately-
/// committed `DropDown`, linux's own `confirm_residency` doc explains this
/// exact contrast). Mirrors android's `ResidencySelect` (`FoldersScreen.kt`),
/// which repaints off `currentResidency` for the identical reason.
private struct FolderResidencySelect: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    @Binding var armed: Bool

    /// Fail-closed to Full, matching `residencyOptions()`'s own contract —
    /// never paint a raw/legacy value outside its own option set.
    private var current: String { normalizeResidency(value: folder.residency) }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(L.devices.folderResidency)
                    .font(.caption)
                Spacer()
                Picker("", selection: Binding(
                    get: { current },
                    set: { newVal in apply(newVal) }
                )) {
                    ForEach(residencyOptions(), id: \.value) { option in
                        Text(renderLocalizedText(option.label)).tag(option.value)
                    }
                }
                .labelsHidden()
                .controlSize(.small)
                .accessibilityIdentifier(Ids.folderNestResidencySelect)
                // Drives the same apply(...) the Picker does; the value is the
                // wire string. Env-gated no-op in production.
                .automationSelect(
                    Ids.folderNestResidencySelect,
                    value: { current },
                    set: { v in apply(v) }
                )
            }

            // Keyed on the SAME normalized value the select paints, so copy
            // and control can never disagree.
            Text(renderLocalizedText(residencyHint(current: current)))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// `"metadata_only"` arms the shared confirm rather than writing —
    /// answering it is the one place the flip is committed
    /// (`FolderRowBody`'s `.alert`). Every other pick (i.e. back to `"full"`)
    /// applies immediately, matching `FolderConflictPolicyPicker`.
    private func apply(_ value: String) {
        if value == RESIDENCY_METADATA_ONLY {
            armed = true
        } else {
            Task { await vm.setFolderResidency(name: folder.name, residency: value) }
        }
    }
}

/// The wire value `residencyOptions()` calls Metadata-only — `fauna_protocol
/// ::folders::RESIDENCY_METADATA_ONLY`'s value, not re-exported over UniFFI
/// (only the record shapes are), so pinned here once rather than repeating
/// the string literal at each call site.
private let RESIDENCY_METADATA_ONLY = "metadata_only"

// MARK: - Following a public folder

/// The **"Folders you follow"** section — the follow gesture plus the followed
/// rows (`ui/folders.md` § Following a public folder; behavior authority
/// `behavior/folders.md` § Publicly-synced follow).
///
/// **Three shapes the trickle-down must keep**, and each is a live trap:
///
/// 1. **The section is always offered**, even with no follows — the button is
///    how a user gets their first one, so gating it on a non-empty list makes it
///    unreachable.
/// 2. **A followed folder is a row in its OWN list, never a `folder-row`** — it
///    has no roster, no binding and no seat, so a `folder-row` would open an
///    expander full of controls that cannot apply to it.
/// 3. **The handle half reuses `recipient-picker-input`** — no second picker,
///    exactly as the share flow does (priority #2). The owner is accepted as a
///    handle *or* a bare 64-hex actor id, the same superset `share_set` takes,
///    and the classification is the shared recipe's, never this view's.
private struct FollowedFoldersSection: View {
    let vm: DevicesMachineVM

    @State private var formOpen = false
    @State private var ownerInput = ""
    @State private var nameInput = ""
    @State private var following = false

    private var followed: [FollowedFolderSummary] {
        vm.snapshot?.followed ?? []
    }

    var body: some View {
        Divider()
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(L.devices.followedFoldersSection)
                    .font(.headline)
                Spacer()
                Button(L.devices.followPublicFolder) { formOpen = true }
                    .accessibilityIdentifier(Ids.folderFollowButton)
                    // Env-gated no-op in production; opens the same form the
                    // Button does.
                    .automationActivate(Ids.folderFollowButton) { formOpen = true }
            }

            Text(L.devices.followPublicFolderHint)
                .font(.caption)
                .foregroundStyle(.secondary)

            if formOpen {
                followForm
            }

            ForEach(Array(followed.enumerated()), id: \.offset) { offset, follow in
                FollowedFolderRow(vm: vm, follow: follow)
                    // The row is the scope its children resolve under, mirroring
                    // `folder-row`, so `folder-followed-item[k]` addresses one
                    // row's own `folder-followed-status` / `folder-unfollow-button`.
                    .automationScope(Ids.folderFollowedItem, index: offset)
            }
        }
    }

    /// The follow flow — `optional_elements`, present only while open. The owner
    /// half is `recipient-picker-input` VERBATIM (shape 3 above).
    @ViewBuilder
    private var followForm: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField(L.conversations.unified.recipientPickerPlaceholder, text: $ownerInput)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.recipientPickerInput)
                .automationField(Ids.recipientPickerInput, text: $ownerInput)

            TextField(L.devices.followFolderName, text: $nameInput)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderFollowNameInput)
                .automationField(Ids.folderFollowNameInput, text: $nameInput)

            // Public names are world-readable by the ratified exception, so the
            // hint says to type it exactly as published rather than implying the
            // app can search for it — it cannot; the plane answers one folded
            // not-found for absent, private and misspelled alike.
            Text(L.devices.followFolderNameHint)
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack {
                Button(L.devices.followConfirm) { submitFollow() }
                    .disabled(!canSubmit)
                    .accessibilityIdentifier(Ids.folderFollowConfirm)
                    // `isEnabled` mirrors the `.disabled(...)` predicate so a
                    // synthesized activate cannot reach the write while the
                    // form is incomplete or a follow is already in flight.
                    .automationActivate(Ids.folderFollowConfirm, isEnabled: { canSubmit }) {
                        submitFollow()
                    }
                Button(L.common.cancel) { cancelFollow() }
            }
        }
        .padding(.leading, 8)
    }

    private var canSubmit: Bool {
        !following
            && !ownerInput.trimmingCharacters(in: .whitespaces).isEmpty
            && !nameInput.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The typed owner goes through UNRESOLVED — the shared recipe classifies it.
    /// The form closes only on success, so a not-found leaves the user's input in
    /// place to correct beside the error rather than making them retype it.
    private func submitFollow() {
        guard canSubmit else { return }
        following = true
        Task {
            let ok = await vm.followFolder(owner: ownerInput, folderName: nameInput)
            following = false
            if ok { cancelFollow() }
        }
    }

    private func cancelFollow() {
        formOpen = false
        ownerInput = ""
        nameInput = ""
    }
}

/// One `folder-followed-item` — name + owner + a *Public* provenance badge +
/// status, and the unfollow button. The owner is the one precomputed
/// `FollowedFolderSummary.ownerDisplay` (the verified handle, else the owner
/// id's short form — shared Rust decides, never re-derived here), painted as
/// *By ‹owner›* on the row's visible text AND its automation text, the one read
/// every app offers (`ui/folders.md` § Following a public folder). Read-only by construction: no roster, no binding, no
/// share section, no toggles.
///
/// `available == false` is the **REVOKE** — the owner flipped the audience back
/// or deleted the folder — and the row stays visible and loud until the user
/// removes it, because a re-flip resumes it under the same `folder_id`. It is
/// never a reason to drop the row.
private struct FollowedFolderRow: View {
    let vm: DevicesMachineVM
    let follow: FollowedFolderSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(follow.displayName)
                    .font(.caption)

                Text(ownerLabel)
                    .font(.caption2)
                    .foregroundStyle(.secondary)

                Text(L.devices.followedPublicBadge)
                    .font(.caption2)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(.quaternary, in: Capsule())

                automationText(
                    Ids.folderFollowedStatus,
                    follow.available
                        ? L.devices.followedStatusFollowing
                        : L.devices.followedStatusUnavailable
                )
                .font(.caption2)
                .foregroundStyle(follow.available ? Color.secondary : Color.red)

                Spacer()

                Button(L.devices.unfollowFolder, role: .destructive) { unfollow() }
                    .accessibilityIdentifier(Ids.folderUnfollowButton)
                    // Env-gated no-op in production; same removal the Button runs.
                    .automationActivate(Ids.folderUnfollowButton) { unfollow() }
            }

            if !follow.available {
                // Names BOTH causes (unshared / removed) because the follower
                // genuinely cannot tell them apart — the home nest folds them —
                // and the difference does not change what the user can do.
                Text(L.devices.followedUnavailableHint)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderFollowedItem)
        // Presence anchor for the indexed `folder-followed-item` — the same
        // scope-is-not-registration rule as `folder-place-row` above.
        .automationValue(Ids.folderFollowedItem, text: { "\(follow.displayName) \(ownerLabel)" })
    }

    private var ownerLabel: String { L.devices.followedOwner(owner: follow.ownerDisplay) }

    /// Addressed by the follow's pinned identity, never its display name — the
    /// owner may have renamed the folder since the last successful read.
    private func unfollow() {
        Task {
            await vm.unfollowFolder(
                homeNestUrl: follow.homeNestUrl, folderId: follow.folderId)
        }
    }
}

// MARK: - Audience and website serving (per folder)
//
// The folders re-model's phase-4 slice 4d (`ui/folders.md` § Audience and
// website serving; behavior authority `behavior/folders.md` § Target re-model).
// Both controls render on EVERY owner row — deliberately NOT gated on
// `folder.mode`, because a folder has no type any more and the website toggle
// is the only door to a website folder since phase 2 slice e retired the
// wizard's mode step; gating either behind the mode would re-create the very
// gap this slice closes.

/// `folder-audience-select` — who can read this folder. Applies ON CHANGE like
/// `FolderConflictPolicyPicker`, EXCEPT picking Public ARMS `armed` (owned by
/// the caller — `FolderRowBody`'s `folder-audience-public-confirm` alert)
/// rather than writing, exactly as `FolderResidencySelect` arms its own
/// confirm.
///
/// **While armed the select keeps painting the CURRENT audience**: the
/// `Picker`'s binding always reads off `current` (the live snapshot), never a
/// locally-cached pick, so SwiftUI's next render after tapping Public shows the
/// folder's real audience again until the confirm is answered. This is the trap
/// slice 4d cost two other legs — a browser `<select>` (web, measured
/// 2026-08-19) and a `gtk::DropDown` (linux, 2026-08-27) each commit the pick
/// the instant it is made and had to snap the widget back by hand, so the row
/// read *Public* while the folder was still private and nothing had been
/// written. apple inherits the tui shape here for the same reason the residency
/// twin beside it does, and `test_folder_audience_control.py
/// ::test_the_audience_control_arms_before_it_publishes` is what proves it.
private struct FolderAudienceSelect: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    @Binding var armed: Bool

    /// Group-bound-ness — entered through the share flow and nowhere else, so
    /// it is what decides which transitions the picker may offer at all.
    private var bound: Bool { folder.mlsGroupId != nil }

    /// NORMALIZED, never the raw column: the select's value has to be one of the
    /// options it offers, and an absent audience arrives as
    /// the empty string (`FolderSummary`'s default). Fail-closed in
    /// the one direction that matters — nothing unparseable ever paints as
    /// Public, because a binary that cannot read the column must not tell the
    /// user their folder is world-readable.
    private var current: String { normalizeAudience(value: folder.audience, bound: bound) }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(L.devices.folderAudience)
                    .font(.caption)
                Spacer()
                Picker("", selection: Binding(
                    get: { current },
                    set: { newVal in apply(newVal) }
                )) {
                    // The option set rides (bound, current), so it offers only
                    // what the nest would accept: an unbound folder gets
                    // private/public, a bound one gets `shared` (its own state)
                    // plus `public`, and a bound-and-public one gets the
                    // `shared` flip-back — the one legal exit from its public
                    // window. Offering an option the nest would refuse is the
                    // anti-pattern this list exists to prevent.
                    ForEach(audienceOptions(bound: bound, current: current), id: \.value) { option in
                        Text(renderLocalizedText(option.label)).tag(option.value)
                    }
                }
                .labelsHidden()
                .controlSize(.small)
                .accessibilityIdentifier(Ids.folderAudienceSelect)
                // Drives the same apply(...) the Picker does; the value is the
                // wire string. Env-gated no-op in production.
                .automationSelect(
                    Ids.folderAudienceSelect,
                    value: { current },
                    set: { v in apply(v) }
                )
                // NOT `.faunaGate`d, deliberately, exactly like the residency
                // and conflict-policy pickers beside it: every direction this
                // offers is a plain `fauna.folders.update` — keyless, the bound
                // `→shared` flip-back included, never the `FoldersAuthor`
                // orchestration the WebDAV and paywall siblings run — and that
                // kind is `OfflineSafe`, so a gate could never desensitize
                // anything (`offline-gate-check` rejects one that cannot).
            }

            // The hint rides the SAME (bound, current) inputs as the option set,
            // so the copy and the picker cannot disagree: a bound folder
            // explains that sharing is edited below, and a bound folder
            // currently public explains the one exit its picker does offer.
            Text(renderLocalizedText(audienceHint(bound: bound, current: current)))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// `"public"` arms the shared confirm rather than writing — answering it is
    /// the one place the flip is committed (`FolderRowBody`'s `.alert`). Every
    /// other pick applies immediately, matching `FolderConflictPolicyPicker`.
    ///
    /// Re-picking the folder's CURRENT audience is a no-op, never a write. That
    /// single guard is also what expresses `AudienceOption.selectable` as
    /// behaviour: a bound, not-public folder renders `shared` (it must be able
    /// to say what it is) but `shared` IS its current value there, so the pick
    /// cannot write — while the same option stays live as the flip-back once
    /// the folder is public.
    private func apply(_ value: String) {
        guard value != current else { return }
        if value == AUDIENCE_PUBLIC {
            armed = true
        } else {
            Task { await vm.setFolderAudience(name: folder.name, audience: value) }
        }
    }
}

/// The wire value `audienceOptions()` calls Public — `fauna_folders_machine
/// ::AUDIENCE_PUBLIC`'s value, not re-exported over UniFFI (only the record
/// shapes are), so pinned here once rather than repeating the string literal at
/// each call site. Mirrors `RESIDENCY_METADATA_ONLY` above.
private let AUDIENCE_PUBLIC = "public"

/// `folder-website-toggle` — publish this folder as the actor's website. The
/// structural sibling of `FolderWebdavToggle`, and the door phase 2 slice e
/// closed.
///
/// **It stays ENABLED on a folder that is neither public nor paywalled**, and
/// is merely inert there: the flag publishes the folder's HEAD, the audience
/// decides who may READ it. Disabling it would imply the setting is unavailable
/// and would strand the user with no way to prepare a site before publishing
/// it — so the control hints rather than disables.
private struct FolderWebsiteToggle: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var isOn: Bool { folder.websiteEnabled }

    private var bound: Bool { folder.mlsGroupId != nil }

    /// The shared TRI-state on the live serving picture. Publishing a site takes
    /// switches in TWO places — this toggle plus the actor's own web-address
    /// opt-in (`web-settings-subdomain-toggle`) — and a user who flipped only
    /// this half was told nothing while the nest served its info page in their
    /// site's place. `websiteAddressEnabled` is the best-effort second half
    /// (`fauna.web.get_subdomain_enabled`, `nil` on a failed
    /// read); UNKNOWN must never claim the site is live, which is why the hedge
    /// is its own state and not a two-state if/else.
    private var hint: String {
        renderLocalizedText(
            websiteServeHint(
                audience: normalizeAudience(value: folder.audience, bound: bound),
                paywalled: folder.webPaywallTier != nil,
                addressEnabled: vm.snapshot?.websiteAddressEnabled
            ))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Toggle(isOn: Binding(
                get: { isOn },
                set: { on in apply(on) }
            )) {
                Text(L.devices.serveWebsite)
            }
            .accessibilityIdentifier(Ids.folderWebsiteToggle)
            // One Entry carries the click AND the "on"/"off" read — the driver's
            // `get_attr(id, "state")` contract answers the two literals, which
            // three legs have now shipped a native spelling that read wrong.
            // Env-gated no-op in production.
            .automationActivate(
                Ids.folderWebsiteToggle,
                value: { isOn ? "on" : "off" }
            ) { apply(!isOn) }
            // Keyless like the audience above it — `set_website_enabled` is a
            // plain `fauna.folders.update`, NOT the `serve_set` orchestration
            // the WebDAV toggle below runs, which is why that toggle carries a
            // `.faunaGate` and this one deliberately does not: `folders.update`
            // is `OfflineSafe`, so a gate here could never desensitize anything.

            Text(hint)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func apply(_ enable: Bool) {
        Task { await vm.setFolderWebsiteEnabled(name: folder.name, enabled: enable) }
    }
}

// MARK: - Folder expander body (selective-sync paths + delete)

private struct FolderRowBody: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    /// Whether this set's local presence is a bound always-resident folder —
    /// the on-demand toggle then yields to the binding (arbitration surface).
    let isBoundToLocation: Bool
    /// FP-domain re-converge hook for the on-demand toggle (may be nil).
    let onDemandChanged: (() -> Void)?
    /// Collapse the row after a save/delete so the next interaction re-reads the
    /// refreshed snapshot (matches the cross-app "rows rebuilt collapsed"
    /// round-trip the e2e asserts).
    let collapse: () -> Void

    @State private var include: String = ""
    @State private var exclude: String = ""
    @State private var loaded = false
    @State private var showingDelete = false
    /// Arms `folder-residency-confirm` — shared with `FolderResidencySelect`
    /// below, which never writes `metadata_only` itself (folders re-model
    /// phase 5; file-sync.md § Content residency).
    @State private var residencyArmed = false
    /// Arms `folder-audience-public-confirm` — shared with
    /// `FolderAudienceSelect` below, which never writes `public` itself
    /// (folders re-model phase 4 slice 4d; ui/folders.md § Audience and website
    /// serving). A public folder rests UNSEALED, so the flip is consent-gated.
    @State private var audienceArmed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Who can read this folder (`folder-audience-select`) and whether
            // its head is published as the actor's website
            // (`folder-website-toggle`) — on EVERY owner row, never gated on
            // `folder.mode` (ui/folders.md § Audience and website serving).
            FolderAudienceSelect(vm: vm, folder: folder, armed: $audienceArmed)
            FolderWebsiteToggle(vm: vm, folder: folder)

            // Per-set, per-device Finder/Files on-demand presence
            // (`folder-on-demand-toggle`) — on EVERY owner row, place-less
            // included (turning it on is the enrol gesture; the mode gate
            // retired, ui/folders.md), apple platform_elements; a bound set
            // renders the arbitration caption instead (`on-demand-files.md`
            // § Apple File Provider binding).
            #if canImport(FileProvider)
                FolderOnDemandToggle(
                    vm: vm, folder: folder, isBound: isBoundToLocation,
                    onChanged: onDemandChanged)
            #endif

            // Per-set "serve over WebDAV" opt-in (`folder-webdav-toggle`) — every
            // OWNER row (a folder has no type; this expander body renders on no
            // `role == "member"` row). Sits in the expanded
            // per-set config, beside the other per-set knobs, matching linux's
            // `row.add_row(&webdav_row)`. webdav-server.md § Independent enablement pt 2.
            FolderWebdavToggle(vm: vm, folder: folder)

            // Per-set "paywall to tier" (`folder-paywall-tier-select`) — the
            // structural sibling of the webdav toggle above (folders.md § Web paywall; the linux shape).
            // Website-enabled rows only — keyed on the toggle, never on the
            // retired `mode = "web"` spelling.
            if folder.websiteEnabled {
                FolderPaywallTierPicker(vm: vm, folder: folder)
            }

            // The nest place's snapshot policy (`folder-nest-*`) — on EVERY
            // folder, not just a Backup-mode one: the mode is gone and "what the
            // nest keeps" is a property of the one place every folder has
            // (`backup-restore.md` § 8b).
            FolderNestPlaceEditor(vm: vm, folder: folder, collapse: collapse)

            // The nest place's own content residency (`folder-nest-residency-
            // select`) — on EVERY folder, its own `fauna.folders.update`
            // field, deliberately OUTSIDE the batched nest-place save above
            // (an older writer's policy edit must never silently clear it).
            // A sibling AFTER the save button, never inside it (mirrors
            // android's placement).
            FolderResidencySelect(vm: vm, folder: folder, armed: $residencyArmed)

            // The post-create device-place editor (`folder-place-row` + its
            // three flag checkboxes) — every enrolled seat's place, edited in
            // place through `fauna.folders.places.set` with a repaint from a
            // roster re-read (ui/folders.md § Implementation status today).
            FolderPlacesSection(vm: vm, folder: folder)

            Text(L.devices.selectiveSync)
                .font(.caption)
                .foregroundStyle(.secondary)

            TextField(L.devices.includePaths, text: $include)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderIncludePaths)
                // Writes/reads the same `$include` binding a keystroke would.
                // Env-gated no-op in production.
                .automationField(Ids.folderIncludePaths, text: $include)

            TextField(L.devices.excludePaths, text: $exclude)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.folderExcludePaths)
                .automationField(Ids.folderExcludePaths, text: $exclude)

            HStack {
                Button(L.devices.savePaths) {
                    savePaths()
                }
                .accessibilityIdentifier(Ids.folderSavePaths)
                // Same `savePaths()` the Button action runs. Env-gated no-op.
                .automationActivate(Ids.folderSavePaths) { savePaths() }

                Spacer()

                Button(L.devices.deleteFolder, role: .destructive) {
                    showingDelete = true
                }
                .accessibilityIdentifier(Ids.folderDeleteButton)
                // Same mutation the Button action performs (opens the confirm
                // alert). Env-gated no-op in production.
                .automationActivate(Ids.folderDeleteButton) { showingDelete = true }
            }
        }
        .padding(.vertical, 4)
        .onAppear {
            guard !loaded else { return }
            include = joinPathsField(paths: folder.includePaths)
            exclude = joinPathsField(paths: folder.excludePaths)
            loaded = true
        }
        .alert(L.devices.deleteConfirmTitle, isPresented: $showingDelete) {
            Button(L.common.cancel, role: .cancel) {}
            Button(L.devices.deleteFolder, role: .destructive) {
                confirmDelete()
            }
            .accessibilityIdentifier(Ids.folderDeleteConfirm)
            // Same mutation the alert's destructive Button runs, plus the
            // dismissal SwiftUI performs for a real press — see
            // `answerAlertViaAutomation`, which owns the ordering rule.
            // Env-gated no-op in production.
            .automationActivate(Ids.folderDeleteConfirm) {
                answerAlertViaAutomation($showingDelete) {
                    await vm.deleteFolder(name: folder.name)
                    collapse()
                }
            }
        } message: {
            Text(L.devices.deleteConfirmBody(name: folder.name))
        }
        // `folder-audience-public-confirm` — present only while armed. A
        // public folder rests UNSEALED, content and names/paths alike (they
        // become the address of each file), which `principles.md` § The user
        // always controls their data holds as the single exception to
        // sealed-at-rest — so answering this alert is the one place the flip is
        // committed. Both consequences are stated because each surprised
        // readers on its own: the names go public too, and flipping back
        // re-seals only FUTURE content.
        .alert(L.devices.declassifyTitle, isPresented: $audienceArmed) {
            Button(L.common.cancel, role: .cancel) {}
            Button(L.devices.declassifyConfirm, role: .destructive) {
                confirmPublicAudience()
            }
            .accessibilityIdentifier(Ids.folderAudiencePublicConfirm)
            // Same mutation the alert's destructive Button runs, plus the
            // dismissal SwiftUI performs for a real press — see
            // `answerAlertViaAutomation`, which owns the ordering rule.
            // Env-gated no-op.
            .automationActivate(Ids.folderAudiencePublicConfirm) {
                answerAlertViaAutomation($audienceArmed) {
                    await vm.setFolderAudience(name: folder.name, audience: "public")
                }
            }
        } message: {
            Text("\(L.devices.declassifyBody)\n\n\(L.devices.declassifyIrreversible)")
        }
        // `folder-residency-confirm` — present only while armed (the
        // `folder-audience-public-confirm` pattern, ui.yaml's own name for
        // this shape). Answering it is the one place the flip is committed.
        .alert(L.devices.residencyConfirmTitle, isPresented: $residencyArmed) {
            Button(L.common.cancel, role: .cancel) {}
            Button(L.devices.residencyConfirm, role: .destructive) {
                Task { await vm.setFolderResidency(name: folder.name, residency: "metadata_only") }
            }
            .accessibilityIdentifier(Ids.folderResidencyConfirm)
            // Same mutation the alert's destructive Button runs, plus the
            // dismissal SwiftUI performs for a real press — see
            // `answerAlertViaAutomation`, which owns the ordering rule.
            // Env-gated no-op.
            .automationActivate(Ids.folderResidencyConfirm) {
                answerAlertViaAutomation($residencyArmed) {
                    await vm.setFolderResidency(name: folder.name, residency: "metadata_only")
                }
            }
        } message: {
            Text(L.devices.residencyConfirmBody)
        }
        // ── Answering an alert through the automation seam ────────────────
        //
        // A real press of a SwiftUI alert Button does TWO things: it runs the
        // action, and SwiftUI itself sets the `isPresented` binding false.
        // `.automationActivate` reaches the action only, so until this pass the
        // seam left each of these alerts GENUINELY PRESENTED after the driver
        // answered it — measured 2026-09-21: the NEXT folder wizard never
        // opened, because a modal alert was still on screen. Hence the `armed = false` beside each activate
        // above: the seam must mirror the whole press, not half of it. Each
        // Task is dispatched FIRST so the mutation is already in flight when
        // the alert tears down — a reset that PRECEDED it lost the write, a
        // refuted fix recorded on the witness.
        //
        // Dismissing is necessary but not sufficient: an alert's content can
        // never de-register its automation ids either (SwiftUI fires
        // `.onAppear` for it but not `.onDisappear`, and the automation
        // sentinel rides in `.background(...)`, which an alert never realizes —
        // measured `VISIBLE(no-geo) geo=nil votes=-`). So the same bindings
        // also declare those ids' lifetime; `AutomationRegistry.hideAll`
        // carries the full rationale. Neither line is a product behaviour —
        // both are no-ops in production.
        .automationPresentation(ids: [Ids.folderDeleteConfirm], isPresented: showingDelete)
        .automationPresentation(
            ids: [Ids.folderAudiencePublicConfirm], isPresented: audienceArmed)
        .automationPresentation(
            ids: [Ids.folderResidencyConfirm], isPresented: residencyArmed)
    }

    private func savePaths() {
        // Shared parse (`parsePathsField`, `libs/fauna-folders-machine` via
        // fauna-ffi — `folders.md` § Where logic lives). An emptied field sends
        // `[]`, the canonical *clear the filter* (`fauna.folders.update` reads an
        // absent field as *leave unchanged*).
        Task {
            await vm.setFolderPaths(
                name: folder.name,
                include: parsePathsField(text: include),
                exclude: parsePathsField(text: exclude)
            )
            collapse()
        }
    }

    private func confirmDelete() {
        Task {
            await vm.deleteFolder(name: folder.name)
            collapse()
        }
    }

    /// Answer one of this row's alerts through the **automation seam**.
    ///
    /// A real press of a SwiftUI alert Button does two things: it runs the
    /// action, and SwiftUI itself clears the `isPresented` binding.
    /// `.automationActivate` reaches the action only, so the seam has to do
    /// both halves or the alert stays GENUINELY on screen — which is not just a
    /// stuck automation id: a live modal blocks the next sheet, and the tell
    /// was the following test's folder wizard never opening.
    ///
    /// **Order is load-bearing, and both orders have now been measured.**
    /// Dismissing FIRST (or synchronously alongside a detached `Task`) tears
    /// the alert down while the mutation is still queued and the write is LOST
    /// — the nest row never moves. So the dismissal waits for the action to
    /// complete, on the MainActor, which is the one ordering that leaves both
    /// the nest row and the id in the right state.
    ///
    /// Retiring the id is a SEPARATE fix and this is not it — an alert's
    /// content can never de-register itself whatever the binding does, which is
    /// what `.automationPresentation` below the alert chain is for.
    @MainActor
    private func answerAlertViaAutomation(
        _ armed: Binding<Bool>, _ action: @escaping () async -> Void
    ) {
        Task { @MainActor in
            await action()
            armed.wrappedValue = false
        }
    }

    /// Commit the declassify the select armed. Keyless — a plain
    /// `fauna.folders.update`; the back-catalogue is moved by each device's own
    /// engine at its next catch-up off the projected audience, never by the
    /// caller (ui/folders.md § Audience and website serving → *Where the logic
    /// lives*). The row is NOT collapsed: unlike a delete, the folder is still
    /// there and its select repaints off the refreshed snapshot.
    private func confirmPublicAudience() {
        Task { await vm.setFolderAudience(name: folder.name, audience: "public") }
    }
}

// MARK: - Device places (the post-create place editor)

/// The **post-create device-place editor** under an expanded owner-side
/// `folder-row`: one `folder-place-row` per enrolled device, each carrying the
/// three flag checkboxes `folder-place-originates` / `-accepts` /
/// `-applies-deletes` (folders re-model phase 2; the per-app record is
/// `ui/folders.md` § Implementation status today, and tui led it 2026-08-19).
///
/// **Swift derives nothing here.** The roster arrives already projected through
/// the one shared rule (`fauna_protocol::folders::place_rows`, applied at the
/// FFI boundary by `FfiFoldersClient::members_list`), so each seat's flag
/// triple and label are read as given — this view never re-derives them, and
/// never filters or re-sorts the roster, because **the roster order IS the e2e
/// address**: seat `j` is `folder-place-row[j]`.
///
/// Lazy-loaded on row expand, exactly like `FolderDeviceActivitySection` — this
/// section only exists while its row is expanded.
private struct FolderPlacesSection: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var places: [FfiFolderMember] {
        vm.folderPlaces[folder.name] ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.devices.folderPlacesTitle)
                .font(.caption)
                .foregroundStyle(.secondary)

            if places.isEmpty {
                Text(L.devices.noDevicesEnrolled)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(places.enumerated()), id: \.offset) { offset, place in
                    FolderPlaceRow(vm: vm, folderName: folder.name, place: place)
                        // Scope path for the indexed `folder-place-row` so the
                        // three flag ids resolve as its descendants (mirrors
                        // `folder-device-activity-item`).
                        .automationScope(Ids.folderPlaceRow, index: offset)
                }
            }
        }
        .padding(.leading, 8)
        .task(id: folder.name) {
            await vm.loadFolderPlaces(name: folder.name)
        }
        // A dropped push (offline, or the socket flapped) is recovered on the
        // next reconnect — the same treatment the device-activity section beside
        // it gets, for the same reason: the per-seat flags live off-snapshot.
        .onReconnect { await vm.loadFolderPlaces(name: folder.name) }
    }
}

/// One `folder-place-row` — a device's label plus its three flag checkboxes.
/// `.accessibilityElement(children: .contain)` keeps the child ids queryable
/// under the indexed container (the Section-clobbers-children rule).
private struct FolderPlaceRow: View {
    let vm: DevicesMachineVM
    let folderName: String
    let place: FfiFolderMember

    /// Which flag of a projected place row a checkbox paints, plus its element
    /// id and label. The flag *meanings* live once, in
    /// `fauna_protocol::folders::PlaceFlags` — this only picks the field
    /// (`FolderWizardSheetView`'s own `PlaceFlagBox` / linux's
    /// `PLACE_FLAG_BOXES` / android's `PlaceFlagBox`, same shape; the labels are
    /// the wizard's, deliberately, so the same flag reads identically wherever
    /// it is edited).
    private enum PlaceFlagBox: CaseIterable, Identifiable {
        case originates, accepts, appliesDeletes

        var id: String {
            switch self {
            case .originates: Ids.folderPlaceOriginates
            case .accepts: Ids.folderPlaceAccepts
            case .appliesDeletes: Ids.folderPlaceAppliesDeletes
            }
        }

        /// The label identity half — shared with `FolderWizardSheetView`'s
        /// twin via `PlaceFlagKind`.
        private var kind: PlaceFlagKind {
            switch self {
            case .originates: .originates
            case .accepts: .accepts
            case .appliesDeletes: .appliesDeletes
            }
        }

        var label: String { kind.label }

        func read(_ p: FfiFolderMember) -> Bool {
            switch self {
            case .originates: p.originates
            case .accepts: p.accepts
            case .appliesDeletes: p.appliesDeletes
            }
        }

        /// The seat's flag triple with this one flipped — the WHOLE point
        /// `set_folder_place` takes, so the two boxes the user did not touch
        /// cannot be dropped on the way to the wire (the shared `toggled`'s own
        /// contract; that helper lives in `fauna-protocol`, which carries no
        /// UniFFI dependency, so three bools cross instead of a `PlaceFlags` —
        /// `ui/folders.md` § Implementation status today records why).
        func flipped(_ p: FfiFolderMember) -> (Bool, Bool, Bool) {
            switch self {
            case .originates: (!p.originates, p.accepts, p.appliesDeletes)
            case .accepts: (p.originates, !p.accepts, p.appliesDeletes)
            case .appliesDeletes: (p.originates, p.accepts, !p.appliesDeletes)
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(place.label)
                .font(.caption)

            ForEach(PlaceFlagBox.allCases) { box in
                flagBox(box)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderPlaceRow)
        // Presence anchor for the indexed `folder-place-row` (one Entry per
        // seat, so `count("folder-place-row")` is the seat count).
        //
        // ⚠ The `.automationScope` on the parent is NOT enough: it only pushes
        // `(id, index)` onto the scope path its LEAVES record, leaving the
        // container itself unregistered — and `wait_for` reads the registry,
        // not the AX tree. Measured: `visible=False, count=0` while the row was
        // on screen. Every sibling indexed row here carries this same pair.
        .automationValue(Ids.folderPlaceRow, text: { place.label })
    }

    @ViewBuilder
    private func flagBox(_ box: PlaceFlagBox) -> some View {
        Button {
            write(box)
        } label: {
            HStack {
                Image(systemName: box.read(place) ? "checkmark.square.fill" : "square")
                Text(box.label)
            }
        }
        .buttonStyle(.plain)
        .padding(.leading, 12)
        .accessibilityIdentifier(box.id)
        // `get_attr(id, "state")` answers the literal `"on"` / `"off"` — the
        // cross-app contract three legs have now shipped a native spelling that
        // read wrong (web's DOM attribute could not see a checkbox's `checked`
        // property; linux's agent fell back to the widget's live state and
        // answered `"true"`/`"false"`). Same shape `folder-webdav-toggle` and
        // the wizard's own boxes carry, and driving it twice cannot undo the
        // caller's intent because the shared action reads the tick first.
        // Env-gated no-op in production.
        .automationActivate(box.id, value: { box.read(place) ? "on" : "off" }) {
            write(box)
        }
        // NOT `.faunaGate`d: `fauna.folders.places.set` is `OfflineSafe`, so a
        // gate could never desensitize anything and `offline-gate-check`
        // rejects one that cannot (the same reason the audience select and
        // website toggle above carry none).
    }

    /// Flip one box and write the WHOLE resulting point. The VM re-reads the
    /// roster afterwards — repaint from the NEST, never from the local flip:
    /// the per-seat flags are not on the page snapshot the write refreshed, so
    /// the re-read is the only thing that can answer, and a FAILED write
    /// re-reads back to the unchanged truth rather than leaving an optimistic
    /// box on screen.
    private func write(_ box: PlaceFlagBox) {
        let (originates, accepts, appliesDeletes) = box.flipped(place)
        Task {
            await vm.setFolderPlace(
                name: folderName,
                deviceId: place.deviceId,
                originates: originates,
                accepts: accepts,
                appliesDeletes: appliesDeletes
            )
        }
    }
}

// MARK: - Device activity (ordinary sync-mode change signal)

/// The **per-set device activity** section under an expanded owner-side
/// `folder-row`: one `folder-device-activity-item` per device that has
/// recorded a sync-mode change against this set (`fauna.folders.devices`,
/// distinct from the Backups page's `cachedSnapshotCount`/`cachedTotalBytes`,
/// which are backup-mode-only — file-sync.md § Implementation status today).
/// Lazy-loaded on row expand (this section only exists while its row is
/// expanded) and re-fetched only when a `fauna.sync.changed` push names THIS
/// exact set — never a blanket refetch for a collapsed row nobody's looking
/// at (mirrors tui's `device_activity_resync_op` / linux's
/// `folder_row_is_expanded` guard / windows' `_expandedFolder == folder`
/// guard).
private struct FolderDeviceActivitySection: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var activity: [DeviceInfo] {
        vm.folderDeviceActivity[folder.name] ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.devices.deviceActivity)
                .font(.caption)
                .foregroundStyle(.secondary)

            if activity.isEmpty {
                Text(L.devices.noDeviceActivity)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(activity.enumerated()), id: \.offset) { offset, device in
                    FolderDeviceActivityRow(device: device)
                        // Scope path for the indexed `folder-device-activity-item`
                        // (mirrors `folder-member-item`) so scoped child reads resolve.
                        .automationScope(Ids.folderDeviceActivityItem, index: offset)
                }
            }
        }
        .padding(.leading, 8)
        .task(id: folder.name) {
            await vm.loadFolderDeviceActivity(name: folder.name)
        }
        .onFolderDeviceActivityChanged { changedFolder in
            guard changedFolder == folder.name else { return }
            await vm.loadFolderDeviceActivity(name: folder.name)
        }
        // A dropped push (offline, or the socket flapped) is recovered on the
        // next reconnect — mirrors Media's own `.onReconnect` (transport.md
        // § Which surfaces a push invalidates — `media` covers this section's
        // per-set activity, part of the full reconnect sweep).
        .onReconnect { await vm.loadFolderDeviceActivity(name: folder.name) }
    }
}

/// One `folder-device-activity-item` — a device's label + its recorded
/// `change_count` for this set. `.accessibilityElement(children: .contain)`
/// keeps the child ids queryable under the indexed container (the memory'd
/// Section-clobbers-children rule).
private struct FolderDeviceActivityRow: View {
    let device: DeviceInfo

    var body: some View {
        HStack {
            automationText(Ids.folderDeviceActivityLabel, device.label)
                .font(.caption)
            Spacer()
            // Inline "Changes" caption — the peer of web's `<th>{col_changes}</th>`
            // column header, ridden per-row instead of a shared header (SwiftUI has
            // no table here either — mirrors linux's `build_device_activity_section`).
            Text(L.devices.colChanges)
                .font(.caption2)
                .foregroundStyle(.secondary)
            automationText(Ids.folderDeviceActivityCount, "\(device.changeCount)")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderDeviceActivityItem)
        // Presence anchor for the indexed `folder-device-activity-item` (one
        // Entry per device, so `count("folder-device-activity-item")` is the count).
        .automationValue(Ids.folderDeviceActivityItem, text: { device.label })
    }
}

// MARK: - Destination places (owner side)

/// Owner-side **Destination places** section on an expanded `folder-row`
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage) —
/// the apple leg of the six-app trickle-down, reusing
/// android's FFI face (`libs/fauna-ffi/src/backup_destinations.rs`, no new
/// bindgen owed). Renders one `folder-destination-row` per ATTACHED
/// destination (label + detach button), then — while at least one enrolled
/// destination remains unattached — the `folder-destination-attach-select` +
/// `folder-destination-attach-button` pair. Lazy-loaded on row expand
/// (mirrors `FolderDeviceActivitySection`); hidden entirely while the list is
/// empty — an affordance that cannot work must not paint, mirroring
/// linux/web/android's posture.
private struct FolderDestinationPlacesSection: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var places: [FfiFolderDestinationPlace] {
        vm.folderDestinations[folder.name] ?? []
    }
    private var attached: [FfiFolderDestinationPlace] { places.filter(\.attached) }
    private var attachable: [FfiFolderDestinationPlace] { places.filter { !$0.attached } }

    var body: some View {
        // `.task` sits on the OUTER VStack, which is ALWAYS present — never on
        // a view that is itself conditionally absent (`Group { if … }` with an
        // initially-false condition never fires its `.task` at all on macOS;
        // measured: `apple-task-id-on-conditionally-absent-view-never-fires`).
        // Only the CHILDREN inside are conditional, which lays out normally
        // (a childless VStack takes no space — no overlay/anchor hack needed).
        VStack(alignment: .leading, spacing: places.isEmpty ? 0 : 6) {
            if !places.isEmpty {
                Text(L.devices.folderDestinationsTitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                ForEach(Array(attached.enumerated()), id: \.offset) { offset, place in
                    FolderDestinationRow(vm: vm, folder: folder, place: place)
                        // Scope path for the indexed `folder-destination-row`
                        // (mirrors `folder-device-activity-item`) so scoped
                        // child reads resolve.
                        .automationScope(Ids.folderDestinationRow, index: offset)
                }

                if !attachable.isEmpty {
                    FolderDestinationAttachRow(vm: vm, folder: folder, attachable: attachable)
                }
            }
        }
        .padding(.leading, places.isEmpty ? 0 : 8)
        .task(id: folder.name) {
            await vm.loadFolderDestinations(name: folder.name, folderId: folder.id)
        }
    }
}

/// One `folder-destination-row` — an attached destination place: label +
/// `folder-destination-detach-button`. `.accessibilityElement(children:
/// .contain)` keeps the child id queryable under the indexed container (the
/// memory'd Section-clobbers-children rule).
private struct FolderDestinationRow: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    let place: FfiFolderDestinationPlace

    var body: some View {
        HStack {
            Text(place.label)
                .font(.caption)
            Spacer()
            Button(L.devices.folderDestinationDetach) { Task { await detach() } }
                .font(.caption)
                .foregroundStyle(.red)
                .buttonStyle(.plain)
                .accessibilityIdentifier(Ids.folderDestinationDetachButton)
                .automationActivate(Ids.folderDestinationDetachButton) { Task { await detach() } }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderDestinationRow)
        // Presence anchor for the indexed `folder-destination-row` (one Entry
        // per attached destination, so `count("folder-destination-row")` is
        // the attached count).
        .automationValue(Ids.folderDestinationRow, text: { place.label })
    }

    private func detach() async {
        // `folderSet` is always set on an attached row (the attach reply's own
        // read-back) — the detach sequence's config-row key, never re-derived here.
        await vm.detachFolderDestination(
            name: folder.name, folderId: folder.id,
            destinationId: place.destinationId, folderSet: place.folderSet ?? "")
    }
}

/// The attach select + button pair — rendered while at least one enrolled
/// destination remains unattached. `selected` falls back to the attachable
/// set's first entry whenever the staged pick is no longer in it (mirrors
/// android's `remember(attachable)` reseed): the moment a mutation
/// shrinks/reorders the set, the stale selection is dropped automatically.
private struct FolderDestinationAttachRow: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary
    let attachable: [FfiFolderDestinationPlace]

    @State private var selectedId: String = ""

    private var selected: String {
        attachable.contains(where: { $0.destinationId == selectedId })
            ? selectedId : (attachable.first?.destinationId ?? "")
    }

    var body: some View {
        HStack {
            Picker("", selection: Binding(
                get: { selected },
                set: { selectedId = $0 }
            )) {
                ForEach(attachable, id: \.destinationId) { place in
                    Text(place.label).tag(place.destinationId)
                }
            }
            .labelsHidden()
            .controlSize(.small)
            .accessibilityIdentifier(Ids.folderDestinationAttachSelect)
            .automationSelect(
                Ids.folderDestinationAttachSelect,
                value: { selected },
                set: { selectedId = $0 }
            )

            Button(L.devices.folderDestinationAttach) { Task { await attach() } }
                .font(.caption)
                .accessibilityIdentifier(Ids.folderDestinationAttachButton)
                .automationActivate(Ids.folderDestinationAttachButton) { Task { await attach() } }
        }
    }

    private func attach() async {
        let destinationId = selected
        guard !destinationId.isEmpty else { return }
        await vm.attachFolderDestination(name: folder.name, folderId: folder.id, destinationId: destinationId)
    }
}

// MARK: - Conflict surface (REVIEW LIST — auto-resolve model)

/// One reviewed conflict. Conflicts **auto-resolve on the detecting device** and the
/// losing version is always retained in version history, so this is a *review list*,
/// never a blocking chooser (`folders.md` § Conflicts; mechanism owner
/// `file-sync.md` § Conflicts, ratified 2026-07-10). It reuses the canonical trio with
/// **zero new conflict IDs** (user-approved): the badge shows the *resolution*, the
/// info line the path + winning head, and the single `conflict-resolve-button` is the
/// one-tap **"use the other version"** — which is exactly the § File Versions restore
/// re-point, and so is itself reversible.
///
/// The legacy per-candidate "keep this version" chooser (and the macOS
/// `conflict-resolution-panel` / `conflict-keep-*` surface) are **retired**: keep-both
/// is subsumed by versions.
private struct ConflictRow: View {
    let vm: DevicesMachineVM
    let conflict: ConflictSummary

    /// Auto-resolution stamped a winner. A row without one is an **unresolved**
    /// report (the engine's degraded path when an upload fails) — it renders informationally, with no button
    /// (the resolving device handles it; `fauna.sync.conflicts.resolve` stays
    /// served within the major version).
    private var isResolved: Bool { conflict.winningManifestHash != nil }

    /// The badge is the RESOLUTION on a resolved row ("Merged" / "Latest kept"),
    /// falling back to the (localized) conflict type on an unresolved row —
    /// the shared `conflictBadgeLabel` (`folders.md` § Where logic lives), which
    /// also localizes the `binary_copy`/`merge_markers` types this view used to
    /// pass through raw.
    private var badgeText: String {
        renderLocalizedText(conflictBadgeLabel(
            resolution: conflict.resolution,
            resolvedAt: conflict.resolvedAt,
            conflictType: conflict.conflictType))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(badgeText)
                    .font(.caption2)
                    .fontWeight(.semibold)
                    .textCase(.uppercase)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background((isResolved ? Color.green : Color.orange).opacity(0.15))
                    .foregroundStyle(isResolved ? Color.green : Color.orange)
                    .clipShape(Capsule())
                    .accessibilityIdentifier(Ids.conflictTypeBadge)
                    // Styled badge — keep Text + a read Entry (presence + value).
                    // Env-gated no-op in production.
                    .automationValue(Ids.conflictTypeBadge, text: { badgeText })
                Spacer()
            }

            // "{set}: {path} → {winner8}" once resolved; the bare "{set}: {path}"
            // before — precomputed on the shared snapshot (`ConflictSummary.fileInfo`,
            // `folders.md` § Where logic lives).
            Text(conflict.fileInfo)
                .font(.caption.monospaced())
                .accessibilityIdentifier(Ids.conflictFileInfo)
                // Read of the affected-file line. Env-gated no-op in production.
                .automationValue(Ids.conflictFileInfo, text: { conflict.fileInfo })

            if isResolved {
                // Re-pointing is only offered when a *different* retained version
                // actually exists to point at (precomputed `hasOtherVersion`).
                if conflict.hasOtherVersion {
                    // The ONE action: re-point at the retained other version.
                    Button(L.devices.conflicts.useOtherVersion) { useOtherVersion() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.conflictResolveButton)
                        // Same `useOtherVersion()` the Button action runs. Env-gated no-op.
                        .automationActivate(Ids.conflictResolveButton) { useOtherVersion() }
                }
            } else {
                // Unresolved row — informational only, never a chooser.
                Text(L.devices.conflicts.awaitingDevice)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }

    private func useOtherVersion() {
        Task { await vm.useOtherVersion(conflictId: conflict.id) }
    }
}
