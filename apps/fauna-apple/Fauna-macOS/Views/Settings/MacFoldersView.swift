import SwiftUI
import FaunaKit

/// macOS **Settings → Folders** shell — a thin wrapper around the shared FaunaKit
/// `FoldersContent` (folder list + per-set config + conflicts +
/// create wizard + the Apple "Photo Library" photo-backup controls), driven by the
/// shared-Rust `DevicesMachine`. The desktop-only local-folder binding (`folder-location-*`,
/// `platform_elements`) is injected per set via `MacFolderBindingSection`, nested
/// under each folder row.
///
/// 2026-06-28 sync/folder UI unification: replaces the former `Settings → Sync`
/// page (`SyncSettingsView`). The free-text set-name binding field
/// (`folder-location-fileset-input`) and the per-row set-name display
/// (`folder-location-fileset`) are **gone** — a set is named once at create, and a
/// bound folder lives under its set (D3 / O-1, folders.md). The former
/// `SyncSettingsView` self-managed folder list is dropped in favour of the
/// machine-driven shared surface; the daemon LaunchAgent start/stop control is
/// retired from Settings. macOS sync runs **in-process on the shared
/// `fauna-sync-engine`** (`FaunaClient.syncHost`): binding a folder here starts a
/// resident engine for that set immediately, and unbinding stops it.
///
/// `reloadToken` is `appState.navGeneration` (the same reload-on-becoming-visible
/// contract `DevicesView` uses).
struct MacFoldersView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(LocationsModel.self) private var locations: LocationsModel?
    /// The session's one VM (app-scene-level, shared with Settings → Devices).
    @Environment(DevicesMachineVM.self) private var vm
    let reloadToken: Int

    var body: some View {
        let locations = self.locations
        FoldersContent(
            vm: vm,
            // Bound sets yield the on-demand toggle to their binding (the
            // one-local-presence arbitration surface).
            boundSets: { Set((locations?.mappings ?? []).map(\.folderId)) },
            // A writer's shared set the agent has parked (owner revoked the
            // write grant) — gates `folder-access-revoked-warning`.
            revokedSets: { locations?.revokedFolders ?? [] },
            // A toggle flip re-converges the FP domains through the same hook a
            // bind/unbind uses (FaunaMacApp wires it to the launch reconcile).
            onDemandChanged: { Task { await locations?.onBindingsChanged?() } }
        ) { folder in
            MacFolderBindingSection(folder: folder)
        }
        .pageTitle(L.folders.title)
        .task(id: reloadToken) {
            if let client {
                await vm.configure(api: client.api)
            }
        }
    }
}

/// The desktop local-folder binding for ONE folder (`folder-location-*`), nested
/// under its row on the Folders page. Each bound folder is a device-local
/// `LocationBinding` from `LocationsModel`, filtered to this set — the set is
/// contextual, so the binding records only the **folder path** (no free-text set
/// name; D3 / O-1, folders.md § Binding). macOS is always-resident, so there is
/// no per-row `folder-location-mode-toggle` (windows-only, until a File Provider host
/// lands).
struct MacFolderBindingSection: View {
    let folder: FolderSummary
    @Environment(LocationsModel.self) private var locations: LocationsModel?
    /// The page-level VM (already in the environment from `MacFoldersView`'s
    /// ancestor) — the only route a no-ref bind refusal has to the shared
    /// page `error-message`, since this binding is `LocationsModel`-driven,
    /// not a `DevicesMachine` gesture.
    @Environment(DevicesMachineVM.self) private var vm: DevicesMachineVM

    @State private var newLocationPath = ""

    private var mappings: [LocationBinding] {
        (locations?.mappings ?? []).filter { $0.folder == folder.name }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // `folder-location-list` is the section's presence sentinel — a leaf Text,
            // NOT an ancestor of the rows (a container id would clobber each row's
            // `folder-location-row`; apple-section-accessibilityid-clobbers-children).
            automationText(Ids.folderLocationList, L.folders.syncedLocations)
                .font(.caption)
                .foregroundStyle(.secondary)

            if mappings.isEmpty {
                Text(L.folders.noLocationsBound)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(mappings.enumerated()), id: \.element.id) { offset, mapping in
                    bindingRow(mapping)
                        .automationScope(Ids.folderLocationRow, index: offset)
                }
            }

