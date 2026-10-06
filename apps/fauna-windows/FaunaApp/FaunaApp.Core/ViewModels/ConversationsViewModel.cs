using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Thin observer over the shared <see cref="ConversationsManager"/>. All
/// authoritative state lives in Rust; this view-model snapshots it and
/// raises <see cref="INotifyPropertyChanged"/> events for XAML bindings.
/// Mirrors the <see cref="OnboardingViewModel"/> pattern (observer +
/// cached-snapshot + ObservableObject INPC plumbing).
///
/// Constructor takes the manager and the WinUI-side observer (a
/// <c>ConversationsNotifyObserver</c> that bridges callbacks to the UI
/// thread); the view-model registers the observer with the manager and
/// forwards its <see cref="INotifyPropertyChanged"/> events as its own
/// blanket "everything changed" notification.
///
/// Marked <c>internal</c> because the UniFFI-generated types
/// (<see cref="ConversationsManager"/>, <see cref="SnapshotObserver"/>,
/// the snapshot records) are emitted as <c>internal</c>.
/// </summary>
internal partial class ConversationsViewModel : ObservableObject
{
    /// <summary>
    /// Singleton accessor mirroring <c>OnboardingViewModel.Current</c>.
    /// Used by <c>TestAgent</c> to route bridge commands at the manager
    /// without threading the VM through every command-handler call.
    /// </summary>
    internal static ConversationsViewModel? Current { get; private set; }

    private readonly ConversationsManager _manager;
    // Observer held to keep its callback alive for the manager's lifetime.
    private readonly SnapshotObserver _observer;
    private ConversationsSnapshot? _cachedSnapshot;
    // The detail twin of _cachedSnapshot — see SelectedDetail for why this is not a
    // free property read. Keyed by thread id; invalidated at the same observer tick.
    private (string ThreadId, ThreadDetail? Detail)? _cachedDetail;
    // The sealed spam-model client-write path (mail-spam.md § Encrypted-mode
    // interaction — the live Insert consumer, the follow-on to the
    // moderation-queue 1d switch which left history_op: None). Null → no
    // sealed-write path → MarkMessageSpamAsync is a silent no-op (mirrors
    // ModerationViewModel's null-seam convention — mail not enabled / no session).
    private readonly ISpamModelClientWrite? _spamWrite;

    public ConversationsViewModel(
        ConversationsManager manager,
        SnapshotObserver observer,
        ISpamModelClientWrite? spamWrite = null)
    {
        _manager = manager;
        _observer = observer;
        _spamWrite = spamWrite;
        Current = this;

        if (observer is INotifyPropertyChanged inpc)
            inpc.PropertyChanged += (_, _) =>
            {
                _cachedSnapshot = null;
                _cachedDetail = null;
                OnPropertyChanged(string.Empty);
            };

        _manager.AddObserver(observer);
    }

    /// <summary>The shared manager — exposed for direct mutator calls.</summary>
    public ConversationsManager Manager => _manager;

    public ConversationsSnapshot Snapshot => _cachedSnapshot ??= _manager.Snapshot();

    public IReadOnlyList<ThreadSummary> Threads => Snapshot.threads;

    public string? SelectedThreadId => Snapshot.selectedThreadId;

    public ThreadSummary? Selected
    {
        get
        {
            var id = SelectedThreadId;
            return id is null
                ? null
                : Snapshot.threads.FirstOrDefault(t => t.threadId == id);
        }
    }

