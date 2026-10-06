import SwiftUI

/// Thin adapter over the **shared** Logs presentation logic in `fauna_log::format`
/// (the severity filter ↔ level map, the time/line/row form, the newest-first copy
/// payload), reached over the UniFFI `log*` exports. The bug-prone parts no longer
/// live here — they live once in shared Rust and every app renders through them
/// (Linux natively, the native apps over `log*`, the web over WASM; priority
/// #2/#3, `observability.md` § Surfaces). This enum only does the genuinely
/// platform-specific bits: hand the FFI the device's local UTC offset (via the
/// one-door `DeviceOffset`, so the FFI renders local `HH:mm:ss` without a timezone
/// library on either side) and hold the localized filter-dropdown labels (display
/// chrome — the shared module owns the index→level *map*, not the labels). The
/// Swift twin of windows `LogsFormat.cs`.
///
/// **Redaction** (`observability.md` § Persistence & privacy): this layer renders
/// whatever `tracing`/`logMessage` captured; call sites never log secrets.
public enum LogsFormat {
    /// The severity-filter dropdown options, in order. Index 0 = "All" (everything);
    /// 1..=5 map to a `LogLevel` threshold via the shared `logLevelForIndex`.
    /// Localized display labels (the shared `fauna_log::format` module owns the
    /// index→level map, not the labels). The e2e `select("log-level-filter", "Error")`
    /// matches a Picker tag, so these are the tags too.
    public static var filterLabels: [String] {
        [L.logs.filterAll, L.logs.levelError, L.logs.levelWarn,
         L.logs.levelInfo, L.logs.levelDebug, L.logs.levelTrace]
    }

    /// Entries at or above the threshold the label selects — the label's *index*
    /// (its position in `filterLabels`) mapped through the shared `logLevelForIndex`,
    /// then narrowed by the shared `logFilterEntries`. "All" / unknown ⇒ every entry.
    public static func filtered(_ entries: [LogEntry], minLabel label: String) -> [LogEntry] {
        let index = filterLabels.firstIndex(of: label) ?? 0
        let min = logLevelForIndex(index: UInt32(index))
        return logFilterEntries(entries: entries, min: min)
    }

    /// The rendered `log-entry` rows, **newest-first** — the shared
    /// `{line, message, subtitle}` shape (`fauna_log::format::rows`). A Logs
    /// ring spans minutes, so a single "now" offset matches every entry's
    /// display (the across-a-DST-boundary case is ignored, as on every app).
    public static func rows(_ entries: [LogEntry]) -> [LogRow] {
        logRows(entries: entries, tzOffsetSecs: DeviceOffset.utcOffsetSeconds())
    }

    /// The given entries joined **newest-first** into one block — the copy payload
    /// (`fauna_log::format::rendered_text`).
    public static func renderedText(_ entries: [LogEntry]) -> String {
        logRenderedText(entries: entries, tzOffsetSecs: DeviceOffset.utcOffsetSeconds())
    }
}

/// Shared **Settings → Logs** / **admin Logs** view (`observability.md` § Surfaces,
/// ratified 2026-06-04). ONE FaunaKit view for both surfaces (priority #2):
/// - **client ring** (`.clientRing`): the macOS Settings shell `logs` rail sub-page
///   + the iOS Settings list — renders `logSnapshot()` with a Clear button.
/// - **nest ring** (`.admin`): the admin shells — renders `FfiAdminClient.logs()`,
///   no Clear (read-only `admin-logs`).
///
/// Both render the same `LogEntry` list **newest-first** with the same severity
/// filter (`log-level-filter`) and `log-entry` rows; only the *source* differs.
/// The row/filter/copy form comes from shared Rust (`LogsFormat` → the `log*`
/// UniFFI exports over `fauna_log::format`), the same logic linux/windows/web
/// render — so this view is the per-platform shell, not a re-implementation.
///
/// **Redaction** (`observability.md` § Persistence & privacy): this view only
/// renders what `tracing`/`logMessage` captured; call sites never log secrets.
public struct LogsView: View {
    public enum Source {
        /// The process-global client ring (`logSnapshot()` + `logClear()`).
        case clientRing
        /// The nest ring, fetched over `fauna.admin.logs` (`FfiAdminClient.logs()`).
        case admin(load: () async throws -> [LogEntry])
    }

