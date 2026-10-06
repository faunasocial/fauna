import Foundation

/// Human-readable display formatting (byte sizes, relative time, durations) for
/// the Apple apps (macOS + iOS, shared via FaunaKit). The *decision* — which
/// 1024-unit, which relative-time bucket, which duration units, the rounding —
/// lives once in shared Rust (`fauna_core::format`, per
/// `docs/goal/behavior/value-formatting.md`) and is returned as an i18n key +
/// args; this only resolves that `LocalizedText` through the apple i18n pipeline
/// (`renderLocalizedText` → `L.lookup`). Clients MUST NOT hand-roll the
/// thresholds or the English unit strings (priority #2; #4 — one shared shape
/// across all 7 apps, mirroring windows `ValueFormat`, android `ValueFormat`,
/// linux `i18n::byte_size`/`duration_secs`, and web `value-format.ts`).
public enum ValueFormat {
    /// Largest 1024-unit ≥ 1, ≤ one decimal (trailing `.0` dropped): "512 B",
    /// "1.5 KB", "3 GB", "1 TB". Replaces the native `ByteCountFormatter`
    /// (which localized the decimal separator and used 1000-based `.file`
    /// counts); units (B/KB/MB/…) are locale-invariant — uniformity over the
    /// native separator is the accepted pre-production tradeoff (goal doc).
    public static func byteSize(_ bytes: UInt64) -> String {
        renderLocalizedText(FaunaFFISwift.byteSize(bytes: bytes))
    }

    /// `Int`-valued convenience for the app-side byte-count models (already
    /// narrowed from the wire's `i64`/`u64` at the API boundary); clamps a
    /// negative value to 0 rather than trapping, unlike a bare `UInt64(_:)`
    /// cast at the call site.
    public static func byteSize(_ bytes: Int) -> String {
        byteSize(UInt64(max(0, bytes)))
    }

    /// Relative timestamp for recent items ("just now", "5m ago", "2h ago",
    /// "3d ago"); items ≥ 7 days old return no localized form and we render an
    /// absolute date with the platform's native, locale-aware formatter.
    ///
    /// `nowMs` / `thenMs` are epoch **milliseconds** (UTC) — convert micros-based
    /// sources (feed/notification/search `created_at`) with `/ 1000` and
    /// seconds-based sources (device `last_seen_at`) with `* 1000` at the call
    /// site. The bucket decision lives in shared Rust (`relative_time_display`);
    /// a `nil` localized result is the signal to format the absolute epoch.
    public static func relativeTime(nowMs: Int64, thenMs: Int64) -> String {
        render(relativeTimeDisplay(nowMs: nowMs, thenMs: thenMs), fallbackMs: thenMs)
    }

    /// Resolve an **already-computed** `RelativeTimeDisplay` — e.g. one
    /// embedded in a richer FFI record such as `BackupLastUploadDisplay.when`
    /// — without a second `relativeTimeDisplay` FFI round-trip. Same
    /// localized-else-absolute-date resolution `relativeTime` uses.
    /// `fallbackMs` covers the (unreachable in practice) case where neither
    /// `localized` nor `absoluteEpochMs` is set.
    public static func render(_ display: RelativeTimeDisplay, fallbackMs: Int64) -> String {
        if let localized = display.localized {
            return renderLocalizedText(localized)
        }
        let absMs = display.absoluteEpochMs ?? fallbackMs
        let date = Date(timeIntervalSince1970: Double(absMs) / 1000.0)
        return DateFormatter.localizedString(from: date, dateStyle: .short, timeStyle: .none)
    }

    /// Convenience: relative time against the current wall clock.
    public static func relativeTime(thenMs: Int64) -> String {
        relativeTime(nowMs: Int64(Date().timeIntervalSince1970 * 1000), thenMs: thenMs)
    }

    /// Convenience for a Swift `Date` value (vs. an epoch scalar). The
    /// `Date.relativeFormatted` extension delegates here, so every `Date`-typed
    /// relative-time site routes through the shared formatter.
    public static func relativeTime(_ date: Date) -> String {
        relativeTime(thenMs: Int64(date.timeIntervalSince1970 * 1000))
    }