    /// <summary>The open thread's full detail, memoized for the life of one snapshot
    /// exactly as <see cref="Snapshot"/> is.
    /// <para><c>ThreadDetail</c> is NOT a field read: it re-derives the whole thread in
    /// shared Rust — every message's <c>RenderDocument</c>, rebuilt by the producer — and
    /// marshals the result, bodies and all, back across UniFFI. Measured on Windows
    /// 2026-09-11 (`mail-message-size.md` § Implementation status today): ~130 ms per
    /// access for a 1.5 MB plain-text mail, and <c>ConversationsPage</c> reads this
    /// property several times in a single refresh pass (the right-pane mode switch, the
    /// rename seed, the recipient picker, the room roster), so the same derivation was
    /// paid over and over WITHIN one pass.</para>
    /// Keyed by thread id and cleared wherever <see cref="_cachedSnapshot"/> is — the
    /// observer tick — so it can never outlive a state change or serve the previously
    /// selected thread's detail (<see cref="SelectedThreadId"/> is itself read off the
    /// cached snapshot, so the two caches turn over together).</summary>
    public ThreadDetail? SelectedDetail
    {
        get
        {
            var id = SelectedThreadId;
            if (id is null) return null;
            if (_cachedDetail is { } cached && cached.ThreadId == id)
                return cached.Detail;
            var detail = _manager.ThreadDetail(id);
            _cachedDetail = (id, detail);
            return detail;
        }
    }

    public ComposeState? NewThreadCompose => Snapshot.newThreadCompose;

    public bool IsNewThreadComposeActive => NewThreadCompose is not null;

    public bool IsDetailActive => SelectedDetail is not null && !IsNewThreadComposeActive;

    public bool IsEmptyDetail => !IsNewThreadComposeActive && SelectedDetail is null;

    /// <summary>
    /// The page-level error text (<c>error-message</c>). Reads
    /// <c>EngineServedElsewhere()</c> FIRST — the conversations-engine role-lock
    /// refusal (<c>account-data-plane.md</c> § Multi-instance concurrency, W5.6 (account-data-plane.md § Workstreams)):
    /// a STANDING condition armed at the engine-construction site
    /// (<c>conversations_session_over_manager</c>), never by a page producer, so it
    /// must outrank the truths below rather than be masked by an unrelated
    /// gesture clearing them. Live on windows since its retirement leg
    /// (2026-08-24): a second same-account instance now coexists, and the
    /// non-role-holder's conversations page must say so honestly instead of
    /// rendering an unwired page.
    ///
    /// <para>Then <c>ReceiveStopped</c> — the fourth truth (<c>conversations.md</c>
    /// § Errors &amp; edge cases, 2026-09-13): the shared receive loop's supervisor
    /// died by panic (<c>ConversationsManager::receive_stopped</c>), standing like
    /// the served-elsewhere refusal until a newer loop over the same manager
    /// retires it — no gesture clears it either. Mirrors apple's
    /// <c>ConversationsVM.pageError</c> and tui's <c>sync_page_error</c>.</para>
    ///
    /// <para>Then a failed membership/label wire op (<c>Snapshot.error</c> —
    /// <c>confirm_add_participant</c>/<c>remove_participant</c>/<c>rename_thread</c>)
    /// takes precedence over a failed compose-send, mirroring linux's
    /// <c>page_error_text</c> (<c>views/conversations/detail.rs</c>), apple's
    /// <c>ConversationsVM.pageError</c> and tui's <c>sync_page_error</c>
    /// (<c>conversations.md</c> § Errors &amp; edge cases).
    /// Every producer — sends included — clears <c>Snapshot.error</c> on entry, so
    /// it is always the more recent of the two by construction; do not reverse the
    /// precedence. The active compose is the new-thread compose when present, else
    /// the selected thread's. No client-side state machine (architectural rule 1);
    /// both carriers are shared <c>LocalizedText</c>, resolved through the windows
    /// pipeline exactly as every other shared-Rust label is (architectural rule 3 —
    /// "never hardcode English").</para>
    ///
    /// <para>Last, the floor of the stack — the fifth truth (<c>conversations.md</c>
    /// § Errors &amp; edge cases, 2026-09-15): mail the receive path skipped because
    /// it could not open it (<see cref="UnopenableMailCount"/>). Ranked strictly
    /// BELOW every truth above, so a fresh failure of any gesture, a dead rail or
    /// the role refusal all outrank it — it is a floor, never a mask — and no gesture
    /// clears it: only the records opening after all retire their entries. The count
    /// is substituted through <c>Strings.Format</c>, this app's generated-resw
    /// formatter (never a hand <c>.Replace</c>). One arm covers both panes because
    /// the page's <c>error-message</c> is page-level (published once from
    /// <c>ConversationsPage.Refresh</c>).</para>
    /// </summary>
    public string? ActiveSendErrorReason
    {
        get
        {
            if (EngineServedElsewhere) return Strings.Get("conversations/errors/served_elsewhere");
            if (ReceiveStopped) return Strings.Get("conversations/errors/receive_stopped");
            if (Snapshot.@error is { } pageError) return Strings.Resolve(pageError);
            if ((NewThreadCompose?.sendState ?? SelectedDetail?.compose.sendState)
                is SendState.Failed failed)
                return Strings.Resolve(failed.reason);
            // The floor of the stack — last, strictly below every truth above.
            var skipped = UnopenableMailCount;
            return skipped > 0
                ? Strings.Format("conversations/errors/mail_unopenable", skipped)
                : null;
        }
    }

