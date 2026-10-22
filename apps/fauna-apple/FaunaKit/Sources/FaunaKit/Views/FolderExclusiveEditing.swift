import FaunaFFISwift
import SwiftUI

/// Exclusive editing — the owner's "one device at a time may edit this folder"
/// switch and the lease line every row of a governed folder carries
/// (`ui/folders.md` § Exclusive editing; `file-sync.md` § Exclusive editing owns
/// the lease). Shared by macOS and iOS; the logic lives in shared Rust
/// (`DevicesMachine.setFolderExclusiveEditing`, `devicesFolderLeaseStatus`).

/// `folder-exclusive-editing-toggle` — on the expanded OWNER row body, default
/// off. **Non-optimistic**: it paints the nest's `FolderSummary.exclusiveEditing`,
/// which the machine's post-write refresh repaints, so a refused write (the error
/// rides `errorMessage` as `devices.error_set_exclusive_editing`) never leaves it
/// showing a state the folder does not have. Keyless — its own `folders.update`
/// field, the shape of `FolderWebsiteToggle`.
struct FolderExclusiveEditingToggle: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    private var isOn: Bool { folder.exclusiveEditing }

    var body: some View {
        Toggle(isOn: Binding(get: { isOn }, set: { apply($0) })) {
            Text(L.devices.folderExclusiveEditing)
        }
        .accessibilityIdentifier(Ids.folderExclusiveEditingToggle)
        // One Entry carries the click AND the "on"/"off" read (the
        // folder-webdav-toggle template), which `BackupsActions.
        // exclusive_editing_state` reads as `checked`. Env-gated no-op in
        // production.
        .automationActivate(
            Ids.folderExclusiveEditingToggle,
            value: { isOn ? "on" : "off" }
        ) { apply(!isOn) }
    }

    private func apply(_ on: Bool) {
        Task { await vm.setFolderExclusiveEditing(name: folder.name, on: on) }
    }
}

/// `folder-lease-status` — on the row header of an owner AND a member row, present
/// only while the folder is governed. The text is resolved once in shared Rust
/// (`devicesFolderLeaseStatus`): never the hex id, never the lease acquire kind,
/// and "this device" is the roster row `device-this-mark-badge` marks
/// (`vm.thisDeviceRow`). Read off the snapshot on every render — never probed,
/// since asking for the lease takes it.
struct FolderLeaseStatusLine: View {
    let vm: DevicesMachineVM
    let folder: FolderSummary

    var body: some View {
        if let status = devicesFolderLeaseStatus(
            folder: folder, devices: vm.snapshot?.devices ?? [], thisDeviceRow: vm.thisDeviceRow)
        {
            automationText(Ids.folderLeaseStatus, renderLocalizedText(status))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
