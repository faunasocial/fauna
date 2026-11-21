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

    /// The page's own empty line — `logs.empty` on the device page,
    /// `admin.logs_page.empty` on the nest one; also the reason the ring
    /// actions give while the filter shows nothing.
    public static func emptyText(admin: Bool) -> String {
        admin ? L.admin.logsPage.empty : L.logs.empty
    }

    /// Whether `log-copy-button` / `log-clear-button` can do anything now: the
    /// shared `ring_action_possibility` over the number of entries the active
    /// filter shows (`observability.md` § Surfaces, *Copy and clear offer only
    /// what they can do*) — blocked with the page's own empty line on an empty
    /// view. The buttons paint from it and both handlers ask it again (tui's
    /// `settings::logs::ring_action_possibility` / `admin::logs::copy_possibility`).
    public static func ringActionPossibility(shown: Int, admin: Bool) -> Possibility {
        logRingActionPossibility(
            shown: UInt32(clamping: shown),
            emptyReasonKey: admin ? "admin.logs_page.empty" : "logs.empty")
    }

    /// `log-copy-button`'s press: ask `answer` again (a refusal is said on
    /// `error-message`, nothing copied), else copy the displayed entries and
    /// record the outcome — `copied` holds the exact string now on the
    /// pasteboard (the button's "Copied!" + `copied` attr), `failure` the
    /// `common.copy_failed` line; a success clears a prior failure (tui's
    /// `record_copy_outcome`).
    @MainActor
    public static func copy(
        _ displayed: [LogEntry], answer: Possibility,
        copied: inout String?, failure: inout String?,
        writer: (String) throws -> Void = Pasteboard.write
    ) {
        if refusePress(answer, errorMessage: &failure) {
            copied = nil
            return
        }
        let outcome = CopyOutcome.attempt(renderedText(displayed), writer: writer)
        copied = outcome.copiedText
        failure = outcome.failure
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
        /// The nest ring, fetched over `fauna.admin.logs` (`FfiAdminClient.logs()`),
        /// plus — when `audit` is given — the admin audit log section below the
        /// log lines (`admin.md` § The admin audit log on `admin-logs`): the rows,
        /// the chain-check line and the cursor all come from the shared
        /// `FfiAuditLog` (`fauna_client_admin::audit_log_view`), never re-derived.
        case admin(load: () async throws -> [LogEntry],
                   audit: (() async throws -> FfiAuditLog?)? = nil)
    }

    private let source: Source
    /// The admin heading id (`admin-logs-heading`); nil for the settings page
    /// (whose landmark is `settings-logs`).
    private let headingId: String?

    @State private var entries: [LogEntry] = []
    @State private var filterLabel: String = L.logs.filterAll
    @State private var errorText: String?
    /// The copy/clear gestures' own line on `error-message` (a refusal or a
    /// failed copy) — apart from the read's `errorText`, so a copy that
    /// succeeds clears its own failure and never a load error.
    @State private var actionError: String?
    /// The exact string the last copy put on the pasteboard — the
    /// `log-copy-button` "Copied!" label and `copied` attr.
    @State private var copiedText: String?
    /// The audit section's state, held in Rust; fetched once per page entry.
    @State private var auditLog: FfiAuditLog?
    @State private var auditView: FfiAuditLogView?
    /// The audit log could not be obtained at all (its "more" answers failed).
    @State private var auditReadFailed = false

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

    /// The copy/clear answer over what the active filter shows.
    private var ringAnswer: Possibility {
        LogsFormat.ringActionPossibility(shown: displayed.count, admin: !isClientRing)
    }

    private var emptyText: String { LogsFormat.emptyText(admin: !isClientRing) }

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

            if let banner = actionError ?? errorText {
                ErrorBanner(message: banner)
            }

            // The page's own empty line — also the blocked reason of the
            // copy/clear answer, so the buttons paint no second copy of it.
            if rows.isEmpty && !hasAuditSection {
                Text(emptyText)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
            } else {
                ScrollView {
                    // Eager, not lazy: the automation registry registers a row when
                    // its view is built, so a lazy stack hides every row below the
                    // fold from `log-entry` queries (the older planted line in
                    // admin-logs 3 sits past the nest's startup lines).
                    VStack(alignment: .leading, spacing: 0) {
                        if rows.isEmpty {
                            Text(emptyText)
                                .foregroundStyle(.secondary)
                                .padding(.vertical, 8)
                        }
                        // `rows` is already newest-first (shared `format::rows`).
                        ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                            entryRow(row)
                            Divider()
                        }
                        if hasAuditSection {
                            auditSection
                        }
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var hasAuditSection: Bool {
        if case .admin(_, let audit) = source { return audit != nil }
        return false
    }

    /// The admin audit log section (`admin-audit-section`): heading, one
    /// `admin-audit-entry` per shared view row (absolute local time, then the
    /// resolved text), the one integrity line, and `admin-audit-more-button`
    /// (enabled iff the view still holds a cursor).
    private var auditSection: some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.adminAuditSection, L.admin.logsPage.auditTitle)
                .font(.headline)
                .padding(.top, 16)
            if let view = auditView {
                ForEach(Array(view.rows.enumerated()), id: \.offset) { _, row in
                    auditEntryRow(row)
                    Divider()
                }
                automationText(Ids.adminAuditIntegrityStatus, renderLocalizedText(view.integrityText))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The shared `audit_more_possibility` over the held log (admin.md
            // § 2, *What a row offers*): blocked, saying so, once the whole
            // record is shown; `auditMore` asks it again. Re-read on every
            // render, so a newly held page repaints it.
            let moreAnswer = auditMoreAnswer
            Button(L.admin.logsPage.auditMore) {
                Task { await auditMore() }
            }
            .accessibilityIdentifier(Ids.adminAuditMoreButton)
            .automationActivate(Ids.adminAuditMoreButton) {
                Task { await auditMore() }
            }
            .faunaPossible(moreAnswer)
            FaunaPossibilityReason(moreAnswer)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func auditEntryRow(_ row: FfiAuditLogRow) -> some View {
        let line = "\(ValueFormat.absoluteDate(epochMs: row.tsMs, withTime: true))  \(renderLocalizedText(row.text))"
        return Text(line)
            .font(.callout)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
            .accessibilityElement(children: .ignore)
            .accessibilityIdentifier(Ids.adminAuditEntry)
            .accessibilityLabel(line)
            .automationValue(Ids.adminAuditEntry, text: { line })
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

            // Copy and clear paint from the shared ring answer (disabled by
            // the page's own empty line, which is already in view — no
            // `FaunaPossibilityReason` beside them); a copy confirms like
            // `account-actor-id-copy-btn` ("Copied!" + the `copied` attr).
            let answer = ringAnswer
            Button(copiedText != nil ? L.common.copied : L.logs.copyButton) {
                copyLogs()
            }
            .accessibilityIdentifier(Ids.logCopyButton)
            .automationActivate(Ids.logCopyButton, attributes: { ["copied": copiedText ?? ""] }) {
                copyLogs()
            }
            .faunaPossible(answer)

            if isClientRing {
                Button(L.logs.clearButton, role: .destructive) {
                    clearLogs()
                }
                .accessibilityIdentifier(Ids.logClearButton)
                .automationActivate(Ids.logClearButton) { clearLogs() }
                .faunaPossible(answer)
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
    /// The press asks the ring answer again (rule 7) before copying.
    private func copyLogs() {
        LogsFormat.copy(displayed, answer: ringAnswer, copied: &copiedText, failure: &actionError)
    }

    /// Clear the client ring — shared by the `log-clear-button` Button and its
    /// `automationActivate` sibling (no drift). Asks the ring answer again: an
    /// empty view refuses with the page's empty line on `error-message`.
    private func clearLogs() {
        if refusePress(ringAnswer, errorMessage: &actionError) { return }
        logClear()
        entries = []
        copiedText = nil
        actionError = nil
    }

    @MainActor
    private func load() async {
        switch source {
        case .clientRing:
            entries = logSnapshot()
        case .admin(let loader, let auditLoader):
            do {
                entries = try await loader()
                errorText = nil
            } catch {
                errorText = error.localizedDescription
            }
            // A failed audit read lands on the same banner and keeps the rows held.
            guard let auditLoader else { return }
            do {
                guard let log = try await auditLoader() else { return }
                auditLog = log
                auditReadFailed = false
                auditView = try await log.load()
            } catch {
                errorText = error.localizedDescription
                if auditLog == nil { auditReadFailed = true }
            }
        }
    }

    /// The audit "more" answer: the held log's own once it is read; before
    /// that the read is still on its way.
    private var auditMoreAnswer: Possibility {
        // `auditView` is read so a newly held page re-renders this answer.
        _ = auditView
        return machinePossibility(auditLog, buildFailed: auditReadFailed) { $0.morePossibility() }
    }

    @MainActor
    private func auditMore() async {
        if refusePress(auditMoreAnswer, errorMessage: &errorText) { return }
        guard let log = auditLog else { return }
        do {
            auditView = try await log.more()
            errorText = nil
        } catch {
            errorText = error.localizedDescription
        }
    }
}
