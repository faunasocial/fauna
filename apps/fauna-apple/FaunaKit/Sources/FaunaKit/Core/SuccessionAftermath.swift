import Foundation

/// apple's driver for the **post-succession aftermath**
/// (`docs/goal/behavior/succession-aftermath.md` § Re-key scope's `BackupKey`
/// corpus row — *"started at first successor sign-in, surfaced with progress,
/// resumed until complete"*).
///
/// **What this fixes.** A succession re-points corpus *ownership* on the nest,
/// but every blob the predecessor sealed is still sealed under the predecessor's
/// `BackupKey`. Until the aftermath re-keys them, the successor's own launch path
/// reads its inherited corpus as *"wrong key or tampered data"* — drafts come up
/// empty, the MLS replica refuses to load and the client stays single-device. The
/// corpus is **stuck, not corrupt**: the material to open it is in the account
/// registry the whole time, and this pass is what uses it.
///
/// **This file composes; it does not sequence.** The order the legs run in, and
/// which of them are barriers, lives once in shared Rust
/// (`fauna_client_recovery::aftermath::run_succession_aftermath`, reached here
/// through the `runSuccessionAftermath` FFI export). tui and web drive the same
/// function. ⚠ Do not re-derive the ordering in Swift — that is exactly the shape
/// the shared driver exists to remove.
///
/// **One leg deliberately runs elsewhere on every app**, so its absence here is
/// the design: leg 5 (the file-corpus re-seal) is the sync agent's, in no app
/// process.
///
/// **Leg 3 (the `__mls` re-seal) is a partial exception**, not an absence: it
/// is still a barrier inside `MlsStateSync::load`, built separately by
/// `fauna-ffi`'s `mls_sync_launch.rs` rather than sequenced from this file —
/// but as of  it both receives real predecessor keys
/// and reports through the same `FfiAftermathSink` this file registers here,
/// so `AftermathProgress.mlsReseal` renders like every other leg. Must not be
/// forked into a Swift-local fix — the wiring is shared by every FFI app.
enum SuccessionAftermath {

    /// Run the aftermath for the session that just authenticated.
    ///
    /// **Called on every authenticated start, not only after a ceremony** — the
    /// pass is resumable by design and returns `notASuccessor` having done
    /// nothing for an identity that never succeeded, which is the overwhelmingly
    /// common case. Gating on "did we just succeed" would be wrong as well as
    /// unnecessary: a re-seal interrupted by a lost connection is finished by the
    /// *next* sign-in, and that sign-in has no ceremony to notice.
    ///
    /// Fire-and-forget and best-effort, in the shape `refreshMailEpochSchedule`
    /// and `runSealBackfill` already use: the account is already the successor's,
    /// and refusing a session over a pass that retries would be strictly worse
    /// than a plane that is briefly still owed.
    ///
    /// No ceremony context is passed in: the raise context the ceremony leaves
    /// (the sweep's unattested-member roster, the nest's succession stamp) is
    /// parked durably in the account registry by shared Rust and drained by the
    /// pass itself, so every sign-in runs this identically.
    ///
    /// `onConfigStageSettled` fires at the shared driver's ratified refresh
    /// point for the member-review surfaces (`succession-aftermath.md`
    /// § Propagation: "behind the aftermath's raise") — the caller re-reads
    /// its cached roster there, never re-deriving it locally.
    static func run(
        api: APIClient,
        onConfigStageSettled: @escaping @Sendable () -> Void = {},
        onProgress: @escaping @Sendable (FfiAftermathLeg, LocalizedText?) -> Void = { _, _ in }
    ) {
        Task {
            let outcome = await logTryAsync(.warn, "fauna.recovery", "successionAftermath") {
                try await api.runSuccessionAftermath(
                    onConfigStageSettled: onConfigStageSettled, onProgress: onProgress)
            }
            guard let outcome else { return }
            // Logged unconditionally, including `notASuccessor`. ⚠ The web
            // client's own history is the reason: a pass that reports a healthy
            // "nothing to do" is indistinguishable from one whose predecessor
            // LINK is missing, and that ambiguity cost a full 30-minute
            // diagnostic cycle there before the line was added. apple records
            // the succession link inside the FFI boundary
            // (`fauna-ffi`'s `recovery.rs` step 4), so the trap is already
            // closed here — but the line is what makes that checkable rather
            // than assumed.
            logMessage(
                level: .info, target: "fauna.recovery",
                message: "succession aftermath: \(outcome)")
        }
    }
}

