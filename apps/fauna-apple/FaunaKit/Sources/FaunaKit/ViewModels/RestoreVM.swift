import SwiftUI

/// Shared view-model for the **message-kind (mail/calendar) restore** surfaces of
/// the Backups page (`docs/goal/ui/backups.md` §§ Restore from backup destination /
/// Restore history / Restore divergence), macOS + iOS (one FaunaKit VM). A thin
/// consumer of the shared `libs/fauna-client-snapshots` WS-RPC composition
/// projected as `FfiSnapshotsClient`, reached through the four `APIClient`
/// wrappers (`fetchMessageKindSnapshots` / `fetchRestoreHistory` /
/// `fetchRestoreDivergence` / `restoreMessageKind`). The render rules here are the
/// per-app glue `backups.md` § Where logic lives leaves to each app.
///
/// Reference: android `LocalRestoreVM` + `RestoreHistoryVM` (all 13 restore-* ids,
/// 2026-06-15) and the linux exemplar
/// `apps/fauna-linux/src/views/backups/restore.rs`. Like both, the wired action is
/// the **local single-snapshot restore** — pick a message-kind snapshot, re-type
/// its id, dispatch `restore_message_kind` once. There is **no** ±60s pairing, no
/// per-kind sequential dispatch, and no client-driven bridge disable→restore→
/// re-enable: the restore endpoint requires the bridge not be serving the actor,
/// which the user arranges via the existing `mail-settings-enabled-toggle`
/// (progress ends "Done — restart the bridge."; `backup-restore.md` § 6). The
/// cross-location `restore-source-select` is a decorative disabled-at-zero-
/// destinations affordance (the destination chunk-pull is blocked on backup-Plan 4).
@MainActor @Observable
public final class RestoreVM {
    // MARK: - Local restore action state

    /// Snapshots offered by `restore-snapshot-select`, from
    /// `fauna.filesync.snapshot.list` (owner-implicit, all kinds — matching
    /// android/linux, which do not filter to message-kind client-side).
    public private(set) var snapshots: [FfiSnapshotSummary] = []
    /// The picked snapshot's id (auto-selected to the first on load, like linux's
    /// single-snapshot auto-select). The friction bar matches against this id.
    public var selectedSnapshotId: Int64?
    /// Bound by `restore-confirm-input`. The friction bar arms only when this
    /// exactly equals the selected snapshot's id (`backups.md` § Where logic lives).
    public var confirmText = ""
    /// The two decorative `restore-kind-checkbox`es (mail, calendar), both checked
    /// by default. They render for parity + the spec but do not gate the local
    /// single-kind-per-snapshot restore (the mail+calendar pair-restore they'd
    /// drive is the destination flow, blocked on backup-Plan 4) — matching android.
    public var mailChecked = true
    public var calendarChecked = true
    /// Whether ≥1 backup destination is configured — gates `restore-source-select`
    /// enabled/disabled (the only wired effect of the decorative source picker).
    public private(set) var hasDestinations = false
    /// `restore-progress` state.
    public private(set) var progress: RestoreProgress = .idle
    /// `restore-warning`: the last restore's reply said `config_present == false`
    /// — the restore proceeded, but the account holds no wrapped-MLS
    /// blob bundle, so the bridge can't sign in after it restarts until that is restored
    /// too. The reply is the advisory's only carrier (no read reproduces it), so
    /// this flag is the one copy. The page paints the shared
    /// `backups.restore_warning_config_absent` string off it — never the reply's
    /// diagnostic `note` (`backups.md` § Restore from backup destination).
    public private(set) var configAbsent = false

    // MARK: - Restore history / divergence read state

    /// The `restore_history` rows (`restore-history-item`), newest-first as the
    /// nest returns them.
    public private(set) var history: [FfiRestoreHistoryRow] = []
    /// Per-snapshot divergence rows keyed by `snapshot_id`, one
    /// `list_restore_divergence` round-trip per history row (mirrors android). A
    /// row with a non-empty list renders `restore-divergence-banner`.
    public private(set) var divergence: [Int64: [FfiRestoreDivergenceRow]] = [:]
    /// The rows shown by the open `restore-divergence-details-modal`; `nil` = closed.
    public private(set) var modalRows: [FfiRestoreDivergenceRow]?

    // MARK: - Page surface

    /// `error-message` — carries restore-dispatch failures (e.g. the 409 when the
    /// bridge is still serving). A failed read leaves the prior state, no scary
    /// error before the user acts.
    public var errorMessage: String?

    private var api: APIClient?

    public init() {}

    public func configure(api: APIClient) {
        self.api = api
    }

    // MARK: - Hydrate

    /// Load the picker snapshots + destination presence + restore history. The
    /// snapshot list is a single NestClient RPC, which the transport already
    /// waits out while the socket is still connecting post-login (`transport.md`
    /// § Request lifecycle step 3's note) — no app-side retry needed. A failed
    /// first hydrate leaves empty state.
    public func hydrate() async {
        // Arriving on the page is not the end of a restore: the progress line
        // returns to its idle prompt and the one-shot advisory goes with it
        // (android's `LocalRestoreVM.refresh` does the same).
        progress = .idle
        configAbsent = false
        guard let api else { return }
        guard let list = try? await api.fetchMessageKindSnapshots() else { return }
        snapshots = list
        if selectedSnapshotId == nil { selectedSnapshotId = snapshots.first?.id }
        hasDestinations = !((try? await api.listBackupDestinations()) ?? []).isEmpty
        await loadHistory()
    }

