package com.fauna.app.ui.util

import android.content.Context
import com.fauna.app.R
import uniffi.fauna_client_mail_settings.TransferTiming
import uniffi.fauna_client_mail_settings.transferTiming

/**
 * The time half of the two mail wizards' summaries: the elapsed time and time
 * left the progress step appends (`mail-export.md` § Wizard steps step 4,
 * `mailbox-migration.md` § Wizard steps step 5) and the time the Done card
 * reports (steps 5 / 6). The computation is the shared Rust
 * `fauna_client_mail_settings::transfer_timing` (priority #2); this only reads
 * the session's clock through it and renders the numbers with
 * [ValueFormat.durationSecs] and the generated templates, once for the export
 * and import screens.
 */
object MailTransferText {
    /**
     * The shared timing of one session at the device's clock. [processed] counts
     * every message dealt with (done, skipped and failed alike).
     */
    fun timing(
        startedAt: Long,
        lastProgressAt: Long,
        running: Boolean,
        processed: ULong,
        total: ULong,
        nowSecs: Long = System.currentTimeMillis() / 1000,
    ): TransferTiming = transferTiming(startedAt, lastProgressAt, running, processed, total, nowSecs)

    /** [summary] plus the elapsed time and, while the run is moving, the time left. */
    fun progress(
        context: Context,
        summary: String,
        timing: TransferTiming,
        elapsedRes: Int,
        remainingRes: Int,
    ): String {
        val elapsed = timing.elapsedSecs ?: return summary
        val remaining = timing.remainingSecs
        return if (remaining != null) {
            context.getStringFmt(
                remainingRes, summary,
                ValueFormat.durationSecs(context, elapsed), ValueFormat.durationSecs(context, remaining),
            )
        } else {
            context.getStringFmt(elapsedRes, summary, ValueFormat.durationSecs(context, elapsed))
        }
    }

    /** Export progress summary with its timing. */
    fun exportProgress(context: Context, summary: String, timing: TransferTiming): String =
        progress(
            context, summary, timing,
            R.string.mail_export_progress_elapsed_fmt, R.string.mail_export_progress_remaining_fmt,
        )

    /** Import progress summary with its timing. */
    fun importProgress(context: Context, summary: String, timing: TransferTiming): String =
        progress(
            context, summary, timing,
            R.string.mail_import_progress_elapsed_fmt, R.string.mail_import_progress_remaining_fmt,
        )

    /**
     * Export Done card: "X messages · format · Y bytes · took T", with the saved
     * path once Download ran. A snapshot with no clock yet keeps the untimed form.
     */
    fun exportDone(
        context: Context,
        exported: UInt,
        format: String,
        bytes: ULong,
        savedPath: String,
        timing: TransferTiming,
    ): String {
        val elapsed = timing.elapsedSecs?.let { ValueFormat.durationSecs(context, it) }
        return when {
            elapsed != null && savedPath.isNotEmpty() -> context.getStringFmt(
                R.string.mail_export_saved_summary_timed_fmt, exported, format, bytes, elapsed, savedPath,
            )
            elapsed != null -> context.getStringFmt(
                R.string.mail_export_done_summary_timed_fmt, exported, format, bytes, elapsed,
            )
            savedPath.isNotEmpty() -> context.getStringFmt(
                R.string.mail_export_saved_summary_fmt, format, bytes, savedPath,
            )
            else -> context.getStringFmt(R.string.mail_export_done_summary_fmt, format, bytes)
        }
    }

    /** Import Done card: the counts and the time taken (untimed without a clock). */
    fun importDone(
        context: Context,
        imported: Any,
        skipped: Any,
        errored: Any,
        timing: TransferTiming,
    ): String {
        val elapsed = timing.elapsedSecs?.let { ValueFormat.durationSecs(context, it) }
        return if (elapsed != null) {
            context.getStringFmt(R.string.mail_import_done_summary_timed_fmt, imported, skipped, errored, elapsed)
        } else {
            context.getStringFmt(R.string.mail_import_done_summary_fmt, imported, skipped, errored)
        }
    }
}
