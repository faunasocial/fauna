import SwiftUI

/// The two session-local verdict surfaces of the Backups page's snapshot half —
/// the integrity-check result and the prune preview. Shared by macOS + iOS (one
/// FaunaKit view each), rendered **inline on the page**, which is the shape every
/// other app already ships (linux's `check_result_label` + `prune_preview_box`,
/// web's and android's equivalents).
///
/// `snapshot-check-result` / `snapshot-prune-preview` (+ its `snapshot-prune-
/// execute-button` / `snapshot-prune-cancel-button`) are the four result-surface
/// `optional_elements` `docs/goal/ui/backups.md` § Snapshot-list shape ratified
/// 2026-08-13 — tagging the chrome every app (tui/linux/web/android/windows)
/// already painted, so the *Prune*/*Check* rulings finally have e2e pressure.
///
/// This pair is what retired apple's two sheets. `IntegrityCheckSheet` made
/// `snapshot-check-button` merely *open* a sheet whose second, untagged "Start
/// check" button ran the check — the actuation-contract violation the *Check*
/// ruling names — and it re-derived its verdict badge from error counts because
/// the old `APIClient` dropped `is_ok`. `RetentionEditorSheet` let the page
/// author a `keep_*` retention policy, which Architectural rule 5 forbids (and
/// whose hand-encoded writer was the known live source of the nest's
/// `policy_state: unparseable` arm).

/// The integrity-check verdict. The verdict is the shared `is_ok` predicate,
/// **read** off `CheckOutcome` — never re-derived from `status == "ok"` or from
/// error counts (§ Snapshot-list shape, *Check* ruling).
///
/// A completed check with problems is a **result, not an error**: it renders
/// here, never on `error-message` (§ Architectural rules, rule 6).
public struct CheckResultView: View {
    private let result: CheckOutcome

    public init(result: CheckOutcome) {
        self.result = result
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: result.isOk ? "checkmark.circle.fill"
                                          : "exclamationmark.triangle.fill")
                .foregroundStyle(result.isOk ? .green : .orange)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal)
        .padding(.vertical, 6)
        .accessibilityIdentifier(Ids.snapshotCheckResult)
        .automationValue(Ids.snapshotCheckResult, text: { text })
    }

    private var text: String {
        if result.isOk {
            return L.backups.checkResultOk(
                snapshots: String(result.snapshotsChecked),
                files: String(result.filesChecked),
                chunks: String(result.chunksChecked))
        }
        return L.backups.checkResultErrors(
            missingManifests: String(result.missingManifests),
            missingChunks: String(result.missingChunks),
            corruptManifests: String(result.corruptManifests))
    }
}

/// The two per-row labels the *Row content contract* adds to `snapshot-item[i]`
/// beyond its formatted created-at / file-count / size. Shared by macOS + iOS so
/// the two targets cannot drift on either rule; both route through the shared
/// `fauna_backups_machine::{snapshot_state_text, snapshot_integrity_text}`
/// (`docs/goal/ui/backups.md` § Where logic lives) —
/// apple owns only the timestamp formatting, never which key or the
/// dated/undated fallback.
public enum SnapshotRowText {
    /// A non-`Active` lifecycle state renders ON the row, carrying the deadline
    /// the user can still act on — that deadline is the whole reason the wire
    /// gained the lifecycle fields. `nil` for `Active`.
    ///
    /// The deadline is `Option` on the wire (a pre-lifecycle-fields nest, or a
    /// pending action whose row was already consumed): the STATE still renders
    /// and only the date is dropped. Inventing a date — or hiding the state
    /// because there is none — is the failure this shape avoids.
    public static func state(_ state: SnapshotState) -> String? {
        let deadline: Int64?
        switch state {
        case .active: deadline = nil
        case .deletionPending(let executeAfter): deadline = executeAfter
        case .softDeleted(let purgeAfter): deadline = purgeAfter
        }
        let formatted = deadline.map { Date(epochSeconds: $0).relativeFormatted }
        return snapshotStateText(state: state, formattedDeadline: formatted).map(renderLocalizedText)
    }

    /// Per-row integrity, derived by the machine from a check reply's
    /// `structured_errors` — never nest state. **Absent until a check runs this
    /// session**: `Unknown` paints nothing rather than the word "unknown", which
    /// on this page would read as a finding (the shape tui's leg recorded).
    public static func integrity(_ integrity: RowIntegrity) -> String? {
        snapshotIntegrityText(integrity: integrity).map(renderLocalizedText)
    }

    /// `snapshot-item[i]`'s own automation text — the whole *Row content
    /// contract* in one line, tui's shape: the shared `snapshot_row` string
    /// (id · created-at · file count · size) plus the state and integrity
    /// suffixes, each two-space separated. The row's `value` stays the bare id
    /// (the friction bar re-types it); this is what `get_text` answers, as it
    /// does on every other app.
    public static func line(_ snapshot: SnapshotRow) -> String {
        var text = L.backups.snapshotRow(
            id: String(snapshot.id),
            when: Date(epochSeconds: snapshot.createdAt).relativeFormatted,
            files: L.backups.fileCount(count: String(snapshot.fileCount)),
            size: ValueFormat.byteSize(UInt64(max(0, snapshot.totalBytes))))
        for suffix in [state(snapshot.state), integrity(snapshot.integrity)].compactMap({ $0 }) {
            text += "  " + suffix
        }
        return text
    }
}

