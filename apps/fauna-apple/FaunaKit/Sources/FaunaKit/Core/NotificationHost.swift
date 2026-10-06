import Foundation

/// Whether this process may touch `UNUserNotificationCenter` at all.
///
/// `UNUserNotificationCenter.current()` raises an **ObjC exception** — which
/// Swift cannot catch, so it takes the whole app down — when the running binary
/// is not a properly provisioned app bundle. That is not a hypothetical: the
/// bare `FaunaMacOS` binary `just mac-debug` produces is exactly such a build,
/// and it is the binary the macOS e2e driver launches.
///
/// One owner (priorities #2/#4): the same two-clause test had been re-spelled in
/// four places (`NotificationManager`, macOS's `BackupNotificationManager` /
/// `SyncNotificationManager` / `AppDelegate`) before `PushManager` needed a
/// fifth. Every notification call site — local *or* remote — reads this instead.
public enum NotificationHost {
    /// True only for a real `.app` bundle with a bundle identifier.
    public static var isAvailable: Bool {
        Bundle.main.bundleURL.pathExtension == "app"
            && Bundle.main.bundleIdentifier != nil
    }
}