/// apple's `AftermathSink`: each leg's already-localized line, logged AND
/// forwarded to whoever renders it.
///
/// **The seven `recovery-kit-*-status` lines now paint on both apple shells**
/// (`docs/goal/ui/settings.md` § Recovery kit → *The post-succession
/// aftermath's progress lines*) — `RecoveryKitSection`
/// reads `FaunaClient.aftermathProgress`, which `onProgress` below feeds.
/// Logging stays: it is what keeps the pass diagnosable from a real run's app
/// log even off-screen, which is how row 181 found the original gap.
///
/// ⚠ **The status line comes from the shared projection, never from a Swift
/// match on the leg.** Four apps each mapping the progress enum themselves is
/// four chances to say something different about one event (priority #1/#3) —
/// which is why the FFI boundary hands over an already-resolved
/// `LocalizedText?` rather than the progress value; this sink renders it
/// verbatim and never re-derives it.
final class LoggingAftermathSink: FfiAftermathSink, @unchecked Sendable {
    // `@unchecked` follows `LaunchdSyncAgentSpawner`'s precedent: both stored
    // properties are `@Sendable` closures, so there is still nothing for the
    // FFI's tokio runtime and the app to race over.

    private let onConfigStageSettled: @Sendable () -> Void
    private let onProgress: @Sendable (FfiAftermathLeg, LocalizedText?) -> Void

    init(
        onConfigStageSettled: @escaping @Sendable () -> Void = {},
        onProgress: @escaping @Sendable (FfiAftermathLeg, LocalizedText?) -> Void = { _, _ in }
    ) {
        self.onConfigStageSettled = onConfigStageSettled
        self.onProgress = onProgress
    }

    func progress(leg: FfiAftermathLeg, line: LocalizedText?) {
        // `nil` is a real value, not an absence: a leg whose outcome owes the
        // user nothing (`NothingConfigured`, `AlreadyEnrolled`, …) reports no
        // line at all, and the render hides it. Nothing to say is not an error
        // and must not read as one in the log either. Forwarded either way —
        // a leg settling into "nothing to report" must clear a stale line from
        // an earlier pass, not leave it painted.
        guard let line else {
            logMessage(
                level: .debug, target: "fauna.recovery",
                message: "aftermath \(leg): nothing to report")
            onProgress(leg, nil)
            return
        }
        logMessage(
            level: .info, target: "fauna.recovery",
            message: "aftermath \(leg): \(line.key)")
        onProgress(leg, line)
    }

    func configStageSettled() {
        // The hook an app that renders the member-review / inherited-filter
        // surfaces re-reads them at (`succession-aftermath.md` § Propagation).
        // apple re-reads both off this pass: the member-review roster and the
        // inherited-filter marks (`FaunaClient.reloadInheritedFilterMarks`,
        // whose count is the Account section's inherited-filters line).
        onConfigStageSettled()
    }
}

/// The post-succession aftermath's per-leg progress, as `FaunaClient` holds
/// it — apple's twin of web's module `aftermathProgress` store and linux's
/// `AftermathProgress` (`docs/goal/ui/settings.md` § Recovery kit → *The
/// post-succession aftermath's progress lines*). One field per leg of the
/// ordered sequence, fed by `LoggingAftermathSink.progress` as each leg
/// reports; read by `RecoveryKitSection`.
///
/// `nil` is a real value, not an absence: a leg whose outcome owes the user
/// nothing reports `nil` from the shared projection, and the render hides
/// that line — never a Swift-side decision (priority #1/#3).
public struct AftermathProgress {
    /// Leg 2 — the `NestBackupKey` re-grant.
    public var backupRegrant: LocalizedText?
    /// Leg 3 — the `__mls` re-seal. Fed by `FaunaClient.recordAftermathProgress`'s
    /// `.mlsReseal` case, which `fauna-ffi`'s `mls_sync_launcher` reports into
    /// through the same `FfiAftermathSink` this session registers for the
    /// other five legs  — the launcher is built
    /// separately (`APIClient.conversationsSession`), so this line may arrive
    /// before, with, or after the rest of a given pass, and can also arrive on
    /// its own with no ceremony this session ran (a resumed reseal from an
    /// earlier sign-in).
    public var mlsReseal: LocalizedText?
    /// Leg 4 — the capability-grant re-mint.
    public var grantRemint: LocalizedText?
    /// Leg 5 — the file corpus. Reports from the sync agent's own process, in
    /// no FFI app's sink; **always `nil`** here for the same structural
    /// reason as `mlsReseal` above.
    public var corpusReseal: LocalizedText?
    /// Leg 7 — the `__drafts` re-seal. Rendered ABOVE leg 6, like tui's.
    public var draftsReseal: LocalizedText?
    /// Leg 6 — the mail burn, the only leg that takes something away.
    public var mailBurn: LocalizedText?
}
