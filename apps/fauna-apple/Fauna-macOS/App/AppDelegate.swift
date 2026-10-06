import AppKit
import SwiftUI
import FaunaKit
import Sparkle
import UserNotifications

class AppDelegate: NSObject, NSApplicationDelegate {
    let menuBarController = MenuBarController()
    let updaterController: SPUStandardUpdaterController
    var appState: MacAppState?

    /// Set by `FaunaMacApp.completeAuthenticatedLaunch` — the APNs callbacks
    /// below have no other route to the session (`settings.md` § Push
    /// notifications). iOS's `AppDelegate` holds the same slot; only the
    /// delegate protocol differs (`NSApplicationDelegate` here,
    /// `UIApplicationDelegate` there).
    var pushManager: PushManager?

    override init() {
        // Don't start the updater in debug/E2E builds — there's no valid appcast URL yet,
        // so Sparkle would show an "Unable to check for updates" alert on every launch.
        #if DEBUG
        let shouldStartUpdater = false
        #else
        let shouldStartUpdater = !FaunaE2E.isActive
        #endif
        updaterController = SPUStandardUpdaterController(
            startingUpdater: shouldStartUpdater,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        menuBarController.setup()

        // `UNUserNotificationCenter.current()` raises an ObjC exception when the
        // bundle isn't properly provisioned (e.g. SPM debug builds) —
        // `NotificationHost` owns that test for every call site.
        if NotificationHost.isAvailable {
            UNUserNotificationCenter.current().delegate = self
        }

        // Restore dock icon preference
        NSApp.setActivationPolicy(DockIconPreference.current ? .regular : .accessory)

    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        // The real WS-RPC socket + resident engines are Rust-side worker threads the
        // process exit reclaims; the legacy second socket this used to close is gone
        // (`FaunaClient`, 2026-07-17). The one thing quit must NOT drop is the
        // engagement-cue rollup: the current dwell is un-drained and the sealed put
        // may still be inside its debounce window, so terminating now would lose the
        // session's cues (engagement-cues.md § At rest — put on batch or on
        // background/close). This is linux's `flush_cues_on_close(bounded: true)`
        // quit path, expressed in AppKit.
        //
        // All THREE drafts rails ride the same gate (`reserved-folders.md` §
        // The leave-flush promise — "leaving the app never loses the compose
        // input the debounced autosave has not yet caught"): `flushDraftsNow()`
        // itself no-ops fast when there is nothing pending (no attached sync,
        // or an unchanged snapshot), so folding all three in here costs nothing
        // on the common quiet-quit path. Window-close is deliberately NOT this
        // door on macOS — closing the window is never a quit here
        // (`test_sync_agent_survives_macos_window_close.py`, `macos.md` § Sync);
        // quitting is, which is why the witness drives a real terminate.
        let hasCueFlush = MainActor.assumeIsolated { CueViewportObserver.hasPendingFlush }
        let hasDraftRail = appState?.eventsVM != nil
            || appState?.conversationsVM != nil
            || appState?.feedVM != nil
        guard hasCueFlush || hasDraftRail else {
            return .terminateNow
        }
        Task { @MainActor in
            await CueViewportObserver.flushForAppLifecycle()
            await appState?.eventsVM?.flushDraftsNow()
            await appState?.conversationsVM?.flushDraftsNow()
            await appState?.feedVM?.flushDraftsNow()
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        // Bound it — quit must never hang on a put that cannot complete (an
        // unreachable nest). Same 2.5 s ceiling linux bounds its blocking flush at;
        // whichever reply lands first wins, and a dropped put leaves the rollup
        // dirty so the next session retries it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    // MARK: - APNs (settings.md § Push notifications; common.md § Push
    // Notifications → Transports)
    //
    // macOS's half of the `apns` transport. Same two callbacks iOS implements in
    // `FaunaApp.swift`, on the AppKit protocol instead of UIKit's — and they
    // hand off to the very same shared `PushManager`, which turns a device token
    // into a `fauna.push.subscribe` carrying this device's P-256 public key.

    func application(_ application: NSApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        MainActor.assumeIsolated {
            pushManager?.didRegisterForRemoteNotifications(deviceToken: deviceToken)
        }
    }

    func application(_ application: NSApplication,
                     didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // Not swallowed: the failure is the user's only witness that push is not
        // working, and until the Developer-portal Push capability grants this
        // app an `aps-environment` entitlement, it is the expected outcome.
        logMessage(level: .warn, target: "fauna.app",
                   message: "[push] APNs registration failed: \(error.localizedDescription)")
        MainActor.assumeIsolated {
            pushManager?.didFailToRegisterForRemoteNotifications(error: error)
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        // Counted whatever the flag said — the counter's contract is "the raise
        // arrived and was handled", not which arm ran (see `ReopensHandled`).
        MainActor.assumeIsolated {
            ReopensHandled.recordHandled()
        }
        if !flag {
            NSApp.activate(ignoringOtherApps: true)
        }
        return true
    }
}

extension AppDelegate: UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        if response.actionIdentifier == BackupNotificationManager.viewBackupAction
            || response.actionIdentifier == UNNotificationDefaultActionIdentifier
        {
            let content = response.notification.request.content
            if content.categoryIdentifier == BackupNotificationManager.categoryIdentifier {
                Task { @MainActor in
                    appState?.selectedSidebar = .backups
                    NSApp.activate(ignoringOtherApps: true)
                }
            } else if content.categoryIdentifier == NotificationManager.knockCategoryIdentifier,
                      let senderId = content.userInfo[NotificationManager.knockSenderIdKey] as? String
            {
                // Same destination `MacNotificationsView.open`'s `.knock` arm sends a
                // tapped notification-feed row to — the OS toast is a second door
                // into the identical `pendingKnockSenderId` hand-off.
                Task { @MainActor in
                    appState?.pendingKnockSenderId = senderId
                    appState?.selectedSidebar = .contacts
                    NSApp.activate(ignoringOtherApps: true)
                }
            }
        }
        completionHandler()
    }

    // Show notifications even when app is in foreground
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}