    /// <summary>
    /// Does another same-account instance hold the conversations-engine role lock
    /// over this account's <c>mls_state.db</c>? A read of shared state, not a
    /// client-side flag — the engine-construction seam sets it and clears it
    /// (<c>set_engine_served_elsewhere</c>), so nothing here can drift from what
    /// actually happened at engine init. An FFI fault degrades to <c>false</c>:
    /// the page's own errors must still be reachable if the probe itself breaks.
    /// </summary>
    public bool EngineServedElsewhere
    {
        get
        {
            try
            {
                return _manager.EngineServedElsewhere();
            }
            catch (Exception)
            {
                return false;
            }
        }
    }

    /// <summary>
    /// Did the receive loop currently serving this manager die by panic
    /// (<c>ConversationsManager::receive_stopped</c>, <c>conversations.md</c> §
    /// Errors &amp; edge cases — the fourth truth, 2026-09-13)? A read of shared
    /// state set by the receive loop's supervisor
    /// (<c>supervise_receive_loop</c>/<c>mark_receive_stopped</c>), never by this
    /// VM. An FFI fault degrades to <c>false</c>, same posture as
    /// <see cref="EngineServedElsewhere"/>: the page's own errors must still be
    /// reachable if the probe itself breaks.
    /// </summary>
    public bool ReceiveStopped
    {
        get
        {
            try
            {
                return _manager.ReceiveStopped();
            }
            catch (Exception)
            {
                return false;
            }
        }
    }

    /// <summary>
    /// How many received mail records the receive path skipped because it could not
    /// open them (<c>ConversationsManager::unopenable_mail_count</c>,
    /// <c>conversations.md</c> § Errors &amp; edge cases — the fifth truth,
    /// 2026-09-15; the skip/count/retire rule is <c>mail-app-surface.md</c> § Inbound
    /// client receive → <i>Unopenable records</i>). A read of shared state: the
    /// receive drivers note and retire the entries, never this VM. An FFI fault
    /// degrades to <c>0</c>, same posture as <see cref="EngineServedElsewhere"/> and
    /// <see cref="ReceiveStopped"/>: <see cref="ActiveSendErrorReason"/> runs on
    /// every page refresh tick, so a broken probe must read as "nothing skipped"
    /// rather than take the page-level error read down with it.
    /// </summary>
    public uint UnopenableMailCount
    {
        get
        {
            try
            {
                return _manager.UnopenableMailCount();
            }
            catch (Exception)
            {
                return 0;
            }
        }
    }

    // ── Action methods (delegate to manager) ─────────────────────

    public void StartNewConversation() => _manager.StartNewConversation();

    public void CancelNewConversation() => _manager.CancelNewConversation();

    public void OpenThread(string id) => _manager.SelectThread(id);

