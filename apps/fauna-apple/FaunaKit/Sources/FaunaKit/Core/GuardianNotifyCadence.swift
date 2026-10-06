import Foundation

/// Native **Guardian Notify** counter + flush cadence (macOS + iOS, shared
/// FaunaKit — priority #2). `family-safety.md` § Guardian Notify — the
/// Slice-D-client leg: the ward's client counts its
/// own guardian-floor render-enforcement events per category, deduped per
/// item per local day, and reports them to `fauna.family.notify_report`
/// batched at most hourly (eager first report).
///
/// A thin driver around the shared [`FfiNotifyAccumulator`] UniFFI object,
/// which owns the whole state machine (dedup set, pending counts, the batch
/// interval gate) — this type supplies only what shared Rust cannot: the
/// clock, the device's UTC offset ([`DeviceOffset`]), the periodic due-check
/// loop, and the live `APIClient` to send the report through. The apple twin
/// of android's `@Singleton FamilyNotifyStore` / windows'
/// `GuardianNotifyCadence` / linux's `content_policy::note_enforcement` — but
/// unlike those three (all built before the shared `NotifyAccumulator` state
/// machine landed, 2026-08-11), this leg hand-rolls none of the dedup/batch
/// logic itself.
///
/// Same singleton-cadence shape as [`DnsAutoRenewCadence`]: `start(api:)` is
/// idempotent (a latched loop), `stop()` disarms it, and both are driven from
/// [`ActorScope`] so an outgoing identity's pending counts and dedup set never
/// leak into the incoming one.
@MainActor
public final class GuardianNotifyCadence {
    public static let shared = GuardianNotifyCadence()

    /// How often the loop wakes to check whether a report is due — cheap
    /// (a local field read, no RPC), so this only bounds the latency of the
    /// first report after the ward flags something. The real cadence gate is
    /// the accumulator's own `notifyReportMinIntervalSecs` interval, matching
    /// android's `CHECK_INTERVAL_MS`.
    private static let checkIntervalSeconds: UInt64 = 5

    private let accumulator = FfiNotifyAccumulator()
    private var loop: Task<Void, Never>?
    private var api: APIClient?

    /// Whether a loop is currently armed — exists so `ActorScopeTests` can pin
    /// that a teardown actually disarms this cadence, mirroring
    /// `DnsAutoRenewCadence.isRunning`.
    var isRunning: Bool { loop != nil }

    /// How many times `start` has actually armed a loop — mirrors
    /// `DnsAutoRenewCadence.armCount`: `isRunning` alone cannot witness the
    /// latch below (it reads `true` whether `start` rebound or was
    /// swallowed), so this is what lets a test pin that a second `start`
    /// without an intervening `stop` does NOT re-arm — the defect class that
    /// would otherwise bind this cadence to an outgoing `APIClient` for good.
    private(set) var armCount = 0

    private init() {}

    /// Idempotent — starts the due-check loop once per launched session. Call
    /// alongside `DnsAutoRenewCadence.shared.start(api:)` at the same
    /// post-login trigger.
    public func start(api: APIClient) {
        guard loop == nil else { return }
        armCount += 1
        self.api = api
        loop = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: Self.checkIntervalSeconds * 1_000_000_000)
                if Task.isCancelled { return }
                await self?.flushIfDue()
            }
        }
    }

    public func stop() {
        loop?.cancel()
        loop = nil
        api = nil
        accumulator.reset()
    }

    /// Set whether the guardian's `content_notify` knob is on — call from
    /// `ContentPolicyStore.refresh` on every `fauna.family.status` read (the
    /// same post-auth + reconnect trigger as the content-policy cache),
    /// alongside the `contentNotify` field it now also hydrates.
    public func setEnabled(_ on: Bool) {
        accumulator.setEnabled(on: on)
    }

    /// Record any guardian-floor render-enforcement on `itemId` (a feed post
    /// or DM message) — call from every social render site alongside
    /// `ContentPolicyInputs.verdictFor`, passing the SAME `labels` and the
    /// guardian floor (`ContentPolicyInputs.contentPolicy` — never the
    /// viewer's own thresholds, which never enter here). A no-op unless
    /// Notify is on and the guardian floor bites (`FfiNotifyAccumulator`'s own
    /// gate), so a render site can call this unconditionally.
    public func record(itemId: String, labels: [ContentLabelEntry], contentPolicy: FfiContentPolicy?) {
        accumulator.record(
            itemId: itemId,
            labels: labels,
            contentPolicy: contentPolicy,
            nowSecs: Self.nowSecs(),
            offsetMinutes: DeviceOffset.utcOffsetMinutes())
    }

    /// Drain the batched report if due and fire it — best-effort,
    /// fire-and-forget (a modified client under-reports, never over-reports —
    /// `family-safety.md` § Guardian Notify trust bound); the periodic loop
    /// ignores the return. `public`, not `private`: the
    /// `family_notify_check_now` TestAgent command (convention 14's run_now
    /// poke) calls this directly from each app shell to force a due-check
    /// synchronously instead of waiting on `checkIntervalSeconds`, mirroring
    /// android's `flushIfDue()`, and surfaces a non-nil return loudly
    /// (convention 11 — the production path stays silent, only the test
    /// command inspects this).
    ///
    /// ⚠ `accumulator.takeDue` is DESTRUCTIVE (drains `pending` on success) —
    /// checked BEFORE `api` so a nil `api` never silently discards a real
    /// batch: `api` missing is reported as its own reason instead of being
    /// folded into the same guard as `takeDue`.
    @discardableResult
    public func flushIfDue() async -> String? {
        guard let due = accumulator.takeDue(nowSecs: Self.nowSecs()) else { return "nothing due" }
        guard let api else { return "batch due but no live api to send it through" }
        do {
            try await api.familyClient().notifyReport(
                entries: due.entries, utcOffsetMinutes: due.offsetMinutes)
            return nil
        } catch {
            return "notifyReport failed: \(error)"
        }
    }

    private static func nowSecs() -> Int64 {
        Int64(Date().timeIntervalSince1970)
    }
}
