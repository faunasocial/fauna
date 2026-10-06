#if os(iOS)
import Foundation
import BackgroundTasks
import os

public class BackgroundScheduler: NSObject {
    static let uploadTaskId = AppleIdentifiers.BackgroundTask.upload
    static let custodianPullTaskId = AppleIdentifiers.BackgroundTask.custodianPull
    static let widgetRefreshTaskId = AppleIdentifiers.BackgroundTask.widgetRefresh
    static let bgSessionId = AppleIdentifiers.BackgroundTask.backgroundUploadSession

    private var photoBackupEngine: PhotoBackupEngine?
    private var custodianEngine: CustodianBackupEngine?
    private var widgetRefresh: (@MainActor () async -> Int?)?

    /// The widget-refresh pass counters (`fauna_e2e_agent::WIDGET_REFRESH_KEY`):
    /// bumped by ``runWidgetRefreshPass()`` and nothing else, so a pass they
    /// count is one the widget's scheduled entry point ran — never the
    /// foreground receive loop's, which ticks the same observer.
    @MainActor public private(set) var widgetRefreshPassesStarted = 0
    @MainActor public private(set) var widgetRefreshPassesCompleted = 0
    /// The unread total the last completed pass left behind — the count the
    /// widget shows — or `nil` when that pass had no live session to poll
    /// (not configured yet, or pre-auth).
    @MainActor public private(set) var widgetRefreshLastPassCount: Int?

    public func configure(photoBackupEngine: PhotoBackupEngine) {
        self.photoBackupEngine = photoBackupEngine
    }

    public func configure(custodianEngine: CustodianBackupEngine) {
        self.custodianEngine = custodianEngine
    }

    /// What the widget refresh runs: one conversations receive pass, whose
    /// ingest republishes the widget's count (`ConversationsVM.receivePass`),
    /// answering the list's unread total after it (`nil`: no session to poll).
    /// Set once the conversations session is active; until then a wake is a
    /// no-op that still resubmits.
    public func configure(widgetRefresh: @escaping @MainActor () async -> Int?) {
        self.widgetRefresh = widgetRefresh
    }

    public func registerTasks() {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.uploadTaskId, using: nil) { task in
            self.handleUploadTask(task as! BGProcessingTask)
        }

        BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.custodianPullTaskId, using: nil) { task in
            self.handleCustodianPullTask(task as! BGProcessingTask)
        }

        BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.widgetRefreshTaskId, using: nil) { task in
            self.handleWidgetRefreshTask(task as! BGAppRefreshTask)
        }
    }

    private static let log = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "social.fauna", category: "background-scheduler"
    )

    /// Submit `request`, logging a refusal instead of discarding it. The
    /// system refuses a request whose kind's `UIBackgroundModes` entry is not
    /// declared (`BGTaskSchedulerErrorCodeNotPermitted`; the Info.plist pins
    /// the modes, `test_apple_identifier_pins.py`) or that it cannot take for
    /// any other reason — with the error swallowed a task that was never
    /// scheduled looked exactly like one waiting for the OS to wake it.
    private func submit(_ request: BGTaskRequest) {
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            let code = (error as NSError).code
            Self.log.error("BGTaskScheduler refused \(request.identifier, privacy: .public) (code \(code)): \(String(describing: error), privacy: .public)")
        }
    }

    /// Submit a `BGProcessingTaskRequest` for `identifier` — the shape both
    /// `schedule*` methods below share (upload/custodian-pull differ only in
    /// which task id they resubmit).
    private func scheduleBackgroundTask(identifier: String) {
        let request = BGProcessingTaskRequest(identifier: identifier)
        request.requiresNetworkConnectivity = true
        request.requiresExternalPower = false
        submit(request)
    }

    public func scheduleUpload() {
        scheduleBackgroundTask(identifier: Self.uploadTaskId)
    }

    /// Enqueue the custodian's next periodic pull pass. The 15-min cadence
    /// `ui/backups.md` § Scheduling settles for every destination tuple is the
    /// OS-scheduler's own minimum granularity for a `BGProcessingTaskRequest`,
    /// so nothing here re-chooses it — `BGTaskScheduler` picks the actual wake
    /// time opportunistically at or after that floor, exactly as android's
    /// `WorkManager` does.
    public func scheduleCustodianPull() {
        scheduleBackgroundTask(identifier: Self.custodianPullTaskId)
    }

    /// Enqueue the home-screen widget's next background count refresh — a
    /// `BGAppRefreshTask` (the short, network-only kind a widget refresh is),
    /// no earlier than android's 15-minute `WidgetDataWorker` period; the OS
    /// picks the actual wake time at or after it, as `WorkManager` does.
    public func scheduleWidgetRefresh() {
        let request = BGAppRefreshTaskRequest(identifier: Self.widgetRefreshTaskId)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        submit(request)
    }

    /// The widget refresh while the app is suspended (`apps/common.md`
    /// § Home-screen widget — "current in the background without the app being
    /// opened"): resubmit, then one receive pass (``runWidgetRefreshPass()``),
    /// whose ingest ticks the conversations observer that publishes the count —
    /// the same path a foreground arrival takes.
    func handleWidgetRefreshTask(_ task: BGAppRefreshTask) {
        scheduleWidgetRefresh()
        let inner = Task { @MainActor in
            await self.runWidgetRefreshPass()
            task.setTaskCompleted(success: true)
        }
        task.expirationHandler = {
            inner.cancel()
        }
    }

    /// The widget refresh's **work**, lifted out of the handler above for the
    /// reason ``runScheduledUploadPass()`` is: `BGAppRefreshTask` has no public
    /// initializer, so this is the only way to drive the production scheduled
    /// path without the OS.
    ///
    /// It is what `fauna_e2e_agent::WIDGET_REFRESH_SCHEDULED_PASS_NOW` pokes —
    /// convention 14's `run_now` for a schedule the OS alone owns — so the witness
    /// of "the count keeps itself current in the background" on iOS drives the
    /// same method the OS does. Not a user-reachable knob: the automation surface
    /// is compiled out of release artifacts (convention 15).
    @MainActor
    public func runWidgetRefreshPass() async {
        widgetRefreshPassesStarted += 1
        widgetRefreshLastPassCount = await widgetRefresh?()
        widgetRefreshPassesCompleted += 1
    }

    /// Runs `work` inside a cancellable `Task`, marking `task` succeeded/failed
    /// accordingly, wires `task.expirationHandler` to cancel it, then calls
    /// `reschedule` — the shape both `BGProcessingTask` handlers below share
    /// (upload/custodian-pull differ only in what `work` does and which schedule
    /// method re-submits).
    private func runBackgroundTask(
        _ task: BGProcessingTask,
        reschedule: @escaping () -> Void,
        work: @escaping () async throws -> Void
    ) {
        let inner = Task {
            do {
                try await work()
                task.setTaskCompleted(success: true)
            } catch {
                task.setTaskCompleted(success: false)
            }
        }

        task.expirationHandler = {
            inner.cancel()
        }

        reschedule()
    }

    /// The `social.fauna.sync.upload` slice — **the photo-backup leg only** since
    /// the slice-5 flip 2026-08-15.
    ///
    /// This task previously drove a mail-segment backup pass first (a
    /// construct-run-drop `FfiBackupCoordinator` over `run_all_tuples`) and photo
    /// backup second, sharing one task id. The mail leg is gone: the **source
    /// nest** is the segment-backup writer and backs enrolled owners up with no
    /// client awake (`backup-restore.md` § Background Tasks → *Flip status (slice
    /// 5)*).
    ///
    /// ⚠ **The task id itself stays registered, submitted and rescheduled.** It
    /// is shared with photo backup, so this is a *narrowing* of what the handler
    /// does — never a cancel of the id. (android's arm did cancel its retired
    /// schedule by name, which was right there and would be wrong here:
    /// `WorkManager` persists unique periodic work across an upgrade under a
    /// name that was the mail worker's alone.)
    func handleUploadTask(_ task: BGProcessingTask) {
        runBackgroundTask(task, reschedule: scheduleUpload) {
            try await self.runScheduledUploadPass()
        }
    }

    /// This slice's **work**, lifted out of the handler above so the production
    /// scheduled path can be driven without a `BGProcessingTask` — a type with no
    /// public initializer, handed out by the OS scheduler and nothing else.
    ///
    /// It is what `fauna_e2e_agent::PHOTO_BACKUP_SCHEDULED_PASS_NOW` pokes:
    /// convention 14's `run_now` for the one photo-backup trigger iOS gives no
    /// period control over (`ui/folders.md` § Photo backup — "iOS grants no
    /// period control, so the OS schedules"). The poke and the handler drive the
    /// *same* method, so a witness of "new photos go up by themselves" cannot be
    /// satisfied by a path no production edge takes. It is emphatically **not** a
    /// user-reachable knob: the automation surface is compiled out of release
    /// artifacts (convention 15) and nothing in the app UI reaches here.
    public func runScheduledUploadPass() async throws {
        _ = try await photoBackupEngine?.syncNewPhotos()
    }

    /// The client-device backup custodian's periodic pull pass
    /// (`docs/goal/behavior/backup-destinations.md` § Third destination
    /// kind) — it drives `CustodianBackupEngine`, built through
    /// `APIClient.custodianHost`, not the file-sync engine host.
    ///
    /// There is no file-sync pull task beside it: the `social.fauna.sync.pull`
    /// slice ran a one-shot pass over the in-process location bindings, and no
    /// app binds a location in-process any more (`sync-engine-deployments.md`
    /// § Apple apps — convergence design) — iOS reads remote files on demand
    /// through Media and the Files-app File Provider.
    func handleCustodianPullTask(_ task: BGProcessingTask) {
        runBackgroundTask(task, reschedule: scheduleCustodianPull) {
            try await self.custodianEngine?.runPass()
        }
    }

    /// Background URLSession for chunk uploads that survive app suspension.
    lazy var backgroundSession: URLSession = {
        let config = URLSessionConfiguration.background(withIdentifier: Self.bgSessionId)
        config.isDiscretionary = true
        config.sessionSendsLaunchEvents = true
        return URLSession(configuration: config, delegate: self, delegateQueue: nil)
    }()
}

extension BackgroundScheduler: URLSessionDelegate, URLSessionTaskDelegate {
    public func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        // Handle background upload completion
    }
}
#endif