    /// <summary>
    /// The <c>SearchNav.Mail</c> deep-link target: thread jump AND message
    /// selection under one <c>notify</c> (<c>ConversationsManager.
    /// select_thread_and_message</c> — <c>ui/search.md</c> § Where logic
    /// lives → Result navigation (deep link); android's twin,
    /// <c>selectThreadAndMessage</c>). Never <see cref="OpenThread"/> +
    /// a second call: that would paint the thread with no message marked
    /// yet, then flip it a moment later.
    /// </summary>
    public void OpenThreadAndMessage(string threadId, string messageId) =>
        _manager.SelectThreadAndMessage(threadId, messageId);

    public void ClearSelection() => _manager.ClearSelection();

    /// <summary>Filter the thread list by a search query (conversations.md § User actions
    /// → conversation-search-box "Filter list"). Delegates to the shared manager, which
    /// applies the filter in <c>snapshot()</c> (<c>filter_summaries</c> over label +
    /// snippet) and re-emits — so the page re-renders the already-filtered
    /// <c>snapshot().threads</c> directly, with NO client-side filter (priority #3/#4,
    /// matching linux/web/android). A blank/whitespace query clears the filter.</summary>
    public void SetSearchQuery(string? query) => _manager.SetSearchQuery(query);

    /// <summary>Advance <see cref="ConversationsSnapshot.sort"/> one step around the
    /// shared 3-way cycle (conversations.md § Where logic lives → Thread-list sort
    /// cycle). The VM never enumerates the orders itself — it hands the current one
    /// to <c>next_sort_order</c> and feeds the result straight back to the manager.</summary>
    public void CycleSort() => _manager.SetSort(FaunaConversationsMethods.NextSortOrder(Snapshot.sort));

    public void SetComposeBody(string id, string body) => _manager.SetComposeBody(id, body);

    /// <summary>The compose bar's <c>dm-reply-preview</c> text for thread <paramref name="id"/>:
    /// the shared <c>ConversationsManager::reply_preview</c> record rendered as
    /// "{sender}: {excerpt}", the tui/linux/web/apple text. <c>null</c> when no reply is
    /// armed or the answered message is outside the fetched window — the page then hides
    /// the preview (conversations.md § Where logic lives → <i>Reply preview</i>: apps render
    /// the record, never derive it).</summary>
    public string? ReplyPreviewText(string id) =>
        _manager.ReplyPreview(id) is { } preview
            ? $"{preview.senderDisplay}: {preview.excerpt}"
            : null;

    /// <summary>Dispatch <c>load-remote-content-button</c> to the manager (D3): flips the
    /// in-memory reveal set for <paramref name="messageId"/> and re-emits; the next
    /// <c>ThreadDetail</c> projects <c>RemoteImage.revealed=true</c> for this message
    /// so the bubble repaint reads the fetched image (render-model.md § D3;
    /// html-mail.md § Security &amp; privacy). Sync void — no <c>ConfigureAwait</c>
    /// (WinUI VM must not leave the UI thread).</summary>
    public void RevealRemoteImages(string messageId) => _manager.RevealRemoteImages(messageId);

    /// <summary>D4 link-preview (render-model.md § D4): resolve a folded <c>Resolving</c>
    /// <c>LinkPreview</c> block's url via the shared
    /// <c>ConversationsManager.resolve_link_preview</c> — the conversations twin of the feed's
    /// <c>FeedManager::resolve_link_preview</c>. The manager fetches once (cached), maps the
    /// reply onto <c>PreviewState</c>, and re-emits; the next <c>ThreadDetail</c> folds the
    /// <c>Resolved</c> state onto the bubble's document and Refresh repaints the card. Fired
    /// fire-once by the page for each <c>Resolving</c> block (the block becomes terminal after,
    /// so it won't re-fire; the cached, idempotent resolve is the in-flight backstop — mirroring
    /// the feed trigger). No <c>ConfigureAwait(false)</c> (a WinUI VM must resume on the UI
    /// thread; off-thread bound-state mutation throws a silent COMException).</summary>
    public async Task ResolveLinkPreviewAsync(string url) => await _manager.ResolveLinkPreview(url);

