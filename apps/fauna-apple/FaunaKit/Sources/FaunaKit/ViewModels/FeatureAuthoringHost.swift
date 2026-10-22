import SwiftUI

/// ONE host's state machine for the shared `feature-policy-editor` family
/// (`docs/goal/architecture/dynamic-features.md` § Authoring surfaces) — the
/// Swift shell around `FfiPolicyEditor` that the admin Nest page, the Status
/// feature-limits rows and the Family page's guardian section each hold,
/// differing only in the two closures that name their tier. Twin of windows'
/// `FeatureAuthoringHost`, android's, linux's `feature_editor.rs` `HostState`
/// and web's host wiring.
///
/// **This decides nothing.** The draft, the cell list and its order, the
/// labels, the no-effect notes, whether *Remove limit* renders and the parser
/// behind *Save* are all the shared editor's; this class holds the handle,
/// forwards keystrokes and re-reads `view()` after each change.
@MainActor @Observable
public final class FeatureAuthoringHost {
    // null until the authored read answers — a summary painted before that
    // would say "No limit set" with no basis.
    public private(set) var rows: [FfiAuthoredRow]?
    /// The worded reason the authored read failed.
    public private(set) var readError: String?
    /// The member the open editor is over, or nil when none is open.
    public private(set) var openFeature: String?
    /// The shared editor's derived view; set exactly while `openFeature` is.
    public private(set) var view: FfiPolicyEditorView?
    /// `feature-policy-editor-status` — present only after a write.
    public private(set) var status: String?
    /// A refused cell or a failed write, worded — the page's `error-message`.
    public private(set) var error: String?

    private let readRows: () async throws -> [FfiAuthoredRow]
    private let openEditor: (String) async throws -> FfiPolicyEditor
    private let onWritten: (() async -> Void)?
    private var editor: FfiPolicyEditor?
    private var working = false

    public init(
        readRows: @escaping () async throws -> [FfiAuthoredRow],
        openEditor: @escaping (String) async throws -> FfiPolicyEditor,
        // A write landed (the editor has already re-seeded): the owner
        // re-reads whatever else shows the same plane (the Status row's
        // transparency cells).
        onWritten: (() async -> Void)? = nil
    ) {
        self.readRows = readRows
        self.openEditor = openEditor
        self.onWritten = onWritten
    }

    /// The authored row for `feature`, once the read has answered.
    public func row(for feature: String) -> FfiAuthoredRow? {
        rows?.first { $0.feature == feature }
    }

    /// Read this tier's authored documents. An open editor is kept.
    public func refresh() async {
        do {
            rows = try await readRows()
            readError = nil
        } catch {
            rows = nil
            readError = L.features.editorLoadFailed(error: Self.reason(error))
        }
    }

    private static func reason(_ error: Error) -> String {
        DisplayError.message(error) ?? error.localizedDescription
    }

    /// Forget everything (the guardian host's ward changed): an editor is one
    /// ward's, and rows for another ward must not flash before the re-read.
    public func reset() {
        editor = nil
        rows = nil
        readError = nil
        openFeature = nil
        view = nil
        status = nil
        error = nil
    }

    /// Open the editor over `feature`, seeded from the tier's AUTHORED document.
    public func open(_ feature: String) async {
        do {
            let opened = try await openEditor(feature)
            editor = opened
            openFeature = feature
            view = opened.view()
            status = nil
            error = nil
        } catch {
            self.error = L.features.editorLoadFailed(error: Self.reason(error))
        }
    }

    /// `feature-policy-editor-on-radio` (`true`) / `-off-radio`.
    public func setOn(_ on: Bool) {
        guard let editor else { return }
        editor.setOn(on: on)
        view = editor.view()
    }

    /// `feature-policy-editor-cell-input[index]` — one keystroke's new text.
    public func setCell(_ index: Int, _ text: String) {
        guard let editor else { return }
        editor.setCell(index: UInt32(index), text: text)
        view = editor.view()
    }

    /// `feature-policy-editor-save-button`.
    public func save() async { await write(remove: false) }

    /// `feature-policy-editor-remove-button`.
    public func remove() async { await write(remove: true) }

    /// `feature-policy-editor-cancel-button` — closes the editor, touching nothing.
    public func cancel() {
        editor = nil
        openFeature = nil
        view = nil
        status = nil
        error = nil
    }

    /// Save (or, with `remove`, clear) the open editor's document. The shared
    /// parser decides: a cell it cannot read dispatches nothing and its reason
    /// lands on the error banner, exactly as on tui.
    private func write(remove: Bool) async {
        guard let editor, !working else { return }
        working = true
        defer { working = false }
        error = nil
        do {
            let verdict = remove ? try await editor.remove() : try await editor.save()
            if let invalid = verdict.invalid {
                error = renderLocalizedText(invalid)
                return
            }
            view = editor.view()
            status = verdict.status.map { renderLocalizedText($0) }
            await refresh()
            if let onWritten { await onWritten() }
        } catch {
            self.error = L.features.editorSaveFailed(error: Self.reason(error))
        }
    }
}

/// A feature-policy editor cell's no-effect note, fully composed (nil when the
/// cell has none) — the Swift twin of shared Rust `editor_note_text`. When the
/// cell carries `noteMagnitude` (a volume ceiling, itself a localized
/// magnitude) it is resolved FIRST and substituted into the note's `{limit}`
/// hole; otherwise `{limit}` is already a finished number.
func editorNoteText(_ cell: FfiEditorCell) -> String? {
    guard let note = cell.note else { return nil }
    guard let magnitude = cell.noteMagnitude else { return renderLocalizedText(note) }
    var args = note.args
    args["limit"] = renderLocalizedText(magnitude)
    return renderLocalizedText(LocalizedText(key: note.key, args: args))
}
