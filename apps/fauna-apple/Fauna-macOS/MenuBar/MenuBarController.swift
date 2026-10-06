import AppKit
import SwiftUI
import FaunaKit

@Observable
class MenuBarController {
    private var statusItem: NSStatusItem?
    private var popover: NSPopover?
    private var pollTimer: Timer?
    var client: FaunaClient?
    var unreadCount: Int = 0
    var syncStatus: String = "Unknown"

    func setup() {
        let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "bird", accessibilityDescription: "Fauna")
            button.action = #selector(togglePopover(_:))
            button.target = self
        }

        let popover = NSPopover()
        popover.contentSize = NSSize(width: 320, height: 400)
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView: MenuBarView(controller: self))

        self.statusItem = statusItem
        self.popover = popover

        startSyncStatusPolling()
    }

    @objc func togglePopover(_ sender: AnyObject?) {
        guard let statusItem, let popover else { return }

        if popover.isShown {
            popover.performClose(sender)
        } else if let button = statusItem.button {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
        }
    }

    func updateUnreadCount(_ count: Int) {
        unreadCount = count
        if let button = statusItem?.button {
            if count > 0 {
                button.title = " \(count)"
            } else {
                button.title = ""
            }
        }
    }

    func updateConnectionStatus(_ connected: Bool) {
        guard let button = statusItem?.button else { return }
        if connected {
            updateSyncIcon(syncStatus == "Active")
        } else {
            button.image = NSImage(systemSymbolName: "bird.fill", accessibilityDescription: "Fauna (disconnected)")
        }
    }

    func openMainWindow() {
        NSApp.activate(ignoringOtherApps: true)
        popover?.performClose(nil)
    }

    // MARK: - Sync Status

    /// Update the menu bar icon to reflect sync activity.
    func updateSyncIcon(_ active: Bool) {
        guard let button = statusItem?.button else { return }
        if active {
            button.image = NSImage(systemSymbolName: "arrow.triangle.2.circlepath",
                                   accessibilityDescription: "Fauna (syncing)")
        } else {
            button.image = NSImage(systemSymbolName: "bird",
                                   accessibilityDescription: "Fauna")
        }
    }

    // The menu bar's Pause/Resume sync buttons retired with the B2 engine cutover.
    // They posted `.faunaSyncPauseRequested` / `.faunaSyncResumeRequested`, which
    // **no observer ever registered for** — the affordance had never worked. Nor is
    // it a mechanical rewire onto the engine host: `stopEngine` is *unbind* (it
    // forgets the folder binding and persists that), so wiring Pause to it would
    // silently delete the user's bindings. A real pause is a shared-engine
    // capability no client has today; if it's wanted, it belongs in
    // `fauna-sync-engine` and on all 7 apps, not as a macOS-only menu item.

    // MARK: - Polling

    /// Start a timer that polls sync status every 30 seconds.
    private func startSyncStatusPolling() {
        pollTimer = Timer.scheduledTimer(withTimeInterval: 30.0, repeats: true) { [weak self] _ in
            self?.pollSyncStatus()
        }
        // Initial poll
        pollSyncStatus()
    }

    /// Query the daemon/client for current sync status.
    private func pollSyncStatus() {
        guard let client else {
            syncStatus = "Stopped"
            updateSyncIcon(false)
            return
        }

        // Use the live WS-RPC connection state as a proxy for sync activity.
        // A full implementation would query the `fauna.sync.backup_status`
        // WS-RPC kind, but that needs a running nest. For now, derive status
        // from the shared `connection-status` state (`FaunaClient.connectionState`).
        Task { @MainActor in
            if client.connectionState == .connected {
                syncStatus = "Active"
                updateSyncIcon(true)
            } else {
                syncStatus = "Stopped"
                updateSyncIcon(false)
            }
        }
    }

    deinit {
        pollTimer?.invalidate()
    }
}
