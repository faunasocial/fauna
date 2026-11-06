// Excised from the Fauna Kids build — the kids FFI flavor carries no
// `archive-import` feature.
#if !FAUNA_KIDS
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it — and required
// for correctness: the two setters are `test-helpers` UniFFI exports.
#if DEBUG

/// `archive_import_pause_after` / `archive_import_hold_after` — arm the mounted
/// archive-import page's machine to pause (or hold, until the user's own Pause
/// or Cancel) after `records` settled records: the restart-resume and
/// pause/cancel journeys' causal anchors (convention 14). Twins of tui's arms
/// (`apps/fauna-tui/src/automation.rs`) and windows' `TestAgent`. One
/// implementation for macOS + iOS, like `FeedReloadHoldTestCommand`.
public enum ArchiveImportTestCommand {
    /// Returns the refusal sentence, or `nil` once armed — convention 11: with
    /// no page mounted, or a bad payload, the command is refused loudly.
    @MainActor
    public static func apply(_ action: String, _ command: [String: Any]) -> String? {
        guard let records = (command["records"] as? NSNumber)?.uint64Value else {
            return "\(action): needs a numeric `records`"
        }
        guard let vm = ArchiveImportVM.currentForTest,
              vm.armTestAnchor(hold: action == "archive_import_hold_after", records: records)
        else {
            return "\(action): no archive-import page holds a machine"
        }
        return nil
    }
}

#endif
#endif // !FAUNA_KIDS