    // ── Attachments ──────────────────────────────────────────────
    // conversations.md § Attachments. Render resolves bytes through the shared
    // attachment store; compose stages a draft the backend send re-resolves.

    /// <summary>Resolve an attachment's <c>blob_hash</c> to its plaintext bytes from
    /// the shared attachment store (the bubble decodes images from these). <c>null</c>
    /// for a not-yet-fetched hash.</summary>
    public byte[]? AttachmentBytes(string blobHash) => _manager.AttachmentBytes(blobHash);

    /// <summary>Stage a compose attachment on an existing thread
    /// (<c>attachment-button</c>): the manager hashes + caches the bytes and stages a
    /// light draft that the next <see cref="Send"/> resolves onto the wire.</summary>
    public void AddAttachment(string id, string filename, string mimeType, byte[] bytes) =>
        _manager.AddAttachment(id, filename, mimeType, bytes);

    /// <summary>Stage a compose attachment on the new-thread compose, carried onto
    /// the materialized thread by <see cref="SendNewThread"/>.</summary>
    public void AddNewThreadAttachment(string filename, string mimeType, byte[] bytes) =>
        _manager.AddNewThreadAttachment(filename, mimeType, bytes);

    /// <summary>Unstage a compose attachment on an existing thread
    /// (<c>dm-compose-attachment-remove</c>) by its positional index over
    /// <c>ComposeState.attachments</c> — the same list the chip row enumerates, so
    /// the chip and this call's index cannot drift.</summary>
    public void RemoveAttachment(string id, uint index) =>
        _manager.RemoveAttachment(id, index);

    /// <summary>Unstage a compose attachment on the new-thread compose
    /// (<c>dm-compose-attachment-remove</c>) by its positional index.</summary>
    public void RemoveNewThreadAttachment(uint index) =>
        _manager.RemoveNewThreadAttachment(index);

    public void ToggleTopic(string id) => _manager.ToggleTopic(id);

    public void SetComposeSubject(string id, string subject) => _manager.SetComposeSubject(id, subject);

    // ── Reply recipients (editable "To" line) ────────────────────
    // conversations.md § Participants vs. reply recipients. All three delegate
    // to the shared manager; the snapshot's compose.reply_recipients drives the
    // To-line render. Gated in the view on capabilities.supports_recipient_selection.

    /// <summary>
    /// Seed a reply draft (<c>dm-reply-button</c> → <c>replyAll=false</c>
    /// sender-only; <c>dm-reply-all-button</c> → <c>replyAll=true</c>
    /// every-participant-but-self). On a recipient-selection rail (mail) this
    /// fills <c>compose.reply_recipients</c>; on FaunaMls it only sets
    /// <c>reply_to</c> (recipients ARE the group).
    /// </summary>
    public void StartReply(string id, string msgId, bool replyAll) =>
        _manager.StartReply(id, msgId, replyAll);

    /// <summary>Append a recipient to the editable reply To line
    /// (<c>dm-reply-recipient-add</c>); deduped by the shared manager.</summary>
    public void AddReplyRecipient(string id, TypedAddress addr) =>
        _manager.AddReplyRecipient(id, addr);

    /// <summary>Drop a recipient from this reply only
    /// (<c>dm-reply-recipient-remove</c>); thread membership untouched.</summary>
    public void RemoveReplyRecipient(string id, TypedAddress addr) =>
        _manager.RemoveReplyRecipient(id, addr);

    /// <summary>Cancel the reply (<c>dm-reply-cancel</c>): clears both
    /// <c>reply_to</c> and the editable To line.</summary>
    public void ClearReplyTo(string id) => _manager.SetReplyTo(id, null);

    public AddParticipantState? AddParticipant => Snapshot.addParticipant;