    /// Contextual last-activity timestamp for conversation / DM rows — the
    /// calendar-based bucket (today → local 24 h clock, Yesterday, weekday name,
    /// else an absolute date), decided once in shared Rust
    /// (`conversation_timestamp_display`) in the caller's local timezone. Replaces
    /// the per-app divergence this lift removes — Linux's UTC `HH:MM`, Android's
    /// relative-duration, and Apple's own former `ConversationsUI.shortTimestamp`
    /// (value-formatting.md § Conversation timestamp; priority #1/#2/#4).
    ///
    /// `thenMs` is epoch **milliseconds** (UTC); a non-positive value (no activity)
    /// renders "". The today bucket is the shared 24 h `clock` ("14:30") — the
    /// canonical all-6-client shape, NOT a reconstructed locale-aware 12 h clock
    /// (which would re-introduce the divergence this lift removes). The older
    /// bucket formats the absolute epoch with the platform's native, locale-aware
    /// formatter (date-only, `.medium` — the `DateFormatter` equivalent of the
    /// abbreviated-month form apple's prior `shortTimestamp` showed) — the goal doc
    /// leaves the absolute style to each app, so apple keeps its prior
    /// conversation-row form; only the bucket *logic* (and the local-zone fix)
    /// is what unifies.
    public static func conversationTimestamp(nowMs: Int64, thenMs: Int64) -> String {
        guard thenMs > 0 else { return "" }
        let display = conversationTimestampDisplay(
            nowMs: nowMs, thenMs: thenMs,
            utcOffsetSeconds: DeviceOffset.utcOffsetSeconds())
        if let clock = display.clock { return clock }              // today → 24 h HH:MM
        if let localized = display.localized {                     // Yesterday / weekday
            return renderLocalizedText(localized)
        }
        let absMs = display.absoluteEpochMs ?? thenMs              // older → native date
        let date = Date(timeIntervalSince1970: Double(absMs) / 1000.0)
        return DateFormatter.localizedString(from: date, dateStyle: .medium, timeStyle: .none)
    }

    /// Convenience: contextual conversation timestamp against the current wall clock.
    public static func conversationTimestamp(thenMs: Int64) -> String {
        conversationTimestamp(nowMs: Int64(Date().timeIntervalSince1970 * 1000), thenMs: thenMs)
    }

    /// Absolute, locale-aware calendar date for surfaces that show an *exact*
    /// date rather than a relative bucket — mail credential creation, alias
    /// last-hit, mailing-list last-send, spam-event timestamps. `epochMs` is
    /// epoch **milliseconds** (UTC); seconds-based sources (e.g. credential
    /// `createdAt`) multiply by 1000 at the call site. `withTime` appends a
    /// short time component (spam events are time-of-day relevant); the date
    /// style is always `.medium`. Unlike `relativeTime`, the bucket decision is
    /// trivially "always absolute," so there is no shared-Rust round-trip — only
    /// the platform's native locale formatter (the same one `relativeTime` falls
    /// back to for items ≥ 7 days old). Consolidates four byte-identical
    /// `relativeDate` helpers previously duplicated across the mail views
    /// (priority #2/#4). Whether these surfaces should instead adopt
    /// `relativeTime`'s "5m ago" buckets is a separate UX decision deferred to
    /// the mail goal docs.
    public static func absoluteDate(epochMs: Int64, withTime: Bool = false) -> String {
        let date = Date(timeIntervalSince1970: Double(epochMs) / 1000.0)
        return DateFormatter.localizedString(
            from: date,
            dateStyle: .medium,
            timeStyle: withTime ? .short : .none)
    }

    /// Coarse d/h/m uptime/duration — the largest non-zero unit down to minutes,
    /// always the full chain below it, seconds dropped: "0m", "1h 0m",
    /// "5d 2h 30m". The multi-arg `{days,hours,mins}` key resolves by name in
    /// `renderLocalizedText` (order-robust), so it sidesteps the windows
    /// positional-resolver bug.
    public static func durationSecs(_ secs: UInt64) -> String {
        renderLocalizedText(FaunaFFISwift.durationSecs(secs: secs))
    }

    /// `total_msats` summed over a post's tips → "1.2k sats" etc.
    /// (`docs/goal/behavior/monetization.md` § Tips). Renders `post-tip-total`,
    /// which paints **iff** the caller checks `total_msats != 0` first — this
    /// helper does not itself gate. Ungated inert surface (`value_format.rs` §
    /// Gated element ids live in gated DOC LINES) — the caller's `Ids.postTip*`
    /// reference is what needs `#if !FAUNA_EXCISE_PAYMENTS`, not this formatter.
    public static func tipAmount(_ msats: Int64) -> String {
        renderLocalizedText(FaunaFFISwift.tipAmount(msats: msats))
    }

    /// A tip `tip_count` → "3 tips" etc., shared so no app hand-rolls the
    /// plural (`docs/goal/behavior/monetization.md` § Tips). Renders
    /// `post-tip-count`.
    public static func tipCount(_ count: Int64) -> String {
        renderLocalizedText(FaunaFFISwift.tipCount(count: count))
    }

