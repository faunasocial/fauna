import Foundation

/// The shape a mailbox row needs to share the toggle-then-echo idiom
/// `MailExportView`/`MailImportView` each carried its own byte-identical copy
/// of (their own doc comments already cross-reference each other — "same
/// idiom, same reason"): both `ToggleMailbox` actions are pure client actions
/// in their respective shared machines (`apply_client_action`), so a
/// synchronous local echo can never disagree with the eventual dispatch
/// outcome. Retroactively conformed below on the two UniFFI-generated
/// snapshot row types — this protocol exists only so the two views' toggle/
/// read functions have one implementation instead of two hand-kept-in-sync
/// ones.
protocol MailboxToggleOption {
    var name: String { get }
    var selected: Bool { get }
}

extension MailboxOption: MailboxToggleOption {}
extension SourceMailboxOption: MailboxToggleOption {}

/// Flips the local echo for `name`, falling back to the snapshot's current
/// value the first time a row is touched. Call BEFORE dispatching the
/// action — the local echo must be visible synchronously, before the
/// `async` dispatch's own re-read catches up.
func toggleMailboxPending<T: MailboxToggleOption>(
    _ name: String, in mailboxes: [T]?, pendingSelection: inout [String: Bool]
) {
    let now = pendingSelection[name] ?? (mailboxes?.first { $0.name == name }?.selected ?? false)
    pendingSelection[name] = !now
}

/// The local echo wins over the snapshot until the dispatch's re-read
/// arrives.
func isMailboxSelected<T: MailboxToggleOption>(_ mailbox: T, pendingSelection: [String: Bool]) -> Bool {
    pendingSelection[mailbox.name] ?? mailbox.selected
}