    public void OpenAddParticipant(string id) => _manager.OpenAddParticipant(id);
    public void SetAddParticipantRecipientInput(string text) => _manager.SetAddParticipantRecipientInput(text);
    public void AcceptAddParticipantChip(TypedAddress addr) => _manager.AcceptAddParticipantChip(addr);
    public void CancelAddParticipant() => _manager.CancelAddParticipant();

    /// <summary>
    /// Commit the staged add-participant via the shared manager's wire-propagating
    /// <c>confirm_add_participant</c> (the same async op linux calls natively): it
    /// takes the overlay state, extracts the committed chip, applies the membership
    /// mutation (MLS fork for a 1:1, in-place add otherwise), selects a newly forked
    /// thread, and fires the MLS wire op for a bound FaunaMls-group add. Returns the
    /// (possibly new) thread id, or null if no recipient was committed.
    /// </summary>
    public async Task<string?> ConfirmAddParticipant() => await _manager.ConfirmAddParticipant();

    public bool AcceptCurrentRecipientChip() => _manager.AcceptCurrentRecipientChip();

    /// <summary>
    /// Drive the shared manager's async recipient probe (<c>resolve_recipient</c>)
    /// on whichever picker is active. Typing owes this probe — the sync input
    /// write parks the picker on <c>Resolving</c> and only this call moves it to a
    /// terminal state — and Enter resolves first, then commits what the probe
    /// confirmed (<c>docs/goal/ui/conversations.md</c> § Errors &amp; edge cases →
    /// <i>The picker tells the truth</i>; the same order android, web, linux and tui
    /// drive). The snapshot round-trip carries the resolve state back to the widget.
    /// </summary>
    public async Task ResolveRecipientAsync() => await _manager.ResolveRecipient();

    /// <summary>
    /// Rename a thread via the shared manager's wire-propagating
    /// <c>rename_thread</c> (the same async op linux calls natively): relabels the
    /// snapshot immediately, then posts the encrypted <c>GroupMeta::NameChanged</c>
    /// application message for a bound FaunaMls group so peers apply it.
    /// </summary>
    public async Task RenameThread(string id, string label) => await _manager.RenameThread(id, label);

    /// <summary>
    /// Remove a participant via the shared manager's wire-propagating
    /// <c>remove_participant</c> (<c>thread-member-chip[i]</c>, when
    /// <c>supports_membership_change</c>) — drops the snapshot row and posts
    /// the FaunaMls Commit (no Welcome). A refused op restores the participant
    /// and surfaces on <c>Snapshot.error</c>, same as <see cref="RenameThread"/>
    /// — no try/catch here, the page's existing error surface already reads it.
    /// </summary>
    public async Task RemoveParticipant(string id, TypedAddress addr) =>
        await _manager.RemoveParticipant(id, addr);

    /// <summary>
    /// Commit everything staged in the room policy editor
    /// (<c>room-settings-save-button</c>) — one policy commit per staged change,
    /// the hand-over last, stopping at the first refusal. Returns whether ALL of
    /// them landed, which is the editor's whole close condition
    /// (<c>ui/conversations.md</c> § Element IDs: "closes only when all
    /// landed"). A refusal is already painted on <c>Snapshot.error</c> by the
    /// individual gesture, so <see cref="ActiveSendErrorReason"/> names it with
    /// no try/catch here — same posture as <see cref="RenameThread"/> and
    /// <see cref="RemoveParticipant"/>. No <c>ConfigureAwait(false)</c>: a WinUI
    /// VM must resume on the UI thread (reference_windows_vm_configureawait_comexception).
    /// </summary>
    internal async Task<bool> ApplyRoomSettings(string id, RoomSettingsEdit[] edits) =>
        await _manager.ApplyRoomSettings(id, edits);