/// The prune preview — a dry run of THIS SET'S OWN resting retention policy,
/// with execute offered only from a standing preview (§ Snapshot-list shape,
/// *Prune* ruling; `prune_execute` is a no-op without one).
///
/// The policy is never client-supplied here, which is why none of these strings
/// names a `keep_*` bound: retention is edited in the folder wizard/config.
public struct PrunePreviewView: View {
    private let preview: PrunePreview
    private let busy: Bool
    private let onExecute: () -> Void
    private let onCancel: () -> Void

    public init(preview: PrunePreview, busy: Bool,
                onExecute: @escaping () -> Void, onCancel: @escaping () -> Void) {
        self.preview = preview
        self.busy = busy
        self.onExecute = onExecute
        self.onCancel = onCancel
    }

    /// The verdict sentence the body paints under the title — one of the three
    /// states typed off the reply (§ Errors & edge cases, *Prune with no
    /// candidates*: "no app infers them"), keyed exactly as the body's own switch
    /// keys it, so the painted line and the automation text cannot disagree.
    static func verdict(of preview: PrunePreview) -> String {
        switch preview.policyState {
        case .notSet:
            return L.backups.prunePolicyNotSet
        case .unparseable:
            return L.backups.prunePolicyUnparseable
        case .applied where preview.wouldPrune == 0:
            return L.backups.prunePreviewNothing
        case .applied:
            return L.backups.prunePreviewCounts(
                wouldPrune: String(preview.wouldPrune),
                remaining: String(preview.remaining))
        }
    }

    /// `snapshot-prune-preview`'s own text: the title and the verdict, the shape
    /// tui's `prune_preview_elements` declares. "Nothing to prune" and "no
    /// retention policy configured" are otherwise indistinguishable from outside
    /// — the preview is present and the execute button absent in both.
    static func automationText(of preview: PrunePreview) -> String {
        "\(L.backups.prunePreviewTitle)  \(verdict(of: preview))"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.backups.prunePreviewTitle)
                .font(.headline)

            switch preview.policyState {
            case .notSet:
                Text(L.backups.prunePolicyNotSet)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .unparseable:
                // Surfaced LOUDLY (the ruling's word): this is the arm apple's own
                // hand-encoded `keep_*` writer used to produce.
                Text(L.backups.prunePolicyUnparseable)
                    .font(.caption)
                    .foregroundStyle(.orange)
            case .applied:
                if preview.wouldPrune == 0 {
                    Text(L.backups.prunePreviewNothing)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    Text(L.backups.prunePreviewCounts(
                        wouldPrune: String(preview.wouldPrune),
                        remaining: String(preview.remaining)))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ForEach(preview.candidates, id: \.id) { candidate in
                        Text(L.backups.prunePreviewCandidate(
                            id: String(candidate.id),
                            when: Date(epochSeconds: candidate.createdAt).relativeFormatted))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            HStack {
                Button(L.backups.pruneCancelButton, action: onCancel)
                    .accessibilityIdentifier(Ids.snapshotPruneCancelButton)
                    .automationActivate(Ids.snapshotPruneCancelButton, perform: onCancel)
                if preview.policyState == .applied && preview.wouldPrune > 0 {
                    Button(L.backups.pruneExecuteButton, action: onExecute)
                        .tint(.red)
                        .disabled(busy)
                        .accessibilityIdentifier(Ids.snapshotPruneExecuteButton)
                        .automationActivate(Ids.snapshotPruneExecuteButton,
                                            isEnabled: { !busy }, perform: onExecute)
                }
                Spacer()
            }
            .padding(.top, 2)
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        // `.contain` keeps this container's own id queryable alongside its two
        // button children's ids — a bare `.accessibilityIdentifier` here would
        // otherwise clobber them (apple-section-accessibilityid-clobbers-children).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.snapshotPrunePreview)
        .automationValue(Ids.snapshotPrunePreview, text: { Self.automationText(of: preview) })
    }
}

extension View {
    /// The snapshot-list delete confirmation — byte-identical on macOS
    /// (`MacSnapshotTimelineView`) and iOS (`SnapshotListView`) before this
    /// extraction, both hand-rolling the same `.confirmationDialog` over a
    /// `pendingDelete: SnapshotRow?` clear-on-dismiss binding.
    public func snapshotDeleteConfirmation(
        pendingDelete: Binding<SnapshotRow?>,
        onDelete: @escaping (SnapshotRow) -> Void
    ) -> some View {
        confirmationDialog(
            L.backups.deleteSnapshot,
            isPresented: Binding(
                get: { pendingDelete.wrappedValue != nil },
                set: { if !$0 { pendingDelete.wrappedValue = nil } }
            ),
            presenting: pendingDelete.wrappedValue
        ) { snapshot in
            Button(L.backups.deleteSnapshot, role: .destructive) {
                onDelete(snapshot)
            }
            Button(L.common.cancel, role: .cancel) {}
        } message: { _ in
            Text(L.backups.deleteSnapshotConfirm)
        }
    }
}
