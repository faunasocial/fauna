import Foundation

/// The `snapshot-restore-result` line for a finished whole-snapshot restore.
///
/// The wording is the shared `fauna_backups_machine::snapshot_restore_result_text`
/// (the same one tui and linux call): "done" when every file was written, else the
/// partial line naming the files not written. Only the byte-size rendering is
/// app glue (`behavior/value-formatting.md`). A partial result is a result, never
/// an error line (`ui/backups.md` § Whole-snapshot restore → *Outcome*).
public enum SnapshotRestoreText {
    public static func line(_ summary: FfiRestoreSummary, path: String) -> String {
        renderLocalizedText(snapshotRestoreResultText(
            filesRestored: summary.filesRestored,
            size: ValueFormat.byteSize(summary.bytesWritten),
            path: path,
            unverified: summary.unverified
        ))
    }
}
