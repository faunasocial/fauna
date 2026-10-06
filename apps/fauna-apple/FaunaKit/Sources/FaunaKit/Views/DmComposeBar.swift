import SwiftUI
import UniformTypeIdentifiers

/// Inline compose bar at the bottom of the conversations detail view —
/// `dm-compose-bar` (inline reply) and the tail of `dm-compose-form`
/// (new-thread). Same compose grammar everywhere; affordances are *disabled*
/// (never hidden, never rail-branched) per `ThreadCapabilities`. Shared by the
/// macOS + iOS conversations views.
///
/// Element IDs: `dm-reply-preview` + `dm-reply-cancel` (when replying),
/// `subject-input` (when topic expanded), `dm-text-field`, `topic-toggle-button`,
/// `markdown-toolbar` (+ `markdown-bold-button`, `markdown-italic-button`,
/// `markdown-code-button`, `markdown-link-button`), `attachment-button`,
/// `dm-send-button`.
public struct DmComposeBar: View {
    public let compose: ComposeState
    /// nil for new-thread compose (rail not locked yet → all-enabled).
    public let capabilities: ThreadCapabilities?
    /// Snippet of the message being replied to, if any.
    public var replyPreviewText: String?

    public var onBodyChange: (String) -> Void
    public var onSubjectChange: (String) -> Void
    public var onToggleTopic: () -> Void
    public var onCancelReply: () -> Void
    public var onAddReplyRecipient: (String) -> Void
    public var onRemoveReplyRecipient: (TypedAddress) -> Void
    /// Stage the user-picked file. Receives the picked URL **with its
    /// security scope already open** — the caller stages synchronously and the
    /// scope is closed as soon as it returns, so a caller must not defer the
    /// read past its own body. Reports a user-facing reason on failure rather
    /// than dropping silently (see the button's own note below).
    public var onAttachFile: (URL) throws -> Void
    public var onAttachError: (String) -> Void
    /// Unstage the staged attachment at this position in `compose.attachments`
    /// (`dm-compose-attachment-remove`). Positional, matching the shared
    /// `remove_attachment(thread_id, index)` / `remove_new_thread_attachment
    /// (index)` seam the caller routes to.
    public var onRemoveAttachment: (Int) -> Void
    public var onSend: () -> Void

    public init(
        compose: ComposeState,
        capabilities: ThreadCapabilities?,
        replyPreviewText: String? = nil,
        onBodyChange: @escaping (String) -> Void = { _ in },
        onSubjectChange: @escaping (String) -> Void = { _ in },
        onToggleTopic: @escaping () -> Void = {},
        onCancelReply: @escaping () -> Void = {},
        onAddReplyRecipient: @escaping (String) -> Void = { _ in },
        onRemoveReplyRecipient: @escaping (TypedAddress) -> Void = { _ in },
        onAttachFile: @escaping (URL) throws -> Void = { _ in },
        onAttachError: @escaping (String) -> Void = { _ in },
        onRemoveAttachment: @escaping (Int) -> Void = { _ in },
        onSend: @escaping () -> Void = {}
    ) {
        self.compose = compose
        self.capabilities = capabilities
        self.replyPreviewText = replyPreviewText
        self.onBodyChange = onBodyChange
        self.onSubjectChange = onSubjectChange
        self.onToggleTopic = onToggleTopic
        self.onCancelReply = onCancelReply
        self.onAddReplyRecipient = onAddReplyRecipient
        self.onRemoveReplyRecipient = onRemoveReplyRecipient
        self.onAttachFile = onAttachFile
        self.onAttachError = onAttachError
        self.onRemoveAttachment = onRemoveAttachment
        self.onSend = onSend
    }

    /// Bracket the security-scoped read of a user-picked file and route every
    /// failure to `onAttachError`.
    ///
    /// A user-picked file is security-scoped on both platforms (sandboxed apple
    /// apps), so the read must sit between `start`/`stopAccessingSecurityScoped
    /// Resource` — the same dance `ComposeAttachButton` and `CalendarListView`'s
    /// ICS import do. Staging is synchronous (the manager hashes + caches
    /// in-memory), so unlike the feed's async upload the scope can close on
    /// return rather than at the end of a `Task`.
    private func handleAttachResult(_ result: Result<URL, Error>) {
        switch result {
        case .success(let url):
            guard url.startAccessingSecurityScopedResource() else {
                onAttachError(L.media.uploadFailed)
                return
            }
            defer { url.stopAccessingSecurityScopedResource() }
            do {
                try onAttachFile(url)
            } catch {
                onAttachError("\(L.media.uploadFailed): \(error)")
            }
        case .failure(let error):
            // Cancelling the panel is not an error — banner-ing it would punish
            // the ordinary "changed my mind" path. Everything else must surface.
            guard (error as? CocoaError)?.code != .userCancelled else { return }
            onAttachError("\(L.media.uploadFailed): \(error)")
        }
    }