    /// <summary>
    /// Walk out of the room <paramref name="id"/> (<c>room-leave-confirm</c>) — the
    /// shared <c>ConversationsManager::leave_room</c>, one verb whose door the
    /// room's class picks in shared Rust (<c>conversation-rooms.md</c> § Roles and
    /// authorization → <i>Leaving — the mechanism</i>). The owner is refused there
    /// with a sentence naming the remedy; every refusal is already on
    /// <c>Snapshot.error</c> (<see cref="ActiveSendErrorReason"/>), so no try/catch
    /// here — the <see cref="RemoveParticipant"/> posture. The thread STAYS in the
    /// list: the user keeps their own copy. No <c>ConfigureAwait(false)</c> — a
    /// WinUI VM must resume on the UI thread.
    /// </summary>
    internal async Task LeaveRoom(string id) => await _manager.LeaveRoom(id);

    /// <summary>
    /// Send the active draft on an existing thread (<c>dm-send-button →
    /// manager.send(thread_id)</c>, <c>conversations.md</c> § User actions). The
    /// shared manager's <c>send</c> runs the rail backend (MLS for FaunaMls),
    /// appends the sent message to the thread, and clears the draft. No
    /// <c>ConfigureAwait(false)</c> — a WinUI VM must resume on the UI thread
    /// (off-thread bound-state mutation throws a silent COMException).
    ///
    /// <para><b>A refusal is a value here, never an exception.</b> The manager
    /// stamps <c>ComposeState.send_state = Failed { reason }</c> — which
    /// <see cref="ActiveSendErrorReason"/> paints — AND returns the same error.
    /// The page reaches this from an <c>async void</c> click handler, so a
    /// rethrow lands on the XAML dispatcher as an unhandled exception: measured
    /// on an over-the-limit send, after which the app ran no later navigation at
    /// all. Swallowed here the way apple's <c>ConversationsVM.send</c> does;
    /// anything that is not the manager's own refusal still propagates.</para>
    /// </summary>
    /// <returns>Whether the message was sent — the page clears the typed text
    /// only then, so a refused draft stays in the field to retry.</returns>
    public async Task<bool> Send(string threadId)
    {
        try
        {
            await _manager.Send(threadId);
            return true;
        }
        catch (BackendException)
        {
            return false;
        }
    }

    /// <summary>
    /// Send the new-thread compose (<c>dm-send-button →
    /// manager.send_new_thread()</c> for new compose, <c>conversations.md</c>
    /// § User actions). The shared manager flushes a typed-but-uncommitted
    /// recipient, materializes the thread, and sends; returns the new thread
    /// id (or null when there is no committed recipient, or the send was
    /// refused — the materialized thread keeps its <c>Failed</c> draft, and
    /// the refusal is on the snapshot exactly as for <see cref="Send"/>). No
    /// <c>ConfigureAwait(false)</c> (WinUI bound-state rule).
    /// </summary>
    public async Task<string?> SendNewThread()
    {
        try
        {
            return await _manager.SendNewThread();
        }
        catch (BackendException)
        {
            return null;
        }
    }

    public void SetNewThreadBody(string body) => _manager.SetNewThreadBody(body);

    public void SetNewThreadSubject(string? subject) => _manager.SetNewThreadSubject(subject);

    public void SetNewThreadRecipientInput(string text) => _manager.SetNewThreadRecipientInput(text);

    public void AcceptNewThreadChip(TypedAddress addr) => _manager.AcceptNewThreadChip(addr);

    // ── Reactions & message delete ───────────────────────────────
    // conversations.md § Reactions & message delete. Both delegate to the shared
    // manager (FaunaMls-only, gated in the view on the thread's
    // supports_reactions / supports_message_delete capability — the manager also
    // re-checks). The manager applies the optimistic state + re-emits, so the
    // bubble re-binds with the updated reactions / deleted tombstone through the
    // normal snapshot-observer Refresh path — NO client-side optimistic flip.

