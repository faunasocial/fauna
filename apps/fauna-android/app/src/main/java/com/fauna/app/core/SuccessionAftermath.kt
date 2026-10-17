package com.fauna.app.core

import com.fauna.ffi.FfiAftermathLeg
import com.fauna.ffi.FfiAftermathSink
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.launch
import uniffi.fauna_core.LocalizedText
import javax.inject.Inject
import javax.inject.Singleton

/**
 * android's driver state for the **post-succession aftermath**
 * (`docs/goal/behavior/succession-aftermath.md` § Re-key scope's `BackupKey`
 * corpus row — *"started at first successor sign-in, surfaced with progress,
 * resumed until complete"*) — apple's `SuccessionAftermath` + `FaunaClient`
 * `reload…` hooks, in one `@Singleton`.
 *
 * **This file composes; it does not sequence.** The order of the legs and which
 * of them are barriers live once in shared Rust
 * (`fauna_client_recovery::aftermath::run_succession_aftermath`, reached through
 * the `runSuccessionAftermath` FFI export; [ApiClient.runSuccessionAftermath] is
 * the accessor). Do not re-derive the ordering here.
 *
 * `@Singleton` so a view model that renders a surface the pass raises into can
 * subscribe to [configStageSettled] without owning the pass: the pass is
 * started once per authenticated start from the post-auth hook, and every live
 * consumer re-reads when its raises have settled.
 */
@Singleton
class SuccessionAftermathHost @Inject constructor(
    private val inheritedFilterMarks: InheritedFilterMarks,
    private val progressStore: AftermathProgressStore,
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val _configStageSettled = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    /** Emits once at the shared driver's ratified refresh point for the review
     *  surfaces (`succession-aftermath.md` § Propagation: "behind the
     *  aftermath's raise"). A subscriber re-reads its cached roster on each
     *  emission — never re-deriving it locally. The inherited filter marks are
     *  re-read by [onConfigStageSettled] itself, being one process-wide cache. */
    val configStageSettled: SharedFlow<Unit> = _configStageSettled.asSharedFlow()

    /** The fired-from-the-FFI-runtime half of the sink. Suspends on the marks
     *  re-read so a caller that awaits it sees the marks current. */
    internal suspend fun onConfigStageSettled() {
        inheritedFilterMarks.reload()
        _configStageSettled.emit(Unit)
    }

    /** Called on the FFI's tokio runtime: hand the re-reads to this host's own
     *  scope so the runtime never waits on them. */
    internal fun settledFromFfi() {
        scope.launch { onConfigStageSettled() }
    }

    /** The sink for one authenticated start; see [LoggingAftermathSink]. */
    internal fun sink(): FfiAftermathSink = LoggingAftermathSink(this, progressStore)
}

/**
 * android's `AftermathSink`: each leg's already-resolved line, logged and
 * parked in [AftermathProgressStore] for the six `recovery-kit-*-status` render
 * lines, and the settled hook forwarded to [SuccessionAftermathHost].
 *
 * ⚠ **The status line comes from the shared projection, never from a Kotlin
 * match on the leg.** The FFI boundary hands over an already-resolved
 * `LocalizedText?` rather than the progress value, and `null` is a real answer —
 * "this leg owes the user nothing" — not an absence. Re-mapping the leg app-side
 * is how apps grow different answers to one event.
 *
 * Both methods are called from the tokio runtime the export runs on and so must
 * not block; the settled hook hands its work to [SuccessionAftermathHost].
 */
internal class LoggingAftermathSink(
    private val host: SuccessionAftermathHost,
    private val progressStore: AftermathProgressStore,
) : FfiAftermathSink {

    override fun progress(leg: FfiAftermathLeg, line: LocalizedText?) {
        // Recorded either way: a leg settling into "nothing to report" must clear
        // a stale line from an earlier pass, not leave it painted.
        progressStore.record(leg, line)
        if (line == null) {
            ShellLog.d("fauna.recovery", "aftermath $leg: nothing to report")
        } else {
            ShellLog.i("fauna.recovery", "aftermath $leg: ${line.key}")
        }
    }

    override fun configStageSettled() {
        host.settledFromFfi()
    }
}
