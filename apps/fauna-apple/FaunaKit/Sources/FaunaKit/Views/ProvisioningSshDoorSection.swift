import SwiftUI

/// The text of `provisioning-ssh-door-command` (`onboarding-provisioning.md`
/// § 5): the machine's command (`ssh <domain>` where this device wrote the key
/// and its `Host` entry, else the `ssh -i` form), the one admitted address the
/// claim reply echoed — or the "address you set the server up from" wording
/// when it echoed none — the key's path where there is one, and the
/// fingerprint. Same template choice as linux's `nest_provisioning.rs` and tui.
public func sshDoorCommandText(_ door: SshDoorReadout) -> String {
    let t = L.onboarding.nestProvisioning.self
    let address = door.admittedFrom ?? t.sshDoorAddressUnknown
    if let path = door.keyPath {
        return t.sshDoorCommandTemplate(
            command: door.command, address: address, path: path, fingerprint: door.fingerprint)
    }
    return t.sshDoorCommandNoPathTemplate(
        command: door.command, address: address, fingerprint: door.fingerprint)
}

/// The SSH door on the provisioning page after a successful door run
/// (`onboarding-provisioning.md` § 5; `installers/vps.md` § The SSH door →
/// *The key*), shared by `MacNestProvisioningView` and `NestProvisioningView`.
///
/// - `provisioning-ssh-door-command`, on both.
/// - **iOS only** (ui.yaml scopes it to the apps with no `~/.ssh`):
///   `provisioning-ssh-door-save-key-button`, which takes the key from the
///   machine ONCE (`takeSshDoorPrivateKey`, which drops the machine's copy) and
///   hands `fauna-<domain>` over through the share sheet, then goes. The share
///   sheet reads a temporary file, deleted when the sheet closes, so the app
///   keeps no copy (android deletes its cache file when the page leaves).
///   macOS writes the key to `~/.ssh` during the run (`OnboardingVM.init`
///   passes the key dir; the app is not sandboxed) and renders no button.
///
/// The caller passes the readout it read in its own `body`, so a machine tick
/// re-renders this view.
public struct ProvisioningSshDoorSection: View {
    let machine: OnboardingMachine
    let door: SshDoorReadout
    @State private var keyHandedOver = false
    @State private var saveError: String?

    public init(machine: OnboardingMachine, door: SshDoorReadout) {
        self.machine = machine
        self.door = door
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(Ids.provisioningSshDoorCommand, sshDoorCommandText(door))
                .font(.callout.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            #if os(iOS)
            if !keyHandedOver {
                Button(L.onboarding.nestProvisioning.sshDoorSaveKeyButton) { saveKey() }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(Ids.provisioningSshDoorSaveKeyButton)
                    .automationActivate(Ids.provisioningSshDoorSaveKeyButton) { saveKey() }
            }
            if let saveError {
                Text(saveError).foregroundColor(.red)
            }
            #endif
        }
    }

    #if os(iOS)
    /// The one hand-over. Under e2e the file goes straight into
    /// `SnapshotFileSaver.e2eDownloadDir` (the driver's `download_dir()`), as
    /// every other iOS download does.
    private func saveKey() {
        guard let key = machine.takeSshDoorPrivateKey() else { return }
        keyHandedOver = true
        let name = "fauna-\(door.domain)"
        let data = Data(key.utf8)
        if let dir = SnapshotFileSaver.e2eDownloadDir {
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                try data.write(to: dir.appendingPathComponent(name))
            } catch {
                saveError = error.localizedDescription
            }
            return
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        do {
            try? FileManager.default.removeItem(at: url)
            try data.write(to: url, options: [.withoutOverwriting, .completeFileProtection])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            ShareSheet.present(items: [url]) {
                try? FileManager.default.removeItem(at: url)
            }
        } catch {
            try? FileManager.default.removeItem(at: url)
            saveError = error.localizedDescription
        }
    }
    #endif
}
