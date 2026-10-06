import Foundation
import Testing
import FaunaKit
@testable import FaunaMacOSLib

// Pins the Swift half of the per-file completed-sync notification seam: the
// `FfiSyncCompleteObserver` implementation forwards each basename to the
// injected `SyncCompleteNotifying` surface. The mechanism below it — the
// Synced filter and the self-healing socket loop, including a fake event
// pushed over a real `unix_transport::serve` socket — is pinned in Rust
// (`fauna_ipc::events` tests + the `fauna-ffi` observer-forwarding test), so
// this test injects a recording notifier and never touches
// `UNUserNotificationCenter` (testing.md § point 10).

private final class RecordingNotifier: SyncCompleteNotifying, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [String] = []

    var filenames: [String] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func notifySyncComplete(filename: String) {
        lock.lock()
        stored.append(filename)
        lock.unlock()
    }
}

@Test func syncCompleteObserverForwardsTheBasenameToTheNotifier() {
    let notifier = RecordingNotifier()
    let observer = SyncCompleteEventObserver(notifier: notifier)
    observer.onSyncComplete(filename: "report.bin")
    observer.onSyncComplete(filename: "photo.jpg")
    #expect(notifier.filenames == ["report.bin", "photo.jpg"])
}
