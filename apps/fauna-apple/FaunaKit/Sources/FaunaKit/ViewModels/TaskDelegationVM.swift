import SwiftUI

/// Shared view-model for the "Task delegation" Settings sub-page (macOS + iOS,
/// one FaunaKit VM) — the per-task-kind runner + assignment surface
/// (`docs/goal/behavior/participants.md` § Task delegation). A thin proxy over
/// the shared `FfiTaskDelegationView`: the shared layer composes the user's
/// pins (`fauna.state.delegation`) with the live advisory lease
/// (`fauna.delegation.observe`) into rows and orchestrates the pin write
/// through the plane's read-modify-write; this VM holds the latest rows
/// as `@Observable` state and re-`load()`s after every `configure` /
/// `setAssignment`. No delegation policy here — the lift is render-only
/// (priority #1/#2). Reference render: linux
/// `apps/fauna-linux/src/settings/task_delegation.rs`.
@MainActor @Observable
public final class TaskDelegationVM {
    /// Latest rows, in `LIVE_TASK_KINDS` order. Empty until `configure`.
    public private(set) var rows: [FfiTaskDelegationRow] = []
    /// `device_id` hex → display label, resolved from the shared Devices
    /// roster — a runner / pinned participant's name is inherently client-side
    /// state the shared view-model deliberately does not bake in
    /// (`RunnerStatus.Other` / `PinOption.Other` carry only the ref).
    public private(set) var deviceLabels: [String: String] = [:]
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public private(set) var isLoading = false

    private var view: FfiTaskDelegationView?
    private var api: APIClient?

    public init() {}

    /// What THIS apple build declares it runs (`participants.md` § The assignment
    /// picker) — the one place the macOS/iOS line is drawn, so the picker and any
    /// test that checks the picker's honesty read the same source.
    ///
    /// - **macOS `.indexOnly`:** runs the content-index builder, drives **no**
    ///   `backup-upload` — its in-app upload driver was deleted at the slice-5
    ///   flip 2026-08-15 and the source nest is the writer
    ///   (`backup-restore.md` § Background Tasks → *Flip status (slice 5)*).
    ///   Retracting the declaration alongside the driver is load-bearing: leaving
    ///   it offers a self-pin nothing on the box can honour, which is the
    ///   stranding linux shipped in 2026-07.
    /// - **iOS `.viewerOnly`:** always battery-mobile, runs no heavy kind; it
    ///   queries the synced index without building it.
    public static var forThisBuild: FfiHeavyTaskCapability {
        #if os(macOS)
        .indexOnly
        #else
        .viewerOnly
        #endif
    }

    /// Build the surface and load the first snapshot. Re-runs on every page
    /// visit (called from `.task(id: reloadToken)`) — the runner column is
    /// **live** advisory-lease state (a peer claims the lease; a desktop
    /// unplugs and yields), so a build-once hydrate would go stale
    /// (participants.md § Task delegation).
    public func configure(
        api: APIClient, deviceId: String, capability: FfiHeavyTaskCapability
    ) async {
        self.api = api
        do {
            view = try await api.taskDelegationView(deviceId: deviceId, capability: capability)
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await reload()
    }

    /// Re-read the rows (post-configure, or after a pin write).
    public func reload() async {
        guard let view else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            rows = try await view.load()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
        await loadDeviceLabels()
    }

    /// Persist a pin change, then reload so the runner column + picker reflect
    /// authoritative state. On failure the error shows and the rows are left
    /// as-is (the write is a CAS read-modify-write — a failure changed
    /// nothing).
    public func setAssignment(taskKind: String, option: FfiPinOption) async {
        guard let view else { return }
        do {
            try await view.setAssignment(taskKind: taskKind, option: option)
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await reload()
    }

    /// The `device_id` hex → label map from the shared Devices roster, for
    /// naming a runner / pinned device. Any read failure yields an empty map —
    /// callers then fall back to a short-hex abbreviation rather than showing
    /// nothing. Mirrors linux's `device_labels`.
    private func loadDeviceLabels() async {
        guard let api else { return }
        do {
            let machine = try await api.devicesMachine(observer: NoopTaskDelegationDevicesObserver())
            await machine.refresh()
            deviceLabels = Dictionary(
                uniqueKeysWithValues: machine.snapshot().devices.map { ($0.deviceId, $0.label) })
        } catch {
            deviceLabels = [:]
        }
    }
}

/// No-op `DevicesObserver`: this VM reads the roster once per reload (for
/// display names) rather than reacting to it, so it needs no reactivity —
/// mirrors linux's `NoopObserver`.
private final class NoopTaskDelegationDevicesObserver: DevicesObserver, @unchecked Sendable {
    func onChanged() {}
}