            // Add a local folder to THIS set — path only (the set is contextual).
            HStack {
                TextField(L.folders.locationPathPlaceholder, text: $newLocationPath)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.folderLocationPathInput)
                    .automationField(Ids.folderLocationPathInput, text: $newLocationPath)
                Button(L.folders.choose) { chooseFolder() }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.folderLocationBrowseButton)
                Button(L.folders.bindLocation) { bindLocation() }
                    .controlSize(.small)
                    .disabled(newLocationPath.isEmpty)
                    .accessibilityIdentifier(Ids.folderLocationAddButton)
                    .automationActivate(Ids.folderLocationAddButton,
                                        isEnabled: { !newLocationPath.isEmpty }) {
                        bindLocation()
                    }
            }
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private func bindingRow(_ mapping: LocationBinding) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                automationText(Ids.folderLocationPath, mapping.path)
                    .font(.caption)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer()
                Button(role: .destructive) {
                    locations?.remove(path: mapping.path)
                } label: {
                    Image(systemName: "trash").foregroundStyle(.red)
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.folderLocationRemoveButton)
                .automationActivate(Ids.folderLocationRemoveButton) {
                    locations?.remove(path: mapping.path)
                }
            }

            // The mass-delete floor's confirm affordance
            // (`delete-propagation.md` § A wholesale-vanished folder is
            // infrastructure failure). Every tracked file in this folder
            // vanished at once — an unmounted volume, a folder moved away —
            // so the engine recorded NOTHING and the nest still holds the
            // set. Rendered ONLY while the hold is non-zero: `0` is the ONLY
            // reading that ever retracts it (the hold is derived per
            // reconcile pass, never stored), and a standing offer to destroy
            // files over a healthy folder is worse than no affordance at all.
            if let held = locations?.deletesHeld[mapping.folder], held > 0 {
                let count = String(held)
                automationText(Ids.folderLocationDeletesHeld, L.folders.deletesHeld(count: count))
                    .font(.caption)
                    .foregroundStyle(.orange)

                Button(L.folders.applyDeletes(count: count)) {
                    // The SET, never `held` above: the agent re-derives what
                    // is actually missing at click time, so a confirm racing
                    // a remount deletes nothing.
                    Task { await locations?.applyHeldDeletes(folder: mapping.folder) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.folderLocationApplyDeletesButton)
                .automationActivate(Ids.folderLocationApplyDeletesButton) {
                    Task { await locations?.applyHeldDeletes(folder: mapping.folder) }
                }
            }

            // The delete rail's unreadable-path line (`delete-propagation.md`
            // § Unreadable is not absent): part of the folder could not be
            // read, so nothing was changed and that subtree stopped syncing.
            // Deliberately a bare status line — there is nothing to confirm,
            // and an apply verb here would be the bug; the remedy
            // (permissions, the mount) is outside the app. Keyed off its own
            // count, never the hold above, so it renders (and the apply
            // button does not) for an unreadable-only set.
            if let unreadable = locations?.deletesSkippedUnreadable[mapping.folder], unreadable > 0 {
                automationText(Ids.folderLocationUnreadable, L.folders.unreadable(count: String(unreadable)))
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.folderLocationRow)
        // Registry read so the in-process driver can count/locate each row; value
        // = the row's folder path.
        .automationValue(Ids.folderLocationRow, text: { mapping.path })
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = L.folders.choose
        guard panel.runModal() == .OK, let url = panel.url else { return }
        newLocationPath = url.path
    }

    private func bindLocation() {
        let path = newLocationPath
        guard !path.isEmpty else { return }
        // The set already exists (created once via the wizard) — binding only
        // records the device-local path under this contextual set. `folderRef`
        // is the binding's key — own row vs. a same-named set owned by someone
        // else (`on-demand-files.md` § Hosting multiple on-demand folders). A row
        // that yields none is refused, never bound by name (fail closed),
        // surfaced on the page's `error-message` like tui/linux/windows.
        guard let folderId = folder.folderRef else {
            logMessage(level: .warn, target: "fauna.sync",
                       message: "refusing to bind \(folder.name): its folder id could not be resolved")
            vm.setBindLocationError(
                L.devices.errorBindLocation(message: "the folder's identity could not be resolved"))
            return
        }
        vm.setBindLocationError(nil)
        locations?.add(path: path, folder: folder.name, folderId: folderId)
        newLocationPath = ""
    }
}
