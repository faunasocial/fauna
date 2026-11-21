import SwiftUI

/// The Profile's **private section** on another person's profile — the viewer's
/// own nickname, notes and labels on them (`profile.md` § The private section).
///
/// Glue over the shared `FfiOverlayEditor`: the staging (untouched fields follow
/// the live overlay, the first edit snapshots the baseline), the changed-register
/// diff, the bounds and every refusal text are shared Rust. This class owns the
/// text the fields show and nothing else. The apple twin of android's
/// `ProfilePrivateVM`.
@MainActor @Observable
public final class ProfilePrivateVM {
    /// What the section shows: the staged edits once editing began, else the
    /// live overlay.
    public private(set) var form = OverlayForm(nickname: "", notes: "", labels: [])

    /// The section's refusal or failure, for the page's `error-message`; `nil`
    /// clears it.
    public private(set) var errorMessage: String?

    /// A Save is in flight (the save button disables).
    public private(set) var isSaving = false

    /// `profile-private-save-button`'s answer — the shared
    /// `contact_overlays::save_possibility` over this seat (`profile.md`
    /// § Errors & edge cases): still loading until the account store has
    /// assembled, blocked `profile.private_save_elsewhere` on a seat hosting
    /// no account runtime. Re-asked at every ``reload()``; the editor's save
    /// refuses with the same answer.
    public private(set) var saveAnswer: Possibility = readPossibility(read: .pending)

    /// One editor per Profile open; `nil` while no other person is shown.
    private var editor: FfiOverlayEditor?

    public init() {}

    /// Begin a fresh open of one person's profile: a new editor (so nothing
    /// staged for the last person carries over) and a clean error.
    public func open(editor: FfiOverlayEditor) {
        self.editor = editor
        errorMessage = nil
        isSaving = false
        reload()
    }

    /// Drop the editor with the session or the person it was for.
    public func reset() {
        editor = nil
        errorMessage = nil
        isSaving = false
        saveAnswer = readPossibility(read: .pending)
        form = OverlayForm(nickname: "", notes: "", labels: [])
    }

    /// Re-read the form: an untouched section follows a sibling device's edit.
    public func reload() {
        guard let editor else { return }
        let live = editor.form()
        if live != form { form = live }
        let answer = editor.savePossibility()
        if answer != saveAnswer { saveAnswer = answer }
    }

    public func setNickname(_ value: String) {
        editor?.setNickname(value: value)
        reload()
    }

    public func setNotes(_ value: String) {
        editor?.setNotes(value: value)
        reload()
    }

    /// Stage the typed label; `true` when it was staged (the add field then
    /// empties), `false` when it was refused (the refusal shows, the text stays).
    @discardableResult
    public func addLabel(_ raw: String) -> Bool {
        guard let editor else { return false }
        let refusal = editor.addLabel(raw: raw)
        show(refusal)
        reload()
        return refusal == nil
    }

    public func removeLabel(at index: Int) {
        guard index >= 0 else { return }
        editor?.removeLabel(index: UInt32(index))
        reload()
    }

    /// One Save for everything staged. A refusal keeps the staged edits on
    /// screen.
    public func save() async {
        guard let editor, !isSaving else { return }
        // Rule 6 is the shared save's own: `save_staged` asks the same
        // `save_possibility` the button was painted from and refuses with its
        // reason (`notReady` below) — nothing staged is a save either way.
        isSaving = true
        let outcome = await editor.save()
        // The page moved to another person (or signed out) while this was in
        // flight: the outcome is about an editor this section no longer shows.
        guard self.editor === editor else { return }
        switch outcome {
        case .saved: show(nil)
        case .refused(let text), .notReady(let text), .failed(let text): show(text)
        }
        isSaving = false
        reload()
    }

    private func show(_ text: LocalizedText?) {
        errorMessage = text.map(renderLocalizedText)
    }
}