    /// (Re)load `restore_history` + one `list_restore_divergence` per row. Called on
    /// hydrate and after a successful restore, so the new row + any divergence
    /// banner appear. A transient failure leaves the prior state.
    public func loadHistory() async {
        guard let api else { return }
        do {
            let rows = try await api.fetchRestoreHistory()
            var map: [Int64: [FfiRestoreDivergenceRow]] = [:]
            for row in rows {
                // Per-row failure degrades to no-banner, not a whole-load failure
                // (mirrors android's per-row `try?`).
                if let d = try? await api.fetchRestoreDivergence(snapshotId: row.snapshotId) {
                    map[row.snapshotId] = d
                }
            }
            history = rows
            divergence = map
        } catch {
            // Leave prior history; the restore-history section stays as-is.
        }
    }

    // MARK: - Local restore action

    /// The `restore-confirm-button` enabled predicate: a snapshot is selected, the
    /// typed text exactly equals its id, and no restore is in flight. Mirrors
    /// linux `recompute_enabled` / android `confirmEnabled`.
    public var confirmEnabled: Bool {
        guard let id = selectedSnapshotId, !confirmText.isEmpty else { return false }
        return confirmText == String(id) && progress != .running
    }

    /// Dispatch `restore_message_kind` for the selected snapshot (one call — no
    /// per-kind loop), then refresh history so the new row surfaces. On failure the
    /// progress falls back to idle and the error surfaces in `error-message`.
    public func restore() async {
        guard let api, let id = selectedSnapshotId else { return }
        let confirmId = confirmText
        progress = .running
        configAbsent = false
        errorMessage = nil
        do {
            let reply = try await api.restoreMessageKind(snapshotId: id, confirmId: confirmId)
            // Decided in the same state change that publishes DONE, so a reader
            // that sees DONE sees the advisory's final verdict.
            configAbsent = !reply.configPresent
            progress = .done
            await loadHistory()
        } catch {
            progress = .idle
            errorMessage = DisplayError.message(error)
        }
    }

    // MARK: - Divergence modal

    public func openDivergenceModal(snapshotId: Int64) {
        modalRows = divergence[snapshotId] ?? []
    }

    public func closeDivergenceModal() {
        modalRows = nil
    }

    // MARK: - Render helpers (the client-glue render rules)

    /// Label for a `restore-snapshot-select` option, via the shared
    /// `snapshotRestoreOptionLabel` — android/linux/tui/windows consume the
    /// same logic.
    public func snapshotLabel(_ s: FfiSnapshotSummary) -> String {
        snapshotRestoreOptionLabel(messageKind: s.messageKind, id: s.id)
    }

    /// The current picker selection's label (backs `restore-snapshot-select`'s
    /// `value` read); empty when nothing is selected.
    public var selectedSnapshotLabel: String {
        guard let id = selectedSnapshotId,
              let s = snapshots.first(where: { $0.id == id }) else { return "" }
        return snapshotLabel(s)
    }

    /// One `restore-history-item`'s text: `"{kinds} from {source} — {when}"`.
    /// `source_member_id` None → "local snapshot"; Some → shared `hex_short`
    /// (`backups.md` § Where logic lives). `when` = a locale-formatted
    /// completed-at (the human-facing precision android renders; the e2e test
    /// asserts only on kinds + source).
    public func historyRowText(_ row: FfiRestoreHistoryRow) -> String {
        let source: String
        if let member = row.sourceMemberId {
            source = hexShort(bytes: member)
        } else {
            source = L.backups.restoreSourceLocal
        }
        return L.backups.restoreHistoryRow(
            kinds: row.kindsRestored,
            source: source,
            when: Self.formatWhen(row.completedAt))
    }

    /// Whether a history row's snapshot has ≥1 divergence row (→ render the banner).
    public func hasDivergence(_ snapshotId: Int64) -> Bool {
        !(divergence[snapshotId] ?? []).isEmpty
    }

    /// `restore-divergence-banner` text: "N MUAs reconnected with newer state".
    public func divergenceBannerText(_ snapshotId: Int64) -> String {
        let n = divergence[snapshotId]?.count ?? 0
        return L.backups.restoreDivergenceBanner(count: String(n))
    }

    /// One `restore-divergence-details-item`'s text. `mua_id` None/empty →
    /// "(unknown)"; `lost_event_count` → "~N writes lost".
    public func divergenceDetailText(_ row: FfiRestoreDivergenceRow) -> String {
        let mua = (row.muaId?.isEmpty == false) ? row.muaId! : L.backups.restoreDivergenceUnknownMua
        return L.backups.restoreDivergenceDetailRow(
            collection: row.collection,
            mua: mua,
            client: String(row.clientModseq),
            server: String(row.serverModseq),
            lost: String(row.lostEventCount))
    }

    /// Locale-formatted completed-at (epoch seconds → medium date + short time).
    static func formatWhen(_ secs: Int64) -> String {
        ValueFormat.absoluteDate(epochMs: secs * 1000, withTime: true)
    }
}

/// `restore-progress` state — the 3-value machine android uses (no sub-steps):
/// idle → running → done. The done text tells the user to restart the bridge
/// (the restore is applied server-side; the bridge re-reads the restored tables
/// on restart — `backup-restore.md` § 6).
public enum RestoreProgress {
    case idle, running, done

    public var text: String {
        switch self {
        case .idle: return L.backups.restoreProgressIdle
        case .running: return L.backups.restoreProgressRunning
        case .done: return L.backups.restoreProgressDone
        }
    }
}
