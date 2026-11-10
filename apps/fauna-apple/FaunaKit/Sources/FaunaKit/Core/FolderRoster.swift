import Foundation

/// The session's folder roster, weakly — the Settings pending-actions row's way
/// to the name of the folder an automatic prune targets (`docs/goal/ui/settings.md`
/// § Pending actions). `DevicesMachineVM` publishes the machine it builds;
/// ``folderName(id:)`` asks that machine's shared `DevicesMachine::folder_name`
/// (label custody applied), so this holds no resolver of its own and makes no
/// fetch. The twin of android's `FolderRoster`, linux's
/// `views::devices_folders::current_folder_name` and web's `sessionFolderName`;
/// one implementation for macOS and iOS.
///
/// Weak so a machine released on sign-out or an account switch
/// (`DevicesMachineVM.reset`) goes with it. `nil` before a Devices or Folders
/// page has built one, or for a folder the roster cannot name: the row then
/// paints the neutral sentence.
public enum FolderRoster {
    private static let lock = NSLock()
    private static weak var machine: DevicesMachine?

    /// Record the machine the Devices/Folders page just built.
    public static func publish(_ built: DevicesMachine) {
        lock.lock(); defer { lock.unlock() }
        machine = built
    }

    /// The name of the folder whose id is `id`, or `nil`.
    public static func folderName(id: String) -> String? {
        lock.lock()
        let current = machine
        lock.unlock()
        return current?.folderName(id: id)
    }
}