    /// The `post-tip-list` bounded window's "+N more" tail — `n` is the
    /// caller's `tipCount - senders.count` (`docs/goal/behavior/monetization.md`
    /// § Tips).
    public static func tipMore(_ n: Int64) -> String {
        renderLocalizedText(FaunaFFISwift.tipMore(n: n))
    }

    /// The display host of a `scheme://host[:port]/…` url — host only, no scheme/path/port
    /// (e.g. `https://example.com/article` → "example.com") — for the D4 `link-preview-domain`
    /// (render-model.md § D4). The host extraction lives once in shared Rust
    /// (`fauna_core::format::url_host`), so every app shows the identical domain string
    /// (windows `FaunaFfiMethods.UrlHost`, linux/web `url_host`); never a per-app URL parse
    /// (priority #2/#3). Falls back to the original url when no host can be extracted, matching
    /// the shared function.
    public static func urlHost(_ url: String) -> String {
        FaunaFFISwift.urlHost(url: url)
    }

    /// A feature-limits quota cell's headroom sentence, fully composed — the
    /// inner magnitudes resolved first, then substituted into the outer
    /// template (`docs/goal/architecture/dynamic-features.md` § Transparency &
    /// auditability). `{remaining}`/`{limit}` are already-finished numbers
    /// unless `cell.magnitudes` is set, in which case those two
    /// `LocalizedText`s are resolved and substituted instead — the
    /// `BackupLastUploadDisplay` two-level shape, for the same reason (a
    /// `LocalizedText` argument is a flat string, so a localized magnitude
    /// cannot be nested inside one). Swift twin of
    /// `fauna_client_features::row::cell_value_text` — no FFI export exists for
    /// it, so every non-Rust app resolves the two-level contract through its
    /// own pipeline (mirrors android's `Localized.kt::cellValueText`, web's
    /// `localized.ts::cellValueText`).
    public static func cellValueText(_ cell: FfiLimitCell) -> String {
        guard let magnitudes = cell.magnitudes else {
            return renderLocalizedText(cell.value)
        }
        var composed = cell.value
        composed.args["remaining"] = renderLocalizedText(magnitudes.remaining)
        composed.args["limit"] = renderLocalizedText(magnitudes.limit)
        return renderLocalizedText(composed)
    }

    /// The `custody-holder-receipt-status` line (`docs/goal/ui/devices.md` §
    /// Custody facet) — the A7 three-state honesty rule, where fresh / stale /
    /// no-receipt-yet are three different strings that never collapse or go
    /// empty. Which state maps to which key is decided in shared Rust and
    /// already folded into the row; this only resolves that key and
    /// substitutes `{when}`. The shared side deliberately hands back epoch
    /// SECONDS rather than a rendered timestamp — `format_unix_local` needs the
    /// OS timezone database and is native-only, so formatting there would cost
    /// `fauna-client-capabilities` the wasm-cleanliness the web leg depends on.
    /// Formatting rides the SHARED `formatUnixLocal` over FFI, never
    /// `DateFormatter`, so a timestamp reads identically on all seven apps.
    /// Swift twin of android `Localized.kt::custodyReceiptStatusText` / linux
    /// `i18n::custody_receipt_status`.
    public static func custodyReceiptStatusText(_ receipt: CustodyReceiptRowView) -> String {
        var composed = receipt.statusLabel
        if let secs = receipt.attestedAtSecs {
            composed.args["when"] = FaunaFFISwift.formatUnixLocal(secs: secs)
        }
        return renderLocalizedText(composed)
    }

    /// The `custody-holder-held-bytes` line — held bytes against the budget in
    /// force. The two inner byte texts are themselves `LocalizedText` and are
    /// resolved first (the `cellValueText` two-level composition, above).
    /// `degraded` is **orthogonal to freshness** — a fresh receipt can
    /// honestly report dropped payload — so its marker appends to this line
    /// rather than replacing the status one. Swift twin of android
    /// `Localized.kt::custodyHeldBytesText` / linux `i18n::custody_held_bytes`.
    public static func custodyHeldBytesText(_ receipt: CustodyReceiptRowView) -> String {
        var composed = receipt.heldBytesLabel
        composed.args["held"] = renderLocalizedText(receipt.held)
        composed.args["cap"] = renderLocalizedText(receipt.cap)
        let line = renderLocalizedText(composed)
        guard receipt.degraded else { return line }
        let badge = renderLocalizedText(LocalizedText(key: FaunaFFISwift.custodyDegradedBadgeKey(), args: [:]))
        return "\(line) — \(badge)"
    }
}