    @State private var body_: String = ""
    @State private var subject: String = ""
    @State private var recipientDraft: String = ""
    /// Caret/selection in the body field (UTF-16), two-way bound to the
    /// `MarkdownComposeField` so the markdown toolbar wraps the *current* selection.
    @State private var bodySelection = NSRange(location: 0, length: 0)
    /// `markdown-marker-toggle-button` state: `false` (the ratified default) HIDES the
    /// inline emphasis markers — concealed with a caret-edge reveal via the shared
    /// `composeDecorationPlan`; `true` shows them DIMMED (the whole-line-reveal live
    /// preview via `composeShowMarkersDimRanges`). Per-editor, client-local, **no
    /// persistence and no global setting** (`conversations.md` § Compose-field inline
    /// markdown styling — the 2026-06-28 marker-default flip). Each `DmComposeBar`
    /// instance owns its own flag, so the new-thread composer and an open thread's reply
    /// bar toggle independently, matching web/linux/windows/android.
    @State private var markersShown = false
    @State private var showAttachImporter = false
    /// The door onto the real text view behind `dm-text-field` — its applied-styling
    /// read and its caret keys (`MarkdownFieldHandle`), which only the live view answers.
    @State private var bodyField = MarkdownFieldHandle()

    private var supportsAttachments: Bool { capabilities?.supportsAttachments ?? true }
    private var supportsMarkdown: Bool { capabilities?.supportsMarkdown ?? true }
    private var supportsSubject: Bool { capabilities?.supportsSubject ?? true }
    /// The editable reply "To" line shows only on rails that select recipients
    /// (mail). nil capabilities = new-thread compose, which uses the
    /// recipient-picker instead, so the To line stays hidden there.
    private var supportsRecipientSelection: Bool { capabilities?.supportsRecipientSelection ?? false }
    private var sending: Bool { if case .sending = compose.sendState { return true } else { return false } }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // Quoted-reply preview
            if compose.replyTo != nil {
                HStack(spacing: 6) {
                    Image(systemName: "arrowshape.turn.up.left")
                        .font(.caption2).foregroundStyle(.secondary)
                    automationText(Ids.dmReplyPreview, replyPreviewText ?? "")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    Spacer()
                    Button {
                        onCancelReply()
                    } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier(Ids.dmReplyCancel)
                        .automationActivate(Ids.dmReplyCancel) { onCancelReply() }
                }
                .padding(6)
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 6))
            }

            // Editable reply "To" line — always visible on rails that select
            // recipients (mail). One removable chip per `compose.replyRecipients`
            // (seeded by reply / reply-all, then editable); the trailing field
            // adds one. Removing a chip drops it from THIS reply only — thread
            // history is untouched (`conversations.md` § Participants vs reply
            // recipients).
            if supportsRecipientSelection {
                replyToLine
            }

            // Staged attachments — one removable chip per `compose.attachments`
            // entry. Renders above the body field so what is attached reads as
            // part of the message being written, and disappears as soon as the
            // send clears the drafts. Until 2026-07-20 nothing on any client
            // rendered these, so a user attached a file and got no feedback at
            // all until the message sent (`conversations.md` § Attachments).
            if !compose.attachments.isEmpty {
                attachmentChips
            }

            // Subject input — only when "+ topic" is expanded.
            if compose.subjectDraft != nil {
                TextField(L.conversations.unified.topicInputPlaceholder, text: $subject)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.subjectInput)
                    .automationField(Ids.subjectInput, text: $subject)
                    .onChange(of: subject) { _, newValue in onSubjectChange(newValue) }
            }

            // Body — `dm-text-field`. An NSTextView/UITextView-backed field
            // (`MarkdownComposeField`) that live-decorates the buffer with inline markdown
            // styling; the buffer still holds literal markdown source (`conversations.md`
            // § Compose-field inline markdown styling). `markersShown` picks the marker
            // mode (hidden by default). Its `selectedRange` makes the toolbar wrap below
            // selection-aware.
            MarkdownComposeField(
                text: $body_,
                selection: $bodySelection,
                placeholder: L.conversations.compose.writeMessage,
                markersShown: markersShown,
                handle: bodyField
            )
            // `dm-text-field`'s a11y id lives on the underlying NSTextView/UITextView
            // (set in the representable); register the in-process field over the same
            // `$body_` storage the wrapper edits. Registry writes mutate `$body_`, so
            // the `.onChange(of: body_)` below still drives `onBodyChange`.
            //
            // The write goes through an explicit binding rather than `$body_` so it also
            // lands the CARET at the end of the new text — where a user's typing leaves
            // it. Without that the caret stays wherever it was (offset 0 for a fresh
            // composer), and the shared caret-edge reveal would un-conceal the very run a
            // test just typed: `**bold**` typed at caret 0 satisfies
            // `caret >= run_start && caret <= run_end`, so the markers would reveal rather
            // than hide. Convention 11's fidelity rule applied to `/element/type`: the
            // agent's write must leave the state a keystroke would.
            .automationField(
                Ids.dmTextField,
                text: Binding(
                    get: { body_ },
                    set: { newValue in
                        body_ = newValue
                        bodySelection = NSRange(location: (newValue as NSString).length, length: 0)
                        // …and push into the VM SYNCHRONOUSLY. `.onChange(of: body_)`
                        // below is the funnel for every writer of `body_` (the real
                        // text view via `$body_`, restore, the markdown toolbar), but
                        // SwiftUI runs it on its NEXT update pass — so after
                        // `/element/type` returns, the field holds the text and the
                        // manager does not yet. A test that types and immediately
                        // drives something model-derived (the leave-flush doors read
                        // `draftsSnapshotBytes()`) then races that pass and fails
                        // nondeterministically — MEASURED as pass/fail/pass on three
                        // identical `test_apple_leave_flush.py` ios runs.
                        //
                        // This is convention 11's fidelity rule, the same one the
                        // caret write above cites: the agent's write must leave the
                        // state a keystroke would, and a keystroke's does reach the VM
                        // before the app goes anywhere. Only the AUTOMATION path is
                        // affected — a real keystroke writes through `$body_` and keeps
                        // its ordinary one-pass propagation. `.onChange` still fires
                        // afterwards with the same value; `setNewThreadBody`/
                        // `setComposeBody` are idempotent, so the repeat is a cheap
                        // re-arm of the debounce, never a second edit.
                        onBodyChange(newValue)
                    }
                ),
                visibleText: {
                    MarkdownDecorator.visibleText(
                        source: body_,
                        caretLocation: bodySelection.location,
                        markersShown: markersShown
                    )
                },
                textRuns: { bodyField.textRuns() },
                pressKey: { bodyField.pressKey($0) }
            )
            .onChange(of: body_) { _, newValue in onBodyChange(newValue) }

            // Toolbar row
            HStack(spacing: 8) {
                Button(L.conversations.unified.topicToggleAdd) { onToggleTopic() }
                    .buttonStyle(.borderless)
                    .disabled(!supportsSubject)
                    .accessibilityIdentifier(Ids.topicToggleButton)
                    .automationActivate(Ids.topicToggleButton, isEnabled: { supportsSubject }) { onToggleTopic() }

                markdownToolbar

                // `attachment-button` — opens the platform's native file picker
                // and hands the chosen URL to `onAttachFile`, which stages it via
                // `ConversationsManager.add_attachment` /
                // `add_new_thread_attachment` (`conversations.md:621`).
                //
                // **No automation `activate` is registered, deliberately** — the
                // same call the feed's `ComposeAttachButton` makes, for the same
                // reason: the real action is an OS-owned panel no in-process agent
                // can drive (`drivers/http_bridge.py::set_input_files` — "Native
                // bridges (AT-SPI, FlaUI, Apple) can't control file picker
                // dialogs"), so a handler could only hang the harness on a modal
                // or quietly no-op. Until 2026-07-20 this button registered an
                // `activate` over an empty closure at both call sites, so a
                // harness click returned `{"ok": true}` while nothing happened —
                // verbatim the silent drop e2e rule 11 forbids, and invisible to
                // every test because no e2e drove it. The element stays *readable*
                // via `automationValue` (so the affordance is assertable), and a
                // stray click now fails loudly as a 404. The e2e attach path is
                // the `compose.file` state-injection command carrying
                // `target: "attachment-button"`, which calls the very same
                // `onAttachFile` seam — mirroring linux's disambiguation seam.
                Button {
                    showAttachImporter = true
                } label: { Image(systemName: "paperclip") }
                    .buttonStyle(.borderless)
                    .disabled(!supportsAttachments)
                    .help(L.conversations.unified.attachmentButton)
                    .accessibilityIdentifier(Ids.attachmentButton)
                    .automationValue(Ids.attachmentButton, isEnabled: { supportsAttachments })
                    .fileImporter(
                        isPresented: $showAttachImporter,
                        // Any file, matching every sibling that ships this:
                        // windows `FileTypeFilter.Add("*")`, web's unfiltered
                        // `<input type="file">`, linux's unfiltered `FileDialog`.
                        // A conversation attachment is not photo-only (priority #4).
                        allowedContentTypes: [.item]
                    ) { result in
                        handleAttachResult(result)
                    }

                Spacer()

                Button(sending ? L.common.sending : L.common.send) { onSend() }
                    .buttonStyle(.borderedProminent)
                    .disabled(sending || body_.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier(Ids.dmSendButton)
                    // Live re-read of the same predicate the `.disabled(...)` uses.
                    .automationActivate(
                        Ids.dmSendButton,
                        isEnabled: { !(sending || body_.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
                    ) { onSend() }
            }
        }
        .onAppear {
            body_ = compose.bodyDraft
            subject = compose.subjectDraft ?? ""
        }
        .onChange(of: compose.bodyDraft) { _, newValue in if newValue != body_ { body_ = newValue } }
        .onChange(of: compose.subjectDraft) { _, newValue in
            let s = newValue ?? ""
            if s != subject { subject = s }
        }
    }

    // Staged-attachment chips: icon · filename · size · remove (×).
    //
    // Structurally `replyToLine`'s chip row (the established removable-indexed-
    // chip idiom in this same bar), over `compose.attachments` instead of
    // `compose.replyRecipients`. `AttachmentDraft` is light by design — no
    // bytes — so everything rendered here is metadata already in the snapshot;
    // no attachment-store round-trip, and a multi-MB image costs nothing to
    // display.
    //
    // The size label routes through the shared `fauna_core::format::byte_size`
    // (`ValueFormat.byteSize`), never a native `ByteCountFormatter` or a
    // hand-rolled threshold table — `value-formatting.md` § Byte sizes owns
    // that decision for all 7 apps (priority #2).
    private var attachmentChips: some View {
        // Plain `HStack`, the same shape `replyToLine` uses for its chip row a
        // few lines down (priority #3 — reuse this bar's own established idiom
        // rather than introduce a wrapping `Layout` for one row). Each chip
        // bounds its own width via `lineLimit(1)` + middle truncation, so a long
        // filename compresses instead of pushing the remove control off-screen.
        HStack(spacing: 6) {
            ForEach(Array(compose.attachments.enumerated()), id: \.offset) { index, draft in
                HStack(spacing: 4) {
                    Image(systemName: draft.isImage ? "photo" : "doc")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    // One element carrying filename + size: the chip is a single
                    // informational unit, so a test reads one id rather than
                    // correlating two positional ones.
                    automationText(
                        Ids.dmComposeAttachmentChip,
                        "\(draft.filename)  \(ValueFormat.byteSize(draft.sizeBytes))"
                    )
                    .font(.caption2)
                    .lineLimit(1)
                    .truncationMode(.middle)

                    Button {
                        onRemoveAttachment(index)
                    } label: { Image(systemName: "xmark.circle.fill").font(.caption2) }
                        .buttonStyle(.borderless)
                        .help(L.conversations.unified.attachmentRemove)
                        .accessibilityIdentifier(Ids.dmComposeAttachmentRemove)
                        .automationActivate(Ids.dmComposeAttachmentRemove) { onRemoveAttachment(index) }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 3)
                .background(Color.secondary.opacity(0.15), in: Capsule())
            }
        }
    }

    // Editable reply "To" line: "To:" label · removable chips · add field.
    // Mirrors linux `compose_bar.rs` (the GTK reference impl). Chips render off
    // `compose.replyRecipients`; the field parses + appends through the manager.
    private var replyToLine: some View {
        HStack(spacing: 6) {
            Text(L.conversations.unified.toLineLabel)
                .font(.caption).foregroundStyle(.secondary)

            ForEach(Array(compose.replyRecipients.enumerated()), id: \.offset) { _, addr in
                HStack(spacing: 2) {
                    automationText(Ids.dmReplyRecipientChip, ConversationsUI.display(addr))
                        .font(.caption2)
                    Button {
                        onRemoveReplyRecipient(addr)
                    } label: { Image(systemName: "xmark.circle.fill").font(.caption2) }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier(Ids.dmReplyRecipientRemove)
                        .automationActivate(Ids.dmReplyRecipientRemove) { onRemoveReplyRecipient(addr) }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Color.secondary.opacity(0.15), in: Capsule())
            }

            TextField(L.conversations.unified.replyRecipientAddPlaceholder, text: $recipientDraft)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.dmReplyRecipientAdd)
                .automationField(Ids.dmReplyRecipientAdd, text: $recipientDraft)
                .onSubmit {
                    let text = recipientDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    onAddReplyRecipient(text)
                    recipientDraft = ""
                }
        }
    }

    // Markdown toolbar — client glue (text splice), capability-gated as a group.
    // The wrap *rule* is shared (`fauna_core::markdown::wrap_selection` via the
    // `wrapMarkdownSelection` UniFFI face); `MarkdownCompose.wrap` runs it over the body
    // field's **current selection** (now reachable: `MarkdownComposeField` backs
    // `dm-text-field` with an NSTextView/UITextView that exposes `selectedRange`), so a
    // word-selection's edge whitespace stays outside the markers and an empty selection
    // inserts the wrapped placeholder at the caret. (The standalone `MarkdownToolbar` view
    // drives the feed composer's toolbar with the same IDs over a selection-less field —
    // it still uses the empty-selection `MarkdownCompose.insertion`; folding the two is a
    // follow-on dedup.)
    private var markdownToolbar: some View {
        HStack(spacing: 4) {
            mdButton("bold", "markdown-bold-button", "**", "**")
            // Italic uses `*` (not `_`) to stay uniform with linux/web/android and
            // with the asterisk-family bold above; the shared renderer accepts both.
            mdButton("italic", "markdown-italic-button", "*", "*")
            mdButton("chevron.left.forwardslash.chevron.right", "markdown-code-button", "`", "`")
            mdButton("link", "markdown-link-button", "[", "](url)")
            markerToggleButton
        }
        .disabled(!supportsMarkdown)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.markdownToolbar)
        // Group presence anchor; mirrors the group `.disabled(!supportsMarkdown)`.
        .automationValue(Ids.markdownToolbar, text: { "" }, isEnabled: { supportsMarkdown })
    }

    /// `markdown-marker-toggle-button` — flip THIS editor between concealed inline
    /// markers (the default) and the dimmed live preview. Per-editor, no persistence
    /// (`conversations.md` § Compose-field inline markdown styling).
    ///
    /// The eye / eye-slash pair is android's icon exactly (`Icons.Default.Visibility` /
    /// `VisibilityOff`); web and linux draw a `∗` glyph instead, because their toolbars
    /// are text-labelled throughout ("B", "I", "<>") where apple's and android's are
    /// icon-only. Same id, same default, same behavior — only the glyph follows each
    /// toolbar's own established vocabulary. The label comes from the shared
    /// `markdown.toggle_markers` string (linux uses it as the tooltip, web as the
    /// `title`), so an icon-only control still announces itself.
    ///
    /// The published automation `value` ("on"/"off") makes the current mode diagnosable
    /// without a screenshot — over the wire the toggle otherwise looks like any other
    /// toolbar button.
    private var markerToggleButton: some View {
        Button {
            markersShown.toggle()
        } label: {
            Image(systemName: markersShown ? "eye" : "eye.slash").font(.caption2)
        }
        .buttonStyle(.borderless)
        .accessibilityIdentifier(Ids.markdownMarkerToggleButton)
        .accessibilityLabel(L.markdown.toggleMarkers)
        .automationActivate(
            Ids.markdownMarkerToggleButton,
            isEnabled: { supportsMarkdown },
            value: { markersShown ? "on" : "off" }
        ) {
            markersShown.toggle()
        }
    }

    private func mdButton(_ symbol: String, _ id: String, _ prefix: String, _ suffix: String) -> some View {
        Button {
            applyMarkdown(prefix: prefix, suffix: suffix)
        } label: { Image(systemName: symbol).font(.caption2) }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(id)
            // Same `applyMarkdown` the Button action runs; mirrors the group's
            // `.disabled(!supportsMarkdown)`.
            .automationActivate(id, isEnabled: { supportsMarkdown }) {
                applyMarkdown(prefix: prefix, suffix: suffix)
            }
    }

    /// Wrap the current body selection with `prefix`/`suffix` — the markdown-toolbar
    /// action shared by the `Button` and its `automationActivate`. `body_`'s change
    /// propagates to the VM via the field's `.onChange(of: body_)`.
    private func applyMarkdown(prefix: String, suffix: String) {
        let result = MarkdownCompose.wrap(text: body_, selection: bodySelection, prefix: prefix, suffix: suffix)
        body_ = result.text
        bodySelection = result.selection
    }
}