    private let source: Source
    /// The admin heading id (`admin-logs-heading`); nil for the settings page
    /// (whose landmark is `settings-logs`).
    private let headingId: String?

    @State private var entries: [LogEntry] = []
    @State private var filterLabel: String = L.logs.filterAll
    @State private var errorText: String?

    public init(source: Source, headingId: String? = nil) {
        self.source = source
        self.headingId = headingId
    }

    private var isClientRing: Bool {
        if case .clientRing = source { return true }
        return false
    }

    private var displayed: [LogEntry] { LogsFormat.filtered(entries, minLabel: filterLabel) }

    /// The displayed entries as shared `LogRow`s — already **newest-first**
    /// (`fauna_log::format::rows`), so the list iterates them in order.
    private var rows: [LogRow] { LogsFormat.rows(displayed) }

    public var body: some View {
        coreView
            .task { await load() }
    }

    // The settings page carries the `settings-logs` landmark (its page id); the
    // admin page carries `admin-logs-heading` (a Text id) instead. `.contain`
    // keeps the child ids (`log-entry`, `log-level-filter`, …) queryable.
    @ViewBuilder private var coreView: some View {
        if isClientRing {
            content
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.settingsLogs)
                // Registry presence sentinel for the in-process driver (no a11y tree).
                .automationValue(Ids.settingsLogs, text: { "" })
        } else {
            content
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let headingId {
                automationText(headingId, L.logs.title)
                    .font(.title2).fontWeight(.semibold)
            }
            Text(L.logs.description)
                .font(.caption)
                .foregroundStyle(.secondary)

            controls

            if let errorText {
                ErrorBanner(message: errorText)
            }

            if rows.isEmpty {
                Text(L.logs.empty)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        // `rows` is already newest-first (shared `format::rows`).
                        ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                            entryRow(row)
                            Divider()
                        }
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var controls: some View {
        HStack(spacing: 12) {
            // Filter — a Picker with String tags = the displayed labels, so the
            // e2e `select("log-level-filter", "Error")` matches by tag (the same
            // drivable idiom as `filter-rule-type`).
            Picker(L.logs.filterLabel, selection: $filterLabel) {
                ForEach(LogsFormat.filterLabels, id: \.self) { Text($0).tag($0) }
            }
            .accessibilityIdentifier(Ids.logLevelFilter)
            // Picker: read the current label + select a new one (tags are the
            // displayed label strings, so the wire option is the binding value).
            .automationSelect(Ids.logLevelFilter, value: { filterLabel }) { filterLabel = $0 }
            .frame(maxWidth: 220)

            Spacer()

            Button(L.logs.copyButton) {
                copyLogs()
            }
            .accessibilityIdentifier(Ids.logCopyButton)
            .automationActivate(Ids.logCopyButton) { copyLogs() }

            if isClientRing {
                Button(L.logs.clearButton, role: .destructive) {
                    clearLogs()
                }
                .accessibilityIdentifier(Ids.logClearButton)
                .automationActivate(Ids.logClearButton) { clearLogs() }
            }
        }
    }

    private func entryRow(_ row: LogRow) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(row.message)
                .font(.callout)
                .lineLimit(3)
            Text(row.subtitle)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 4)
        // One element whose value is the full one-line form, so
        // get_text("log-entry", index) returns "LEVEL · time · target · message".
        .accessibilityElement(children: .ignore)
        .accessibilityIdentifier(Ids.logEntry)
        .accessibilityLabel(row.line)
        // In-process read: get_text("log-entry", index) returns the same one-line
        // form the a11y label carries ("LEVEL · time · target · message").
        .automationValue(Ids.logEntry, text: { row.line })
    }

    /// Copy the displayed entries — shared by the `log-copy-button` Button and its
    /// `automationActivate` sibling (no drift).
    private func copyLogs() {
        Pasteboard.copy(LogsFormat.renderedText(displayed))
    }

    /// Clear the client ring — shared by the `log-clear-button` Button and its
    /// `automationActivate` sibling (no drift).
    private func clearLogs() {
        logClear()
        entries = []
    }

    @MainActor
    private func load() async {
        switch source {
        case .clientRing:
            entries = logSnapshot()
        case .admin(let loader):
            do {
                entries = try await loader()
                errorText = nil
            } catch {
                errorText = error.localizedDescription
            }
        }
    }
}