    /// <summary>
    /// Toggle a quick-set or picked emoji reaction on a message
    /// (<c>dm-reaction-option</c> / <c>dm-reaction-pill</c> / the more-grid →
    /// <c>manager.toggle_reaction(thread, message, emoji)</c>,
    /// <c>conversations.md</c> § Reactions &amp; message delete). The shared
    /// manager resolves Add vs Remove against my current set, applies it
    /// optimistically, and fires the FaunaMls wire op. No
    /// <c>ConfigureAwait(false)</c> — a WinUI VM must resume on the UI thread
    /// (off-thread bound-state mutation throws a silent COMException).
    /// </summary>
    public async Task ToggleReactionAsync(string threadId, string messageId, string emoji) =>
        await _manager.ToggleReaction(threadId, messageId, emoji);

    /// <summary>
    /// Delete an own message (<c>dm-message-delete-confirm-button</c> →
    /// <c>manager.delete_message(thread, message)</c>, <c>conversations.md</c>
    /// § Reactions &amp; message delete). FaunaMls-only, sender-only — the manager
    /// rejects a non-own target. Optimistically tombstones the message + fires the
    /// FaunaMls wire op. No <c>ConfigureAwait(false)</c> (WinUI bound-state rule).
    /// </summary>
    public async Task DeleteMessageAsync(string threadId, string messageId) =>
        await _manager.DeleteMessage(threadId, messageId);

    // ── Mark as spam (the live Insert consumer) ──────────────────
    // mail-spam.md § Wire shapes / § Encrypted-mode interaction — the
    // follow-on to the moderation-queue 1d switch (history_op: None). Mirrors
    // linux app.rs::mark_message_spam.

    /// <summary>
    /// Mark a received message as spam (<c>dm-message-mark-as-spam-button</c> →
    /// <c>MailSettingsMachine::train_spam_model_client_mail</c>). Trains the caller's
    /// sealed tier-1 model over the retained decrypted <paramref name="body"/> AND
    /// writes a sealed <c>spam_training_history</c> row (<c>Insert</c>) — distinct from
    /// <see cref="DeleteMessageAsync"/>'s sibling gestures and from the
    /// moderation-queue's <c>history_op: None</c> train. <paramref name="messageId"/> is
    /// stored <b>opaque</b> (never decoded nest-side); the mailbox is <c>INBOX</c> — a
    /// received conversation message has no IMAP mailbox, this is display metadata only
    /// on the sealed row. <paramref name="subjectLine"/> falls back to a body snippet
    /// when the message carries none (the common case — <c>MessageSnapshot.subject_line</c>
    /// is <c>Some</c> only on a subject-change message).
    ///
    /// Gated <c>!isOwn</c> — both here (defense in depth) and at the menu, which never
    /// offers this gesture on an own message. Silently no-ops when the seam is absent
    /// (mail not enabled / no session), the body is empty, or the nest doesn't
    /// advertise <c>spam-model-sealed-at-rest</c>: a conversation message is client-only
    /// encrypted content the nest can't read, so — unlike a moderation-queue correction —
    /// there is NO server-side train to degrade to (mail-spam.md § Encrypted-mode
    /// interaction). No <c>ConfigureAwait(false)</c> (WinUI bound-state rule).
    /// </summary>
    public async Task MarkMessageSpamAsync(string messageId, string body, string? subjectLine, bool isOwn)
    {
        if (isOwn || _spamWrite is null || string.IsNullOrWhiteSpace(body))
            return;
        // Cheap pre-check before paying for the sealed write: a nest without
        // spam-model-sealed-at-rest ⇒ silent no-op (no server-train fallback exists for
        // a conversation message).
        if (!await _spamWrite.SealedSpamWriteAvailableAsync())
            return;
        // The empty-subject snippet fallback lives ONLY in the shared machine
        // (train_spam_model_client_mail — mail-spam.md § Encrypted-mode interaction),
        // so the VM passes the raw subject through, like linux/apple/android/web.
        await _spamWrite.TrainSpamModelClientMailAsync(
            body, isSpam: true, Encoding.UTF8.GetBytes(messageId), mailbox: "INBOX", subjectLine ?? "");
    }
}
