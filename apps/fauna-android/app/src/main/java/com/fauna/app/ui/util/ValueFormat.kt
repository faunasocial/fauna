package com.fauna.app.ui.util

import android.content.Context
import com.fauna.app.core.DeviceOffset
import java.text.DateFormat
import java.util.Date
import uniffi.fauna_core.RelativeTimeDisplay

/**
 * Human-readable value formatting (byte sizes, relative time) for the android
 * client. The unit / threshold *decision* lives once in shared Rust
 * (`fauna_core::format`, per `docs/goal/behavior/value-formatting.md`) and is
 * returned as an i18n key + args; this only resolves that `LocalizedText`
 * through the android string pipeline ([resolveLocalized]) — mirroring windows
 * `ValueFormat`, linux `i18n::byte_size`, and web `value-format.ts`. Clients
 * MUST NOT hand-roll the thresholds or the English unit strings (priority #2/#4).
 *
 * `duration_secs` (coarse uptime) is intentionally not wired: android renders no
 * uptime/duration UI, so there is no call site.
 */
object ValueFormat {
    /**
     * Largest 1024-unit ≥ 1, ≤ one decimal (trailing `.0` dropped): "512 B",
     * "1.5 KB", "3 GB", "1 TB". Replaces the native `Formatter.formatFileSize`
     * (which localized the decimal separator but hand-rolled nothing shared);
     * units (B/KB/MB/…) are locale-invariant.
     */
    fun byteSize(context: Context, bytes: Long): String =
        resolveLocalized(context, com.fauna.ffi.byteSize(bytes.toULong())) ?: ""

    /**
     * Relative timestamp for recent items ("just now", "5m ago", "2h ago",
     * "3d ago"); for items ≥ 7 days old the shared decision returns no localized
     * form and we render an absolute date with the platform's locale-aware
     * formatter.
     *
     * `epochMillis` MUST be epoch **milliseconds**. Convert at the call site for
     * sources that carry micros — feed posts and search results are epoch micros
     * (`÷1000`), matching web's `value-format.ts`; everything else is already ms.
     */
    fun relativeTime(context: Context, epochMillis: Long): String {
        val display = com.fauna.ffi.relativeTimeDisplay(System.currentTimeMillis(), epochMillis)
        display.localized?.let { return resolveLocalized(context, it) ?: "" }
        return display.absoluteEpochMs?.let {
            DateFormat.getDateInstance(DateFormat.SHORT).format(Date(it))
        } ?: ""
    }

    /**
     * Resolve an ALREADY-COMPUTED [RelativeTimeDisplay] (e.g. the `when` field of a
     * shared composite display like `backupLastUploadLabel`'s result) — no second FFI
     * round-trip, unlike [relativeTime]. Mirrors apple `ValueFormat.render(_:fallbackMs:)`.
     * `fallbackMs` is used only when the display carries neither a localized form nor
     * its own absolute epoch (shouldn't happen — defensive, matches the caller's own
     * source timestamp so degrading never renders nothing).
     */
    fun render(context: Context, display: RelativeTimeDisplay, fallbackMs: Long): String {
        display.localized?.let { return resolveLocalized(context, it) ?: "" }
        val ms = display.absoluteEpochMs ?: fallbackMs
        return DateFormat.getDateInstance(DateFormat.SHORT).format(Date(ms))
    }

    /**
     * Contextual last-activity timestamp for a conversation/thread row: today →
     * the local 24 h clock (`HH:MM`); Yesterday / a weekday name → the shared
     * localized form; older → an absolute date with the platform's locale-aware
     * formatter. The bucket *decision* lives once in shared Rust
     * (`fauna_core::format::conversation_timestamp_display`, per
     * `docs/goal/behavior/value-formatting.md` § Conversation timestamp); this
     * only renders the flattened `{ clock, localized, absoluteEpochMs }` (exactly
     * one set). The bucketing is timezone-aware, so we pass the device's current
     * UTC offset. `epochMillis` is epoch **milliseconds**.
     */
    /**
     * A tip amount on the shared sats/msats scale ("21 sats") for
     * `post-tip-total` and each `post-tip-item`'s amount. The unit split
     * (sats above 1 sat, msats below) is a shared decision
     * (`fauna_core::format::tip_amount`) — this only resolves it against the
     * android string pipeline. See `docs/goal/behavior/monetization.md` § Tips.
     */
    fun tipAmount(context: Context, msats: Long): String =
        resolveLocalized(context, com.fauna.ffi.tipAmount(msats)) ?: ""

    /** A tip count with its singular ("1 tip" / "3 tips") for
     *  `post-tip-count` — shared so no client ships "1 tips". */
    fun tipCount(context: Context, count: Long): String =
        resolveLocalized(context, com.fauna.ffi.tipCount(count)) ?: ""

    /**
     * The `post-tip-list` tail, "and N more", for a bounded attribution
     * window. `n` comes from the nest's own `hasMore` + totals, never from
     * comparing the rendered row count against a cap this client hard-codes.
     */
    fun tipMore(context: Context, n: Long): String =
        resolveLocalized(context, com.fauna.ffi.tipMore(n)) ?: ""

    /**
     * A fixed, non-localized `"YYYY-MM-DD HH:MM"` local wall-clock render for
     * audit/technical timestamps that intentionally do NOT bucket or
     * localize (`fauna_core::format::format_unix_local`, gated behind
     * `fauna-core`'s `local-clock` feature, forwarded here via `fauna-ffi`'s
     * own `value-format` feature) — the pending-actions row's
     * `pending-action-execute-after` text, mirroring tui's/linux's direct
     * `format_unix_local` call. `secs` is unix **seconds**.
     */
    fun absoluteLocal(secs: Long): String = com.fauna.ffi.formatUnixLocal(secs)

    fun conversationTimestamp(context: Context, epochMillis: Long): String {
        val offsetSeconds = DeviceOffset.utcOffsetSeconds()
        val display = com.fauna.ffi.conversationTimestampDisplay(
            System.currentTimeMillis(), epochMillis, offsetSeconds,
        )
        display.clock?.let { return it }
        display.localized?.let { return resolveLocalized(context, it) ?: "" }
        return display.absoluteEpochMs?.let {
            DateFormat.getDateInstance(DateFormat.SHORT).format(Date(it))
        } ?: ""
    }
}
