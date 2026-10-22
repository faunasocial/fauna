using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Conversations;
using FaunaApp.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;
using uniffi.fauna_labeler_catalog_machine;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Unified conversations page. Two-pane layout (320px list | detail);
/// renders entirely off the shared <see cref="ConversationsManager"/>
/// snapshot via <see cref="ConversationsViewModel"/>. Replaces the
/// nest-API-driven list + ContentDialog new-conversation flow.
/// </summary>
public sealed partial class ConversationsPage : Page
{
    /// <summary>
    /// Live page instance, set by Page_Loaded and cleared by Page_Unloaded.
    /// Used by the TestAgent's <c>conversations_accept_recipient</c> route
    /// to fire AcceptRequested on the currently-visible recipient picker
    /// without going through Win32 keyboard input (which is sandboxed on
    /// Windows 11 for non-foreground processes).
    /// </summary>
    internal static ConversationsPage? Current { get; private set; }

    private ConversationsViewModel? _vm;
    // The shared-Rust conversations session built at login (owns the wired
    // FaunaMls-backed manager); null in E2E / when no session exists, in which
    // case the page falls back to ConversationsManagerHost.Instance.
    private ConversationsSession? _convSession;

    // search.md § User actions (SearchNav.Mail / SearchNav.Draft) — set when
    // SearchResultsPage routed a Mail or Draft search hit here; consumed once
    // in Page_Loaded, right after _vm is built. Draft is a plain thread-jump
    // (or a new-compose start, for a threadless Draft); Mail additionally
    // carries _deepLinkMessageId, selecting a message inside the thread
    // (conversations.md § The selected message).
    private bool _deepLinkOpenConversations;
    private string? _deepLinkThreadId;
    private string? _deepLinkMessageId;

    // The blob loader for D4 link-preview og:images (render-model.md § D4) — the SAME
    // authenticated /api/v1/blob/<hash> nest GET the feed card uses, built once from the
    // nav-param's nest client. null in E2E / when navigated without a ServiceClients (the
    // og:image element stays realized via ImageHashBind ManageVisibility=False).
    private BlobImageLoader? _imageLoader;

    // Session crypto — only for the backup-audit observation feed's actor-scoped
    // state path (AccountStateDir.ObserveBackupAudit). Mirrors FeedPage's _crypto.
    private ICryptoService? _crypto;

    // The sealed spam-model client-write path (mail-spam.md § Encrypted-mode
    // interaction — dm-message-mark-as-spam-button, the live Insert
    // consumer). Built once from the nav-param's WS-RPC client, like _imageLoader; null
    // in E2E / when navigated without a ServiceClients (MarkMessageSpamAsync then
    // silently no-ops — mirrors ModerationPage's MachineSpamModelClientWrite wiring).
    private ISpamModelClientWrite? _spamWrite;

    // The WS-RPC façade — only for the post-succession member-review roster
    // (succession-aftermath.md § Propagation → *Removing a flagged member*).
    // null in E2E / when navigated without a ServiceClients, in which case the
    // review roster stays empty and no chip ever paints a mark.
    private INestRpcClient? _rpc;

    // The owner's open review roster (`member_reviews_list`), CACHED — a member
    // list paints far more often than the ledger changes. Loaded once on
    // Page_Loaded/reconnect and again after every Keep press
    // (RefreshMemberReviewRosterAsync); answered per-thread by the shared
    // synchronous join `FaunaFfiMethods.MemberReviewMarksForThread`.
    private IReadOnlyList<FfiMemberReview> _memberReviewRoster = Array.Empty<FfiMemberReview>();

    // ── Compose-body seed tracking (conversations.md § Persistence) ──
    // The live compose BODY lives in the C# control. It is now forwarded to the shared
    // manager on EVERY change (DmComposeBar.BodyChanged, wired in WireComposeBar) for
    // draft persistence v2 — which also schedules a debounced nest save — and still on
    // send / FlushActiveComposeDraft. These two fields record which compose surface the
    // page last SEEDED its body from, so a background snapshot tick (an inbound message,
    // a send-echo) never clobbers the reply or new message the user is mid-typing: the
    // body is re-seeded (from the manager's restored/cleared draft) only when the shown
    // surface first changes. `null` / `false` ⇒ re-seed on next render.
    private string? _seededThreadId;
    private bool _seededNewThread;

    // ── The messages list, identity-keyed by message id ──
    // Bubbles and subject dividers are REUSED across observer ticks and re-bound in place —
    // web (`{#each … (msg.message_id)}`) and android (`key = { it.messageId }`) key their
    // message lists the same way (priority #1/#4). Until 2026-09-15 every tick cleared
    // `MessagesList` and built every bubble anew, so a large body's layout was paid on every
    // tick while ticks queued behind it (mail-message-size.md § Implementation status today).
    // All three dictionaries hold the shown thread's messages only (cleared on a thread switch).
    private string? _bubblesThreadId;
    private readonly Dictionary<string, DmMessageBubble> _bubblesById = new();
    private readonly Dictionary<string, SubjectDivider> _dividersById = new();
    // The newest snapshot per message — what a reused bubble's event closures read (a
    // closure captures the bubble's id, never the tick's snapshot).
    private readonly Dictionary<string, MessageSnapshot> _latestSnapshotById = new();
    // The `SearchNav.Mail` selected message already brought into view — once per selection,
    // never per tick (a per-tick bring-into-view would fight the user's own scrolling).
    private string? _broughtIntoViewMessageId;
    private long _traceDetailViewMs;

    public ObservableCollection<ThreadRow> Threads { get; } = new();

    // `AcceptVisibleRecipientPicker()` lived here until 2026-08-29. It was
    // `_vm?.AcceptCurrentRecipientChip() ?? false` and the TestAgent's
    // `conversations_accept_recipient` arm was its only caller — so the page hop bought
    // nothing but a `?.` that turned "the conversations page is not mounted" into a
    // silent no-op, which `e2e-conventions.md` § convention 11 forbids as squarely as a
    // `.debug` log. The arm now drives `ConversationsManagerHost.Instance` directly
    // (probe, then commit, then refuse by name if no chip landed), which is also what
    // tui, linux and both apple targets do. Don't re-add a page-level forwarder for the
    // agent: the manager already decides which picker is active.

    /// <summary>
    /// Stage an attachment against whichever composer is currently active
    /// (an existing thread's reply, or the new-thread compose) — the
    /// TestAgent's <c>compose.file</c> route for <c>attachment-button</c>
    /// (native bridges can't drive the real OS file picker, so this stages
    /// against the same call its completion handler makes; mirrors
    /// <c>WireComposeBar</c>'s <c>AttachmentPicked</c> wiring — conversations.md
    /// § Attachments). Returns true if a composer was active to stage against.
    /// </summary>
    /// <summary>The active composer's applied styling (<see cref="DmComposeBar.ComposeTextRuns"/>)
    /// — the new-thread compose when it is up, else the open thread's reply. The TestAgent's
    /// <c>compose_text_runs</c> read, which answers <c>get_attr(dm-text-field, "text-runs")</c>.</summary>
    internal List<Dictionary<string, object?>> ComposeTextRuns() =>
        (_vm?.NewThreadCompose is not null ? NewThreadComposeBar : DetailComposeBar).ComposeTextRuns();

    internal bool StageAttachment(byte[] bytes, string mimeType, string filename)
    {
        if (_vm is null) return false;
        if (_vm.NewThreadCompose is not null)
        {
            _vm.AddNewThreadAttachment(filename, mimeType, bytes);
            App.ConvDrafts?.ScheduleSave();
            return true;
        }
        if (_vm.SelectedThreadId is { } id)
        {
            _vm.AddAttachment(id, filename, mimeType, bytes);
            App.ConvDrafts?.ScheduleSave();
            return true;
        }
        return false;
    }

    public ConversationsPage()
    {
        this.InitializeComponent();
        // The departure is the room plane's self-scoped door on the home nest
        // (OnlineOnly, `libs/fauna-protocol/src/offline_class.rs`); no nest, no walk-out.
        RoomLeaveConfirmButton.FaunaGate("fauna.conversations.room.leave");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _convSession = clients.ConvSession;
            _crypto = clients.Crypto;
            // Build the link-preview og:image loader off the same nest client the feed uses
            // (render-model.md § D4). ??= so a re-navigation reuses the cached loader — and
            // the loader resolves the CURRENT client per call rather than capturing
            // `clients.Nest`, which is disposed on the next re-login.
            _imageLoader ??= new BlobImageLoader(() => App.CurrentNest);
            // Sealed spam-model client-write path — same INestRpcClient
            // seam ModerationPage builds MachineSpamModelClientWrite from. ??= reuses it
            // across a re-navigation; null when navigated without an RPC client (E2E).
            if (clients.Rpc is not null)
                _spamWrite ??= new MachineSpamModelClientWrite(clients.Rpc);
            _rpc = clients.Rpc;
            _deepLinkOpenConversations = clients.DeepLinkOpenConversations;
            _deepLinkThreadId = clients.DeepLinkThreadId;
            _deepLinkMessageId = clients.DeepLinkMessageId;
        }
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // Reuse the manager-lifetime VM+observer pair across every re-navigation
        // instead of registering a fresh observer per page load — see ConversationsManagerHost.PageObserverState's doc for why
        // this is safe and where it gets nulled. ConversationsPage carries no
        // NavigationCacheMode (WinUI's default Disabled applies), so a fresh page
        // instance — and this handler — runs on every tab/nav-item activation.
        if (ConversationsManagerHost.PageObserverState is { } state)
        {
            _vm = state.Vm;
        }
        else
        {
            var observer = new ConversationsNotifyObserver();
            // Render off the login-built session's wired manager in production;
            // fall back to ConversationsManagerHost.Instance (mock backends) in
            // E2E / when no session exists (conversations.md § Architectural rules
            // #2 — the session owns all MLS state in shared Rust).
            var manager = _convSession?.Manager() ?? ConversationsManagerHost.Instance;
            _vm = new ConversationsViewModel(manager, observer, _spamWrite);
            ConversationsManagerHost.PageObserverState = (_vm, observer);
        }
        _vm.PropertyChanged += Vm_PropertyChanged;
        ConversationsList.ItemsSource = Threads;
        WireComposeBar(DetailComposeBar, isNewThread: false);
        WireComposeBar(NewThreadComposeBar, isNewThread: true);
        WireRecipientPicker();
        WireThreadHeader();
        Current = this;

        // Conversation drafts are nest-backed + cross-device now (draft-persistence v2):
        // App.ConvDrafts restored them INTO the manager at login (file-sync.md § Drafts
        // Sync), so opening a thread renders its restored body via the normal re-seed
        // path — no per-page C# seeding/registration (the feed compose rides the same
        // shape now, App.FeedDrafts, per-page since its manager rebuilds per visit).
        Refresh();

        // Post-succession member-review roster (succession-aftermath.md §
        // Propagation): loaded once on page load/reconnect — mirrors
        // linux/android's per-page-load caching, since windows drives no
        // aftermath pump of its own for this surface. A no-op member list
        // paints unmarked until this resolves; the next observer tick (or
        // this load's own re-render below) catches up.
        _ = LoadMemberReviewRosterAsync();
        // …and again on the shared store-change notice (account-runtime.md § A
        // runtime's own pump is a source of the notice too): a person put up for
        // review marks the OPEN thread's member chips with no re-visit.
        if (_rpc is not null)
        {
            _rpc.StoreChanged -= OnStoreChanged;
            _rpc.StoreChanged += OnStoreChanged;
        }

        // search.md § User actions (SearchNav.Mail / SearchNav.Draft): a
        // present message id (Mail only) selects that message inside its
        // thread (conversations.md § The selected message — never a plain
        // thread-jump followed by a second call, which would paint the
        // thread with no message marked yet); a present thread id with no
        // message id (Draft naming a thread) jumps to the thread; neither
        // present (the threadless new-thread Draft) starts a fresh compose.
        // No round trip — all three ViewModel calls are synchronous over the
        // already-built manager.
        if (_deepLinkOpenConversations)
        {
            _deepLinkOpenConversations = false;
            var threadId = _deepLinkThreadId;
            var messageId = _deepLinkMessageId;
            _deepLinkThreadId = null;
            _deepLinkMessageId = null;
            if (threadId is not null && messageId is not null) _vm.OpenThreadAndMessage(threadId, messageId);
            else if (threadId is not null) _vm.OpenThread(threadId);
            else _vm.StartNewConversation();
        }
    }

    private void Page_Unloaded(object sender, RoutedEventArgs e)
    {
        // Navigating away destroys the compose controls — flush the live body to
        // the manager so returning to the page restores it (the manager is
        // session-owned and outlives the page). conversations.md § Persistence.
        // Guarded: a sign-out's or switch's unload runs AFTER the actor change —
        // see UnlessManagerRetired.
        UnlessManagerRetired(FlushActiveComposeDraft, "unload draft flush");
        // Persist the just-flushed draft set to the nest now (draft-persistence v2),
        // in case the debounce window hadn't elapsed — fire-and-forget, best-effort.
        _ = App.ConvDrafts?.SaveNowAsync();
        if (_vm is not null) _vm.PropertyChanged -= Vm_PropertyChanged;
        if (_rpc is not null) _rpc.StoreChanged -= OnStoreChanged;
        if (ReferenceEquals(Current, this)) Current = null;
    }

    /// <summary>The store-change re-read of the review roster (the member chips'
    /// source): a failed read keeps the roster held, an unchanged one repaints
    /// nothing, and the compose draft is never touched (the pass is the same
    /// <see cref="Refresh"/> every observer tick runs).</summary>
    private async void OnStoreChanged()
    {
        if (_rpc is null) return;
        IReadOnlyList<FfiMemberReview> roster;
        try
        {
            roster = await _rpc.MemberReviewsListAsync();
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.ShellLog.Warn("ConversationsPage",
                $"[store-change] member-review roster read failed: {ex.GetType().Name}");
            return;
        }
        if (MemberReviewViewModel.SameRoster(roster, _memberReviewRoster)) return;
        _memberReviewRoster = roster;
        UnlessManagerRetired(Refresh, "store-change roster refresh");
    }

    private void Vm_PropertyChanged(object? sender, PropertyChangedEventArgs e) =>
        UnlessManagerRetired(Refresh, "observer refresh");

    /// <summary>
    /// Run one of this page's passes over the manager <see cref="_vm"/> renders,
    /// unless an actor change has already destroyed that manager under it.
    ///
    /// <para>The manager is retired off the UI thread, BEFORE this page is navigated
    /// away: <c>ConversationsManagerHost.ResetForActorChange</c> (every sign-out, the
    /// e2e reset, an account switch) disposes the host's own wrapper on the
    /// command's thread, and the frame navigates on a later UI turn. So this page's
    /// <see cref="Page_Unloaded"/> — and any observer tick queued in between (the
    /// engine's retirement itself notifies) — always runs after the actor change.
    /// When the page rendered off the host's wrapper (no <c>ConvSession</c> was
    /// handed over: production's swallowed session-build failure, or the e2e
    /// login's bounded await expiring), its next call is an
    /// <see cref="ObjectDisposedException"/> on the UI thread, which killed the app
    /// with <c>[fauna] FATAL (UI thread)</c>. A session
    /// wrapper (<c>ConvSession.Manager()</c>) is the page's own and is never
    /// disposed, so this guard only ever fires on the host-wrapper path.</para>
    ///
    /// <para>The exception IS the check, deliberately: a "retired?" flag read on
    /// this thread races the dispose on that one, while the wrapper's own call
    /// counter refuses atomically. Nothing is lost by skipping: the pass's target
    /// no longer exists, so its write could not have landed, and the page is on its
    /// way out (the flush that CAN land — into a session wrapper — never reaches the
    /// catch). Same posture as <c>UnreadBadgeObserver</c>'s read of the same manager
    /// mid-teardown.</para>
    /// </summary>
    private static void UnlessManagerRetired(Action pass, string what)
    {
        try
        {
            pass();
        }
        catch (ObjectDisposedException ex)
        {
            FaunaApp.Core.Logs.ShellLog.Info("ConversationsPage",
                $"[actor-change] {what} skipped: the page's manager was retired ({ex.ObjectName})");
        }
    }

    /// <summary>
    /// Refresh every UI surface from the latest snapshot. Cheap; called
    /// on every observer tick. Mirrors WinUI's "rebuild from snapshot"
    /// pattern from the launch / onboarding pages.
    /// </summary>
    private int _traceRefreshCount;
    private long _traceSyncThreadsMs;

    private void Refresh()
    {
        if (_vm is null) return;
        if (!FaunaApp.Core.Logs.E2eTrace.Enabled)
        {
            RefreshCore();
            return;
        }
        // Stage timings under the e2e trace only. `Snapshot` and `SelectedDetail` are
        // memoized per observer tick, so reading them first attributes their shared-Rust
        // derivation + UniFFI marshal here, and RefreshCore below reads the cached values.
        var sw = System.Diagnostics.Stopwatch.StartNew();
        var threads = _vm.Threads;
        var snapshotMs = sw.ElapsedMilliseconds;
        _ = _vm.SelectedDetail;
        var detailMs = sw.ElapsedMilliseconds - snapshotMs;
        RefreshCore();
        var n = ++_traceRefreshCount;
        FaunaApp.Core.Logs.E2eTrace.Write(
            $"[conv-refresh #{n}] snapshot={snapshotMs}ms selectedDetail={detailMs}ms "
                + $"core={sw.ElapsedMilliseconds - snapshotMs - detailMs}ms (syncThreads={_traceSyncThreadsMs}ms detailView={_traceDetailViewMs}ms) "
                + $"total={sw.ElapsedMilliseconds}ms "
                + $"rows={threads.Count} snippetChars={threads.Sum(t => (long)(t.snippet?.Length ?? 0))}");
    }

    private void RefreshCore()
    {
        if (_vm is null) return;

        // Thread list
        var syncSw = System.Diagnostics.Stopwatch.StartNew();
        SyncThreads(_vm.Threads);
        _traceSyncThreadsMs = syncSw.ElapsedMilliseconds;

        // The community-room invitations standing atop the list.
        PaintRoomInvitations(_vm.RoomInvitations);

        // Backup-audit observation feed (backups.md § Audit-alert surface): the
        // newest message-kind activity this client has actually rendered, fed on
        // EVERY render pass — not a separate effect — mirroring linux/tui/web/
        // android's "the conversation list's own render is the observation choke
        // point" idiom. A shell that renders threads but never observes ships a
        // permanently-passing audit; this is the load-bearing half, not an
        // afterthought.
        if (_vm.Threads.Count > 0)
        {
            var newestActivityMs = _vm.Threads.Max(t => t.lastActivityMs);
            var actorIdHex = _crypto?.HasKey == true ? _crypto.ActorIdHex : null;
            Core.Services.AccountStateDir.ObserveBackupAudit(actorIdHex, newestActivityMs);
        }

        // Right-pane mode: empty / detail / new-thread
        var nt = _vm.NewThreadCompose;
        var detail = _vm.SelectedDetail;
        if (nt is not null)
        {
            EmptyHint.Visibility = Visibility.Collapsed;
            DetailView.Visibility = Visibility.Collapsed;
            NewThreadView.Visibility = Visibility.Visible;
            RefreshNewThreadView(nt);
        }
        else if (detail is not null)
        {
            EmptyHint.Visibility = Visibility.Collapsed;
            DetailView.Visibility = Visibility.Visible;
            NewThreadView.Visibility = Visibility.Collapsed;
            var detailSw = System.Diagnostics.Stopwatch.StartNew();
            RefreshDetailView(detail);
            _traceDetailViewMs = detailSw.ElapsedMilliseconds;
        }
        else
        {
            EmptyHint.Visibility = Visibility.Visible;
            DetailView.Visibility = Visibility.Collapsed;
            NewThreadView.Visibility = Visibility.Collapsed;
            // Nothing shown → both compose surfaces must re-seed on next render.
            _seededThreadId = null;
            _seededNewThread = false;
        }

        // Add-participant overlay — driven off snapshot.addParticipant,
        // mirroring RefreshNewThreadView's NewThreadPicker block.
        var ap = _vm.AddParticipant;
        if (ap is not null)
        {
            AddParticipantPicker.SetChips(ap.picker.chips.Select(a => new ChipItem(ChipDisplay(a))).ToList());
            AddParticipantPicker.SetBridgeLabels(BridgeLabels());
            AddParticipantPicker.UpdateResolveState(ap.picker.resolveState);
            AddParticipantOverlay.Visibility = Visibility.Visible;
        }
        else
        {
            AddParticipantOverlay.Visibility = Visibility.Collapsed;
        }

        // ── Page-level error surface ──────────────────────────────
        // Surface either a failed membership/label op (Snapshot.error) or a
        // failed compose-send into the page error-message InfoBar, membership
        // taking precedence (conversations.md § Errors & edge cases — see the
        // VM's ActiveSendErrorReason doc comment for why the order is safe).
        // Runs on every observer tick (Vm_PropertyChanged → Refresh), so a page
        // error stamped while the user sits on the thread paints without a
        // re-navigate. Reads shared Rust state — no client-side state machine
        // (rule 1).
        // Publishing to App.CurrentErrorMessage is NOT optional bookkeeping: it is the
        // thread-safe mirror SerializeState reads for the state protocol's
        // `messages.error`, and therefore what `ActionLayer.error_text()`/`has_error()`
        // resolve to (they consult state first and never fall through to the element).
        // Every other page that owns an error bar publishes here; this page was the
        // outlier, so a surfaced conversations error read as no error at all.
        //
        // Which ELEMENT paints it depends on whether the room policy editor is
        // open. That editor owns an `error-message` of its own (ui.yaml lists it
        // among the `room_settings` sub-page's elements) and it covers this bar,
        // so while it is open the bar is held CLOSED — an InfoBar with
        // IsOpen=false collapses, taking its own `error-message` id out of the
        // UIA tree, so exactly one element ever carries that id and the
        // dm-send-button lesson two overlays up does not repeat. The state
        // channel below is unaffected either way, which is what the action
        // layer actually reads.
        // The report sheet's own failure line (a failed send, or a block/hide that did
        // not land beside a landed report) rides the same surface, held across ticks —
        // Refresh runs on every observer tick and would otherwise clear it at once.
        var sendError = _vm.ActiveSendErrorReason ?? _reportError;
        App.CurrentErrorMessage = sendError;
        var roomEditorOpen = RoomSettingsOverlay.Visibility == Visibility.Visible;
        if (sendError is not null && roomEditorOpen)
        {
            RoomSettingsError.Text = sendError;
            RoomSettingsError.Visibility = Visibility.Visible;
            ErrorBar.IsOpen = false;
            ErrorBar.Message = "";
        }
        else if (sendError is not null)
        {
            ErrorBar.Message = sendError;
            ErrorBar.IsOpen = true;
            RoomSettingsError.Visibility = Visibility.Collapsed;
            RoomSettingsError.Text = "";
        }
        else
        {
            ErrorBar.IsOpen = false;
            ErrorBar.Message = "";
            RoomSettingsError.Visibility = Visibility.Collapsed;
            RoomSettingsError.Text = "";
        }
    }

    /// <summary>A picker chip's text: the shared per-rail display, bridge-aware so a
    /// bridged address reads with the label its bridge declared
    /// (<c>TypedAddressDisplayWithBridges</c> — the nest-side match, no app mapping).</summary>
    private string ChipDisplay(TypedAddress a)
        => FaunaConversationsMethods.TypedAddressDisplayWithBridges(
            a, _vm?.Snapshot.bridges ?? Array.Empty<uniffi.fauna_core.BridgeIdentitySnapshot>());

    /// <summary>The serving bridges' declared labels, for the picker's bridges line.</summary>
    private IReadOnlyList<string> BridgeLabels()
        => (_vm?.Snapshot.bridges ?? Array.Empty<uniffi.fauna_core.BridgeIdentitySnapshot>())
            .Select(b => b.label).ToList();

    private void SyncThreads(IReadOnlyList<ThreadSummary> snapshot)
    {
        // The snapshot's `threads` is ALREADY filtered by the active search query in shared
        // Rust (ConversationsManager::snapshot() → filter_summaries over label + snippet), so
        // the page renders it directly — no client-side filter (priority #3/#4, matching
        // linux/web/android). Rebuild — small lists; if/when this gets huge, diff by id.
        Threads.Clear();
        foreach (var t in snapshot)
        {
            Threads.Add(new ThreadRow(t));
        }
        // Bind the list to the observable Threads once; subsequent Clear/Add ticks flow
        // through the binding (re-setting ItemsSource each tick would reset the ListView).
        if (!ReferenceEquals(ConversationsList.ItemsSource, Threads))
            ConversationsList.ItemsSource = Threads;

        // Keep the ListView's visual selection in sync with the shown thread so
        // SelectionChanged doesn't re-fire on every refresh — BUT clear it while
        // the new-thread composer is active. WinUI suppresses SelectionChanged
        // when the clicked row is already SelectedItem, so if the composer is
        // shown "over" the still-selected previous thread, clicking THAT same
        // conversation again fires nothing and the composer stays stuck. Clearing
        // the selection while composing makes the next click on any row — the
        // previous one included — a fresh selection that fires the event.
        var target = _vm?.NewThreadCompose is null && _vm?.SelectedThreadId is { } selectedId
            ? Threads.FirstOrDefault(r => r.ThreadIdValue == selectedId)
            : null;
        if (!ReferenceEquals(ConversationsList.SelectedItem, target))
        {
            ConversationsList.SelectionChanged -= ConversationsList_SelectionChanged;
            ConversationsList.SelectedItem = target;
            ConversationsList.SelectionChanged += ConversationsList_SelectionChanged;
        }
    }

    private void RefreshDetailView(ThreadDetail detail)
    {
        // Empty-label fallback is single-sourced in shared Rust
        // (thread_label_display): blank → "(no subject)", non-empty verbatim.
        // conversations.md § Where logic lives → Thread label display.
        DetailThreadHeader.SetLabel(
            Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ThreadLabelDisplay(detail.label)));
        DetailThreadHeader.SetProtocolIcon(
            ThreadRow.ProtocolTextFor(detail.glyph, detail.bridge), detail.bridge?.id);
        DetailThreadHeader.SetGuardianState(
            detail.guardianState is { } gs ? FaunaConversationsMethods.GuardianStateLabel(gs) : null,
            detail.guardianState is { } gt ? FaunaConversationsMethods.GuardianStateAttrToken(gt) : null);
        DetailThreadHeader.SetCapabilities(
            supportsRename: detail.capabilities.supportsRename,
            supportsMembershipChange: detail.capabilities.supportsMembershipChange,
            canInvite: detail.capabilities.canInvite,
            canRemoveMembers: detail.capabilities.canRemoveMembers);
        // The room's class statement and the editor's door — read off the
        // PROJECTED room (`RailBackend::room_state`), never computed here
        // (`conversation-rooms.md` § The three classes). Null room ⇒ both absent.
        var room = detail.room;
        DetailThreadHeader.SetRoom(
            classLabel: room is null ? null : RoomLabels.ClassLabel(room.@class),
            classToken: room is null
                ? null
                : FaunaConversationsMethods.RoomClassAttrToken(room.@class));
        // `thread-room-notice` — the room's standing notice, which one (if any)
        // holds decided by the shared `RoomNoticeFor` over the projected room's
        // three facts, its sentence and `state` token by the shared helpers;
        // there is no `room.notice()` on the C# record to ask instead.
        var notice = room is null
            ? null
            : FaunaConversationsMethods.RoomNoticeFor(
                room.awaitingKey, room.moderationUnverified, room.bridgeDisconnected);
        DetailThreadHeader.SetRoomNotice(
            notice is { } noticeForLabel ? FaunaConversationsMethods.RoomNoticeLabel(noticeForLabel) : null,
            notice is { } noticeForToken ? FaunaConversationsMethods.RoomNoticeAttrToken(noticeForToken) : null);
        // The editor's per-control gates follow the LIVE capabilities while it is
        // open (the door itself is never greyed — see ThreadHeader.SetRoom), and
        // so does what stands pending on the room: the home nest serves that list
        // on its own cadence, and a withdrawal re-reads it.
        if (RoomSettingsOverlay.Visibility == Visibility.Visible)
        {
            PaintRoomEditorGates(detail.capabilities);
            RestageRoomExtras();
        }
        // Members chips list, each index-parallel with its post-succession
        // review mark (succession-aftermath.md § Propagation → *Removing a
        // flagged member*) — the shared synchronous join over the CACHED
        // roster, never a hand-rolled scan.
        var marks = _vm is null
            ? Array.Empty<byte[]?>()
            : uniffi.fauna_ffi.FaunaFfiMethods.MemberReviewMarksForThread(
                _vm.Manager, detail.threadId, _memberReviewRoster.ToArray());
        // A chip removes its participant when the rail's membership is mutable
        // AND the viewer's role in a governed room may remove one — the roles
        // table applied in shared Rust (`RoomSnapshot::gate`), never re-derived
        // here. The GREYED case is the middle one: mutable membership, no role
        // to remove with. A mail chip is neither removable nor greyed — it stays
        // an informational pill (`ThreadHeader.xaml.cs`'s standing note).
        var greyedChips = detail.capabilities.supportsMembershipChange
            && !detail.capabilities.canRemoveMembers;
        var members = detail.participantDisplays
            .Select((d, i) =>
            {
                var person = i < marks.Length ? marks[i] : null;
                var address = i < detail.participants.Length ? detail.participants[i] : null;
                // The member's role on a governed room: the `role` attribute a
                // driver reads off the chip, and the localized owner/admin mark
                // in its text. Null on a policy-less room and on every non-room
                // thread — there are no roles to mark, not "everyone is a
                // member".
                var role = room is not null && i < room.members.Length
                    ? room.members[i].@role
                    : null;
                return new ChipItem(
                    RoomLabels.MemberChipText(d, role),
                    person is null ? null : Convert.ToHexString(person).ToLowerInvariant(),
                    address,
                    roleToken: role is null
                        ? null
                        : FaunaConversationsMethods.RoomRoleAttrToken(role.Value),
                    removeEnabled: !greyedChips);
            })
            .ToList();
        var membersControl = (Microsoft.UI.Xaml.Controls.ItemsControl)
            DetailThreadHeader.FindName("MembersList");
        if (membersControl is not null) membersControl.ItemsSource = members;

        // Compose bar reflects the per-thread draft. Re-seed the BODY only when
        // this thread first becomes the shown detail (conversations.md
        // § Persistence) — otherwise a background tick clobbers the reply being
        // typed. Subject/topic are live-forwarded, so they refresh every tick.
        var compose = detail.compose;
        var shownId = _vm?.SelectedThreadId;
        if (_seededThreadId != shownId)
        {
            DetailComposeBar.MessageText = compose.bodyDraft;
            _seededThreadId = shownId;
        }
        _seededNewThread = false;
        DetailComposeBar.TopicExpanded = compose.subjectDraft is not null;
        DetailComposeBar.SubjectText = compose.subjectDraft ?? string.Empty;
        DetailComposeBar.SetCapabilities(detail.capabilities);

        // Editable reply "To" line (mail only) — render the shared
        // compose.reply_recipients as display strings; the compose bar gates
        // visibility on supports_recipient_selection (conversations.md
        // § Participants vs. reply recipients).
        DetailComposeBar.SetReplyRecipients(
            compose.replyRecipients.Select(FaunaConversationsMethods.TypedAddressDisplay).ToList());

        // Staged-attachment chip row — render the shared compose.attachments
        // (conversations.md § Attachments → Staged-attachment preview).
        DetailComposeBar.SetAttachments(compose.attachments);

        // List-send texts — the shared compose.list_send, painted verbatim
        // (mail-mass-mailing.md § Composing a list message).
        DetailComposeBar.SetListSend(compose.listSend);

        // Reply preview banner — the shared reply_preview record (sender + excerpt of the
        // answered message), never derived here; none when no reply is armed.
        if (_vm?.ReplyPreviewText(detail.threadId) is { } replyPreview)
        {
            DetailComposeBar.ShowReplyPreview(replyPreview);
        }
        else
        {
            DetailComposeBar.HideReplyPreview();
        }

        // Messages list — imperative because x:Bind can't dispatch between
        // SubjectDivider and DmMessageBubble inside a single DataTemplate. Identity-keyed
        // by message id (the `_bubblesById` note above): a bubble the thread already shows is
        // RE-BOUND in place, a new one is built and wired once, and the panel's children are
        // reconciled to the snapshot's order — never cleared and rebuilt.
        if (_bubblesThreadId != detail.threadId)
        {
            _bubblesById.Clear();
            _dividersById.Clear();
            _latestSnapshotById.Clear();
            MessagesList.Children.Clear();
            _bubblesThreadId = detail.threadId;
            _broughtIntoViewMessageId = null;
        }
        var showReplyAll = detail.capabilities.supportsRecipientSelection;
        var desired = new List<UIElement>(detail.messages.Length * 2);
        var seen = new HashSet<string>(detail.messages.Length);
        // The `SearchNav.Mail` deep-link's bring-into-view half (conversations.md
        // § The selected message — "bringing it into view is part of the
        // affordance, not a nicety"): tracked as the loop below binds the
        // bubbles, brought into view once the list is reconciled.
        DmMessageBubble? selectedBubble = null;
        foreach (var m in detail.messages)
        {
            seen.Add(m.messageId);
            _latestSnapshotById[m.messageId] = m;
            if (!string.IsNullOrEmpty(m.subjectLine))
            {
                if (!_dividersById.TryGetValue(m.messageId, out var divider))
                {
                    divider = new SubjectDivider();
                    _dividersById[m.messageId] = divider;
                }
                divider.Subject = m.subjectLine ?? string.Empty;
                desired.Add(divider);
            }
            if (!_bubblesById.TryGetValue(m.messageId, out var bubble))
            {
                bubble = NewBubble();
                _bubblesById[m.messageId] = bubble;
            }
            // The document + byte resolver are set BEFORE Message (the DP whose change
            // fires Bind), so the bubble has them when it walks the structured
            // MessageSnapshot.document for BOTH the body and its first-class Attachment
            // blocks (render-model.md § D1/§ D2) — no client re-parses the body and no
            // client reads the sibling attachments field at render time.
            bubble.AttachmentBytesResolver = _vm!.AttachmentBytes;
            // D4 link-preview og:image loader (render-model.md § D4) — same nest blob
            // path as the feed card; null in E2E leaves the revealed element realized.
            bubble.LinkPreviewImageLoader = _imageLoader;
            bubble.Document = m.document;
            // Content-label badge (moderation.md § Per-row badge data path): set
            // alongside Document, before Message (the DP whose change fires Bind) —
            // same ordering rule, same reason.
            bubble.Labels = m.labels;
            // Reactions capability, set from the thread's shared capabilities (like
            // ShowReplyAll) — never branching on the rail enum (conversations.md §
            // Architectural rules #5). The delete button is gated on the message's
            // own canDelete (see ToMessageView), not on a thread capability here.
            bubble.SupportsReactions = detail.capabilities.supportsReactions;
            bubble.ShowReplyAll = showReplyAll;
            // A fresh view record per tick (its Reactions list is a new instance), so the DP
            // change fires Bind on every tick — which is what re-projects a document change
            // (a D3 reveal, a resolved link preview) onto a reused bubble.
            bubble.Message = ToMessageView(m, selected: m.messageId == detail.selectedMessageId);
            // D4 link-preview: fire-once resolve for EACH folded Resolving block in this
            // message's body (mirror FeedPage.SyncPosts). The shared
            // ConversationsManager.resolve_link_preview fetches once (cached) + re-emits; the
            // next ThreadDetail folds Resolved onto the document and Refresh repaints the cards.
            // Fire-once via state change — once terminal the block is no longer Resolving. The
            // shared `resolving_link_preview_urls` face yields every unresolved url in body
            // order, so a message with two bare URLs resolves BOTH.
            foreach (var url in FaunaApp.Core.Helpers.DocumentRenderer.ResolvingLinkPreviewUrls(m.document))
                _ = _vm?.ResolveLinkPreviewAsync(url);
            desired.Add(bubble);
            if (m.messageId == detail.selectedMessageId) selectedBubble = bubble;
        }
        // Messages the snapshot no longer carries (a thread re-projection) drop out of the
        // caches; the reconcile below drops them from the panel.
        foreach (var id in _bubblesById.Keys.Where(id => !seen.Contains(id)).ToList())
            _bubblesById.Remove(id);
        foreach (var id in _dividersById.Keys.Where(id => !seen.Contains(id)).ToList())
            _dividersById.Remove(id);
        foreach (var id in _latestSnapshotById.Keys.Where(id => !seen.Contains(id)).ToList())
            _latestSnapshotById.Remove(id);
        ReconcileChildren(MessagesList.Children, desired);
        if (selectedBubble is not null && _broughtIntoViewMessageId != detail.selectedMessageId)
        {
            selectedBubble.StartBringIntoView();
            _broughtIntoViewMessageId = detail.selectedMessageId;
        }
    }

    /// <summary>Make <paramref name="children"/> equal <paramref name="desired"/> by identity and
    /// order with the fewest removals/insertions: an element already at its index is left
    /// untouched (its layout survives), a stale one is removed, a new or moved one inserted at
    /// its index. The common tick — the same messages, or one appended — touches nothing or
    /// appends one.</summary>
    private static void ReconcileChildren(UIElementCollection children, List<UIElement> desired)
    {
        var keep = new HashSet<UIElement>(desired);
        for (int i = children.Count - 1; i >= 0; i--)
        {
            if (!keep.Contains(children[i])) children.RemoveAt(i);
        }
        for (int i = 0; i < desired.Count; i++)
        {
            if (i < children.Count && ReferenceEquals(children[i], desired[i])) continue;
            var at = children.IndexOf(desired[i]);
            if (at >= 0) children.RemoveAt(at);
            children.Insert(i, desired[i]);
        }
    }

    /// <summary>Build and wire one bubble — ONCE per message id; every tick after that re-binds
    /// it in place (RefreshDetailView). The closures therefore capture nothing from a tick:
    /// they read the message id off the event argument and the newest snapshot off
    /// <see cref="_latestSnapshotById"/>.</summary>
    private DmMessageBubble NewBubble()
    {
        var bubble = new DmMessageBubble();
        // dm-reply-button / dm-reply-all-button → manager.start_reply
        // (sender-only vs reply-all). The snapshot re-render seeds the To line.
        bubble.ReplyRequested += msg =>
        {
            if (_vm?.SelectedThreadId is { } tid) _vm.StartReply(tid, msg.Id, replyAll: false);
        };
        bubble.ReplyAllRequested += msg =>
        {
            if (_vm?.SelectedThreadId is { } tid) _vm.StartReply(tid, msg.Id, replyAll: true);
        };
        // load-remote-content-button → manager.reveal_remote_images (D3). The manager
        // flips the in-memory reveal set + re-emits; the next Refresh re-binds with
        // RemoteImage.revealed=true for this message (render-model.md § D3).
        bubble.RemoteRevealRequested += msg => { _vm?.RevealRemoteImages(msg.Id); };
        // dm-message-muted-reveal-button → session-local reveal (moderation.md §
        // Muted keywords). Unlike RemoteRevealRequested this is NOT a manager
        // dispatch — the mute list + reveal set are client-only state
        // (MutedKeywordsCache), so mark revealed then re-render this page
        // directly (no snapshot round-trip to re-bind from).
        bubble.MutedRevealRequested += msg =>
        {
            FaunaApp.Core.Services.MutedKeywordsCache.Reveal(msg.Id);
            Refresh();
        };
        // The content-policy collapse's "show anyway" (family-safety.md § Content
        // policy) — the same client-local shape as the muted reveal above, over
        // ContentPolicyCache's own session set. A `block` never raises this: the
        // shared SocialRenderGate resolves block ahead of the reveal set, so the
        // bubble paints no reveal button for one.
        bubble.ContentRevealRequested += msg =>
        {
            FaunaApp.Core.Services.ContentPolicyCache.Reveal(msg.Id);
            Refresh();
        };
        // dm-reaction-* / dm-reaction-pill → manager.toggle_reaction;
        // dm-message-delete-confirm-button → manager.delete_message. The VM ops
        // are async — fire-and-forget (the manager applies the optimistic state +
        // re-emits, and the observer → Refresh re-binds the bubble, the SAME path
        // StartReply uses, so NO local optimistic flip in C#).
        bubble.ReactionToggleRequested += tuple =>
        {
            if (_vm?.SelectedThreadId is { } tid)
                _ = _vm.ToggleReactionAsync(tid, tuple.msg.Id, tuple.emoji);
        };
        bubble.DeleteRequested += msg =>
        {
            if (_vm?.SelectedThreadId is { } tid)
                _ = _vm.DeleteMessageAsync(tid, msg.Id);
        };
        // dm-message-mark-as-spam-button → ConversationsViewModel.MarkMessageSpamAsync
        // (mail-spam.md § Wire shapes — the live Insert consumer). The
        // retained decrypted body + subject line aren't carried on the bubble's
        // DmMessageView (render-model.md § D1 keeps that surface UI-projection-only) — read
        // them off the message's newest MessageSnapshot. The VM gates !is_own + degrades
        // silently (no server-train fallback exists).
        bubble.MarkAsSpamRequested += msg =>
        {
            if (_latestSnapshotById.TryGetValue(msg.Id, out var m))
                _ = _vm?.MarkMessageSpamAsync(msg.Id, m.body, m.subjectLine, msg.IsOwn);
        };
        // dm-message-report-button → the shared report sheet (moderation.md §
        // User-initiated reporting). The plane ref and sender are not carried on the
        // bubble's UI-projection view — read them off the message's newest snapshot,
        // exactly as mark-as-spam does. The shared report_message_target answers
        // none for a mail / bridged message (no plane identity): nothing opens.
        bubble.ReportRequested += msg =>
        {
            if (_latestSnapshotById.TryGetValue(msg.Id, out var m))
                _ = OpenReportSheetAsync(m);
        };
        return bubble;
    }

    private void RefreshNewThreadView(ComposeState compose)
    {
        // Seed the BODY only when the new-thread composer first becomes the shown
        // detail (entering via +): a background tick must not clobber live typing,
        // and re-opening + after a switch restores the preserved draft body
        // (conversations.md § Persistence). Subject/topic/recipients are
        // live-forwarded, so they refresh every tick below.
        if (!_seededNewThread)
        {
            NewThreadComposeBar.MessageText = compose.bodyDraft;
            _seededNewThread = true;
        }
        _seededThreadId = null;
        NewThreadComposeBar.TopicExpanded = compose.subjectDraft is not null;
        NewThreadComposeBar.SubjectText = compose.subjectDraft ?? string.Empty;
        // For new-thread compose, capabilities aren't known until rail
        // locks via the recipient picker. Default to all-enabled.
        NewThreadComposeBar.IsEnabled = true;
        // Staged-attachment chip row — render the shared compose.attachments
        // (conversations.md § Attachments → Staged-attachment preview).
        NewThreadComposeBar.SetAttachments(compose.attachments);
        NewThreadComposeBar.SetListSend(compose.listSend);

        var picker = compose.recipientPicker;
        // `recipient-picker-home-nest-toggle` — painted with the composer whether
        // or not a chip is committed yet: it is a property of the room, chosen
        // before its first message, and the class statement follows it. Off until
        // the picker state exists (`include_home_nest` is a field of it).
        var includeHomeNest = picker?.includeHomeNest ?? false;
        NewThreadPicker.SetHomeNest(includeHomeNest);
        // A send that FOUNDS a community room (the home-nest choice is on and the
        // prospective class is community) is the `room.create` ceremony, never a
        // queued channel append — the send gates on it (see DmComposeBar.SetFoundsRoom).
        NewThreadComposeBar.SetFoundsRoom(
            picker is not null
            && includeHomeNest
            && FaunaConversationsMethods.RoomProspectiveClass(picker.chips, includeHomeNest)
                == RoomClass.Community);
        if (picker is not null)
        {
            NewThreadPicker.SetChips(picker.chips.Select(a => new ChipItem(ChipDisplay(a))).ToList());
            NewThreadPicker.SetBridgeLabels(BridgeLabels());
            NewThreadPicker.UpdateResolveState(picker.resolveState);
            // `recipient-picker-class` — the class of the room this composer is
            // about to create, derived in shared Rust from the committed chips
            // and the home-nest choice (`prospective_room_class`). Null before
            // the first chip commits, which paints nothing at all.
            var prospective = FaunaConversationsMethods.RoomProspectiveClass(
                picker.chips, picker.includeHomeNest);
            NewThreadPicker.SetProspectiveClass(
                prospective is null ? null : RoomLabels.ClassLabel(prospective.Value),
                prospective is null
                    ? null
                    : FaunaConversationsMethods.RoomClassAttrToken(prospective.Value));
            // Group-conversation hint when N >= 2 chips.
            GroupConversationHint.Visibility = picker.chips.Length >= 2
                ? Visibility.Visible
                : Visibility.Collapsed;
        }
    }

    /// <summary>
    /// Push the live compose BODY of whichever surface is currently shown into
    /// the shared manager's draft, so it survives a view switch
    /// (conversations.md § Persistence — each conversation, plus the new-thread
    /// composer, keeps its half-written message across switches). Windows holds
    /// the live body in the C# control and otherwise forwards it only on send;
    /// subject/topic/recipients are already live-forwarded. Call this BEFORE any
    /// action that changes the shown surface (selecting a thread, opening +,
    /// leaving the page). The manager mutation marshals back asynchronously
    /// (ConversationsNotifyObserver), so this never re-enters Refresh inline.
    /// </summary>
    private void FlushActiveComposeDraft()
    {
        if (_vm is null) return;
        if (_vm.NewThreadCompose is not null)
            _vm.SetNewThreadBody(NewThreadComposeBar.MessageText);
        else if (_vm.SelectedThreadId is { } id)
            _vm.SetComposeBody(id, DetailComposeBar.MessageText);
    }

    private void NewConversationButton_Click(object sender, RoutedEventArgs e)
    {
        // Preserve the current thread's half-written reply before switching to
        // the new-thread composer (which itself restores its own stashed draft).
        FlushActiveComposeDraft();
        _vm?.StartNewConversation();
    }

    private void SortButton_Click(object sender, RoutedEventArgs e) => _vm?.CycleSort();

    private void NewConversationCancel_Click(object sender, RoutedEventArgs e)
    {
        // Discard the in-progress new-thread draft (conversations.md § Persistence:
        // only an explicit cancel/discard or a successful send clears it). The shared
        // manager clears new_thread_compose; the snapshot observer then collapses
        // NewThreadView (its visibility is a pure function of NewThreadCompose) and
        // re-seeds an empty composer on the next open. Deliberately NO
        // FlushActiveComposeDraft — a discard must not first forward the
        // about-to-be-cleared body back into the manager.
        _vm?.CancelNewConversation();
        // Persist the now-cleared snapshot immediately (not the debounced
        // ScheduleSave — this is a discrete terminal action, same idiom as
        // Page_Unloaded's flush): without this the manager's in-memory state is
        // cleared but the nest's __drafts blob still holds the discarded draft,
        // which RestoreOnLaunchAsync resurrects into the composer on the next
        // launch (found via the e2e app-gate drain sweep, row 23 batch 19).
        _ = App.ConvDrafts?.SaveNowAsync();
    }

    private void ConversationsList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (ConversationsList.SelectedItem is ThreadRow row && _vm is not null)
        {
            // Preserve the body of the surface we're leaving (the new-thread
            // composer, or the previously-selected thread) before opening this one.
            FlushActiveComposeDraft();
            _vm.OpenThread(row.ThreadIdValue);
            _ = _vm.RefreshListSendAsync(row.ThreadIdValue);
            // No MarkRead here: the shared manager now reads a thread when it is
            // selected (ConversationsManager::notify, conversations.md § State &
            // data shape → When a thread is read) — an app calling it too would be
            // redundant, never wrong. web dropped its call the same way.
        }
    }

    /// <summary>
    /// Stamp every materialized <see cref="ListViewItem"/> with
    /// <c>AutomationProperties.AutomationId="conversation-item"</c>. Without
    /// this the e2e bridge's UIA query for <c>conversation-item</c> finds
    /// nothing — the DataTemplate's Grid has no AutomationPeer, so its
    /// own AutomationId is dropped when the row materializes; only the
    /// ListViewItem container is exposed to UIA. Stamping the container
    /// puts the id where the bridge can find it.
    /// </summary>
    private void ConversationsList_ContainerContentChanging(
        Microsoft.UI.Xaml.Controls.ListViewBase sender,
        Microsoft.UI.Xaml.Controls.ContainerContentChangingEventArgs args)
    {
        if (args.ItemContainer is Microsoft.UI.Xaml.Controls.ListViewItem container)
        {
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(container, Ids.ConversationItem);
        }
    }

    private void ConversationSearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        // Search is a manager re-query (conversations.md § Where logic lives → thread-list
        // search filtering), NEVER a client-side filter: the box drives set_search_query, the
        // manager re-emits, and Refresh re-renders the already-filtered snapshot().threads.
        // An empty/blank term clears the filter (empty → null, like web/android).
        var query = (sender.Text ?? "").Trim();
        _vm?.SetSearchQuery(string.IsNullOrEmpty(query) ? null : query);
    }

    // ── Compose bar wiring ───────────────────────────────────────

    private void WireComposeBar(DmComposeBar bar, bool isNewThread)
    {
        bar.SendRequested += OnSendRequested;
        // Stage immediately on pick (not deferred to Send) so it renders in the
        // chip row right away and remove_attachment/remove_new_thread_attachment
        // have something real to unstage (conversations.md § Attachments).
        bar.AttachmentPicked += (filename, mimeType, bytes) =>
        {
            if (_vm is null) return;
            if (isNewThread) _vm.AddNewThreadAttachment(filename, mimeType, bytes);
            else if (_vm.SelectedThreadId is { } id) _vm.AddAttachment(id, filename, mimeType, bytes);
            // The draft rests its attachments by content address (conversations.md §
            // Persistence → A restored draft's attachment is a handle, not a file), so a
            // pick is a draft edit like a keystroke: save it, or a relaunch loses the file
            // with nothing left to name or refuse.
            App.ConvDrafts?.ScheduleSave();
        };
        bar.RemoveAttachmentRequested += index =>
        {
            if (_vm is null) return;
            if (isNewThread) _vm.RemoveNewThreadAttachment((uint)index);
            else if (_vm.SelectedThreadId is { } id) _vm.RemoveAttachment(id, (uint)index);
            App.ConvDrafts?.ScheduleSave();
        };
        bar.SubjectChanged += subj =>
        {
            if (_vm is null) return;
            if (isNewThread) _vm.SetNewThreadSubject(string.IsNullOrEmpty(subj) ? null : subj);
            else if (_vm.SelectedThreadId is { } id) _vm.SetComposeSubject(id, subj);
            App.ConvDrafts?.ScheduleSave();
        };
        bar.TopicToggled += expanded =>
        {
            if (_vm is null) return;
            if (isNewThread)
            {
                _vm.SetNewThreadSubject(expanded ? string.Empty : null);
            }
            else if (_vm.SelectedThreadId is { } id)
            {
                _vm.ToggleTopic(id);
            }
            App.ConvDrafts?.ScheduleSave();
        };
        // Editable reply "To" line — only on an existing thread's detail bar
        // (new-thread compose addresses via the recipient picker, not a reply).
        if (!isNewThread)
        {
            bar.RemoveReplyRecipientRequested += index =>
            {
                if (_vm?.SelectedThreadId is { } id && _vm.SelectedDetail is { } d
                    && index >= 0 && index < d.compose.replyRecipients.Length)
                {
                    _vm.RemoveReplyRecipient(id, d.compose.replyRecipients[index]);
                }
            };
            bar.AddReplyRecipientRequested += text =>
            {
                if (_vm?.SelectedThreadId is { } id)
                {
                    // Shared format-only recognizer (the same one the recipient
                    // picker uses) — a malformed entry is a no-op.
                    var addr = FaunaConversationsMethods.TryParseTypedAddress(text.Trim());
                    if (addr is not null) _vm.AddReplyRecipient(id, addr);
                }
            };
            bar.ReplyCancelled += () =>
            {
                if (_vm?.SelectedThreadId is { } id) _vm.ClearReplyTo(id);
            };
        }
        // Live body forwarding for draft persistence v2 (conversations.md
        // § Persistence): every user edit updates the shared manager's ComposeState
        // (so drafts_snapshot_bytes captures it) and schedules a debounced nest save.
        // Programmatic seeds via the MessageText setter are suppressed, so this fires
        // only on real user input (typing / the e2e ValuePattern).
        bar.BodyChanged += body =>
        {
            if (_vm is null) return;
            if (isNewThread) _vm.SetNewThreadBody(body);
            else if (_vm.SelectedThreadId is { } id) _vm.SetComposeBody(id, body);
            App.ConvDrafts?.ScheduleSave();
        };
    }

    private async Task OnSendRequested(string body)
    {
        // dm-send-button: set the body draft, then route through the shared manager
        // (conversations.md § User actions) — manager.send_new_thread() for new
        // compose (it flushes a typed-but-uncommitted recipient and materializes the
        // thread), manager.send(thread_id) for an existing thread. Any staged
        // attachment is already in the manager (WireComposeBar's AttachmentPicked
        // wiring stages on pick, not deferred to here); send re-resolves it onto the
        // wire. All MLS / wire I/O runs in shared Rust (rule #2).
        if (_vm is null) return;
        if (_vm.NewThreadCompose is not null)
        {
            _vm.SetNewThreadBody(body);
            await _vm.SendNewThread();
        }
        else if (_vm.SelectedThreadId is { } id)
        {
            _vm.SetComposeBody(id, body);
            // Refused → the draft stays, in the manager and in the field, to retry;
            // the refusal is already on the page's error-message.
            if (!await _vm.Send(id)) return;
            // Sent → the manager cleared this thread's compose draft. Clear the field to
            // match (so FlushActiveComposeDraft can't re-forward the sent text) and
            // persist the now-empty draft set to the nest, so a later quit / another
            // device doesn't restore stale text (draft-persistence v2). The setter
            // suppresses BodyChanged, so this clear doesn't re-trigger a save itself.
            DetailComposeBar.MessageText = "";
            App.ConvDrafts?.ScheduleSave();
        }
    }

    // ── RecipientPicker wiring ────────────────────────────────────

    private void WireRecipientPicker()
    {
        NewThreadPicker.RawInputChanged += (_, text) =>
        {
            // Typing owes a probe: the sync write parks the picker on Resolving,
            // and the shared manager's async resolve settles it (Fauna → … →
            // Email, the rail-probe chain lives in shared Rust). Fire-and-forget,
            // like the link-preview resolves above; the snapshot Refresh carries
            // the terminal state back into the widget.
            _vm?.SetNewThreadRecipientInput(text);
            _ = _vm?.ResolveRecipientAsync();
        };
        // The home-nest choice stages straight into the shared picker state; the
        // snapshot tick repaints the toggle, `recipient-picker-class` and the send's
        // gate off it.
        NewThreadPicker.HomeNestToggled += (_, include) => _vm?.SetNewThreadHomeNest(include);
        NewThreadPicker.AcceptRequested += async (_, text) =>
        {
            if (_vm is null || string.IsNullOrWhiteSpace(text)) return;
            // Resolve, then commit what the probe confirmed — the manager's
            // `accept_current_recipient_chip` commits only a probed address
            // (never a format parse of the raw text), the same order the
            // TestAgent route (`ConversationsCommands`) and every other app
            // drive. A refused commit leaves the text in place; the widget's
            // status line (from the snapshot) says why.
            await _vm.ResolveRecipientAsync();
            _vm.AcceptCurrentRecipientChip();
            // A committed chip may be a mailing list: re-read the list-send texts
            // (tui's Op::AcceptRecipientChip does the same).
            await _vm.RefreshListSendAsync(null);
        };
    }

    // ── ThreadHeader wiring (rename / add-participant overlays) ───

    private void WireThreadHeader()
    {
        DetailThreadHeader.RenameRequested += (_, _) =>
        {
            RenameInput.Text = _vm?.SelectedDetail?.label ?? "";
            RenameOverlay.Visibility = Visibility.Visible;
            RenameInput.Focus(FocusState.Programmatic);
        };
        DetailThreadHeader.AddParticipantRequested += (_, _) =>
        {
            if (_vm?.SelectedThreadId is { } id)
            {
                _vm.OpenAddParticipant(id);
            }
            // Refresh() shows AddParticipantOverlay from the snapshot.
        };
        AddParticipantPicker.AcceptRequested += async (_, _) =>
        {
            // Resolve, then commit what the probe confirmed (see the new-thread
            // picker above); the manager decides which picker is active and the
            // snapshot round-trip carries the resolve state back.
            if (_vm is null) return;
            await _vm.ResolveRecipientAsync();
            _vm.AcceptCurrentRecipientChip();
        };
        AddParticipantPicker.RawInputChanged += (_, text) =>
        {
            _vm?.SetAddParticipantRecipientInput(text);
            _ = _vm?.ResolveRecipientAsync();
        };
        DetailThreadHeader.MemberKeepRequested += async (_, personHex) => await MemberReviewKeepAsync(personHex);
        DetailThreadHeader.MemberRemoveRequested += async (_, addr) => await RemoveMemberAsync(addr);
        DetailThreadHeader.RoomSettingsRequested += (_, _) => OpenRoomSettings();
    }

    // ── The room policy editor (ui.yaml `conversations.sub_pages.room_settings`)

    /// <summary>The staged draft, live only while the editor is open. Every
    /// mutation is value-in/value-out, so this field IS the staged state.</summary>
    private RoomSettingsDraft? _roomDraft;

    /// <summary>The thread the open editor is editing.</summary>
    private string? _roomThreadId;

    /// <summary>The roster the painted rows are indexed against, captured when
    /// the editor opened — so the rows a user is looking at do not renumber
    /// under them mid-edit. Every read and every staging gesture resolves
    /// through it BY IDENTITY (<c>RoomSettingsDraft::slot_of</c> keys on the
    /// actor id), so a shift cannot carry a choice onto the wrong person. Save
    /// deliberately re-reads the LIVE list instead.</summary>
    private TypedAddress[] _roomParticipants = Array.Empty<TypedAddress>();

    /// <summary>The display strings index-parallel with <see cref="_roomParticipants"/>.</summary>
    private string[] _roomDisplays = Array.Empty<string>();

    /// <summary>
    /// <c>thread-room-settings-button</c> → the policy editor. Seeds the shared
    /// draft off the thread's projected room and paints it; a null seed means a
    /// policy-less room or a non-room (nothing to edit) — the door only paints on a
    /// room, so this is the belt behind that. The door is live for every member;
    /// what a member may DO inside is <see cref="PaintRoomEditorGates"/>'s.
    /// </summary>
    private void OpenRoomSettings()
    {
        if (_vm?.SelectedThreadId is not { } id) return;
        if (_vm.SelectedDetail is not { } detail) return;
        if (FaunaConversationsMethods.RoomSettingsSeed(detail) is not { } seed) return;

        _roomDraft = seed;
        _roomThreadId = id;
        _roomParticipants = detail.participants;
        _roomDisplays = detail.participantDisplays;

        EnsureRoomTokenSelects();
        SelectRoomToken(
            RoomJoinRuleSelect,
            FaunaConversationsMethods.RoomJoinRuleToken(seed.@joinRule));
        SelectRoomToken(
            RoomHistoryPolicySelect,
            FaunaConversationsMethods.RoomHistoryPolicyToken(seed.@historyPolicy));

        BuildRoomMemberRows();
        PaintRoomEditorGates(detail.capabilities);
        RoomPendingInvitesList.ItemsSource = _roomPendingRows;
        RestageRoomExtras();
        // A community room's labeler set is staged over the labeler catalog, which
        // this page reads through its own machine (built off the editor's open).
        if (seed.labelers is not null) _ = StartRoomCatalogAsync(id);
        RoomSettingsError.Visibility = Visibility.Collapsed;
        RoomSettingsError.Text = "";
        RoomSettingsOverlay.Visibility = Visibility.Visible;
        RoomSettingsOverlay.Focus(FocusState.Programmatic);
    }

    /// <summary>
    /// The editor's per-control greying, read straight off the shared
    /// capabilities (<c>ui/conversations.md</c> § Element IDs, § Architectural
    /// rules 5: greyed, never hidden). The two selects and Save are the policy
    /// edit, so they follow <c>can_set_policy</c> — owner or admin; a plain
    /// member reads the settings and cannot stage a change. The walk-out is a
    /// member's own verb and follows <c>can_leave_room</c>, which the roles
    /// table closes only for the owner (hand the room over first). The
    /// per-participant rows carry their own capabilities
    /// (<see cref="RestageRoomMemberRows"/>).
    /// </summary>
    private void PaintRoomEditorGates(ThreadCapabilities caps)
    {
        RoomJoinRuleSelect.IsEnabled = caps.canSetPolicy;
        RoomHistoryPolicySelect.IsEnabled = caps.canSetPolicy;
        RoomSettingsSaveButton.IsEnabled = caps.canSetPolicy;
        RoomNestReadToggle.IsEnabled = caps.canSetPolicy;
        RoomLeaveButton.IsEnabled = caps.canLeaveRoom;
    }

    /// <summary>Whether the two token selects have been filled. Their option
    /// SETS are shared-Rust constants (<c>JoinRule::EDITOR_CHOICES</c>,
    /// <c>HistoryPolicy::EDITOR_CHOICES</c>), so they are filled once for the
    /// life of the page and never rebuilt.</summary>
    private bool _roomSelectsFilled;

    /// <summary>
    /// Fill the two token selects the <c>event-detail-reminder-select</c> way:
    /// each item's Content is the localized sentence, its Name AND Tag the
    /// driver-facing token — because <c>select</c> matches on Name and
    /// <c>get_text</c> reads the selected item's Name back, which is what makes
    /// the token round-trip (<c>ui.yaml</c> `room-join-rule-select`).
    ///
    /// <para>Filled ONCE, never rebuilt. Re-filling a ComboBox with the
    /// identical options is not a no-op under UIA — it destroys the automation
    /// peers of every item, and doing that under an automation client's feet is
    /// what crashed this app with a stowed <c>E_UNEXPECTED</c> on
    /// <c>BackupsPage</c> in 2026-09, whose prescription is exactly this: rebuild only when the
    /// option set changed, move the selection separately. The option set here is a constant, so it
    /// never changes; <see cref="SelectRoomToken"/> moves the selection.</para>
    /// </summary>
    private void EnsureRoomTokenSelects()
    {
        if (_roomSelectsFilled) return;
        foreach (var rule in FaunaConversationsMethods.RoomJoinRuleEditorChoices())
        {
            AddRoomTokenItem(
                RoomJoinRuleSelect,
                FaunaConversationsMethods.RoomJoinRuleToken(rule),
                RoomLabels.JoinRuleLabel(rule));
        }
        foreach (var policy in FaunaConversationsMethods.RoomHistoryPolicyEditorChoices())
        {
            AddRoomTokenItem(
                RoomHistoryPolicySelect,
                FaunaConversationsMethods.RoomHistoryPolicyToken(policy),
                RoomLabels.HistoryPolicyLabel(policy));
        }
        _roomSelectsFilled = true;
    }

    private static void AddRoomTokenItem(ComboBox box, string token, string label)
    {
        var item = new ComboBoxItem { Content = label, Tag = token };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, token);
        box.Items.Add(item);
    }

    /// <summary>Move a token select's selection without touching its items.</summary>
    private static void SelectRoomToken(ComboBox box, string token)
    {
        foreach (var candidate in box.Items)
        {
            if (candidate is ComboBoxItem { Tag: string t } item && t == token)
            {
                if (!ReferenceEquals(box.SelectedItem, item)) box.SelectedItem = item;
                return;
            }
        }
    }

    private static string SelectedRoomToken(ComboBox box)
        => (box.SelectedItem as ComboBoxItem)?.Tag as string ?? string.Empty;

    /// <summary>The editor's member rows, built ONCE when it opens and then only
    /// ever restaged in place — see <see cref="RoomMemberRow.Restage"/> for why
    /// the objects must outlive every gesture.</summary>
    private List<RoomMemberRow> _roomRows = new();

    /// <summary>Build the member rows for a freshly opened editor. The roster
    /// the editor paints is captured at open and does not change while it is up,
    /// so this runs exactly once per opening and <see cref="RestageRoomMemberRows"/>
    /// does all the rest.</summary>
    private void BuildRoomMemberRows()
    {
        _roomRows = new List<RoomMemberRow>();
        for (var i = 0; i < _roomDisplays.Length; i++)
        {
            _roomRows.Add(new RoomMemberRow(i, _roomDisplays[i]));
        }
        RoomMembersList.ItemsSource = _roomRows;
        RestageRoomMemberRows();
    }

    /// <summary>
    /// Re-read every member row off the draft, in place. Called after each
    /// staging gesture, which is what keeps the <c>checked</c> attributes and
    /// the at-most-one-staged hand-over rule true without tracking anything per
    /// widget. Each row resolves through the draft's identity column
    /// (<c>RoomSettingsIsEligible</c>/<c>AdminAt</c>/<c>TransferStagedAt</c>),
    /// never by indexing its vectors.
    /// </summary>
    private void RestageRoomMemberRows()
    {
        if (_roomDraft is not { } draft) return;
        if (_vm?.SelectedDetail?.capabilities is not { } caps) return;
        foreach (var row in _roomRows)
        {
            var index = (uint)row.Index;
            // Whether either control may act on this row AT ALL — a Fauna
            // member who is not the owner. The greying is exactly
            // `capability && eligible`, the expression the shared draft's own
            // doc prescribes for all seven apps.
            var eligible = FaunaConversationsMethods.RoomSettingsIsEligible(
                draft, _roomParticipants, index);
            row.Restage(
                adminStaged: FaunaConversationsMethods.RoomSettingsAdminAt(
                    draft, _roomParticipants, index),
                transferStaged: FaunaConversationsMethods.RoomSettingsTransferStagedAt(
                    draft, _roomParticipants, index),
                adminEnabled: caps.canAppointAdmins && eligible,
                transferEnabled: caps.canTransferOwnership && eligible);
        }
    }

    private void RoomAdminToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_roomDraft is not { } draft) return;
        if (sender is not ToggleButton { Tag: int index }) return;
        _roomDraft = FaunaConversationsMethods.RoomSettingsToggleAdmin(
            draft, _roomParticipants, (uint)index);
        RestageRoomMemberRows();
    }

    private void RoomOwnerTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (_roomDraft is not { } draft) return;
        if (sender is not ToggleButton { Tag: int index }) return;
        // Staging a second row un-stages the first — the rule is the draft's,
        // and the rebuild is what makes every other row's `checked` follow.
        _roomDraft = FaunaConversationsMethods.RoomSettingsToggleTransfer(
            draft, _roomParticipants, (uint)index);
        RestageRoomMemberRows();
    }

    /// <summary>
    /// <c>room-settings-save-button</c>: commit every staged change as its own
    /// policy commit, the hand-over last, and close ONLY when all of them
    /// landed — otherwise stay open with the refused value still staged and the
    /// page's `error-message` saying which (<c>ui/conversations.md</c> § Element
    /// IDs). The whole loop is one <c>apply_room_settings</c> call, which stops
    /// at the first refusal; nothing about the order or the refusal is decided
    /// here.
    /// </summary>
    private async void RoomSettingsSave_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _roomDraft is not { } draft || _roomThreadId is not { } id) return;

        // The two pickers are read AT SAVE (they hold their staged tokens; the
        // toggles staged straight into the draft as they were pressed), so the
        // whole staged state reaches the manager as one draft.
        draft = FaunaConversationsMethods.RoomSettingsSetJoinRule(
            draft, SelectedRoomToken(RoomJoinRuleSelect));
        draft = FaunaConversationsMethods.RoomSettingsSetHistoryPolicy(
            draft, SelectedRoomToken(RoomHistoryPolicySelect));
        _roomDraft = draft;

        // The LIVE roster, never the list captured when the editor opened: a
        // member who left while it sat open must not be written into the room's
        // signed policy. The draft resolves by identity, so this is safe to
        // differ from the painted list.
        var live = _vm.SelectedDetail?.participants ?? _roomParticipants;
        var edits = FaunaConversationsMethods.RoomSettingsEdits(draft, live);
        if (edits.Length == 0)
        {
            // Nothing staged: Save is a close, not a commit.
            CloseRoomSettings();
            return;
        }

        var allLanded = await _vm.ApplyRoomSettings(id, edits);
        if (allLanded)
        {
            CloseRoomSettings();
            return;
        }
        // Refused. The editor STAYS OPEN; Refresh() routes the page error into
        // this overlay's own `error-message` while it is.
        Refresh();
    }

    private void RoomSettingsCancel_Click(object sender, RoutedEventArgs e) => CloseRoomSettings();

    /// <summary>Esc cancels, like the rename overlay (no cancel id —
    /// <c>ui/conversations.md</c> § Element IDs).</summary>
    private void RoomSettings_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Escape)
        {
            CloseRoomSettings();
            e.Handled = true;
        }
    }

    /// <summary>
    /// <c>room-leave-button</c> → <c>room-leave-confirm</c>'s overlay. The
    /// capability is re-checked HERE, not only at paint: a greyed control must
    /// not be openable by a stale snapshot or a driver (tui shipped that bug and
    /// its own test caught it).
    /// </summary>
    private void RoomLeave_Click(object sender, RoutedEventArgs e)
    {
        if (_vm?.SelectedDetail?.capabilities.canLeaveRoom is not true) return;
        RoomLeaveConfirmOverlay.Visibility = Visibility.Visible;
        RoomLeaveConfirmOverlay.Focus(FocusState.Programmatic);
    }

    private void RoomLeaveCancel_Click(object sender, RoutedEventArgs e) =>
        RoomLeaveConfirmOverlay.Visibility = Visibility.Collapsed;

    /// <summary>Esc cancels the confirm, like the room editor beneath it.</summary>
    private void RoomLeaveConfirm_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Escape)
        {
            RoomLeaveConfirmOverlay.Visibility = Visibility.Collapsed;
            e.Handled = true;
        }
    }

    /// <summary>
    /// <c>room-leave-confirm</c>: walk out NOW — leaving is an immediate act, not
    /// a staged policy edit, so it never goes through
    /// <see cref="RoomSettingsSave_Click"/>. The editor closes with the gesture
    /// (whatever was staged in it belongs to a room this account is leaving) and
    /// the whole mechanism is one shared verb, <c>ConversationsManager::leave_room</c>,
    /// whose class-picked door this page never sees. A refusal — the owner's
    /// transfer-first, or a report the floor did not take — is already on
    /// <c>Snapshot.error</c> when it returns, and <c>Refresh()</c> paints it on the
    /// page's <c>error-message</c>. The thread stays in the list: the user keeps
    /// their own copy. No try/catch, the <see cref="RemoveMemberAsync"/> posture.
    /// </summary>
    private async void RoomLeaveConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _roomThreadId is not { } id) return;
        if (_vm.SelectedDetail?.capabilities.canLeaveRoom is not true)
        {
            RoomLeaveConfirmOverlay.Visibility = Visibility.Collapsed;
            return;
        }
        CloseRoomSettings();
        await _vm.LeaveRoom(id);
        Refresh();
    }

    // ── The community room's controls in the editor (`ui/conversations.md`
    //    § Element IDs → *The community room's three controls*, *Pending
    //    invitations*, *The room's labeler set*) ──────────────────────────────

    /// <summary>The page's own labeler-catalog machine (the windows
    /// per-page-machine convention), live only while the editor is open on a room
    /// whose draft carries a labeler set.</summary>
    private LabelerCatalogMachine? _roomCatalog;

    /// <summary>Held for the machine's lifetime — the observer is what ticks the
    /// UI thread when the catalog (or its inspect view) changes.</summary>
    private LabelerCatalogNotifyObserver? _roomCatalogObserver;

    private List<RoomLabelerRow> _roomLabelerRows = new();

    /// <summary>The pending-invite rows, keyed by invitee and updated in place so
    /// a served-list tick destroys only the rows that actually left (a withdrawn
    /// invitation's own row), never the whole list under a live automation
    /// client — see <see cref="RoomMemberRow.Restage"/>.</summary>
    private readonly ObservableCollection<RoomPendingInviteRow> _roomPendingRows = new();

    /// <summary>Re-read the editor's three room-class sections off the staged
    /// draft and the live projected room. Cheap; called on open, after every
    /// staging gesture and on every observer tick while the editor is up.</summary>
    private void RestageRoomExtras()
    {
        PaintRoomNestRead();
        PaintRoomPendingInvites();
        PaintRoomLabelers();
    }

    /// <summary>
    /// <c>room-nest-read-toggle</c>: the home nest's read, painted only where
    /// there is one to stage — a community room whose answer this device has read
    /// (<c>RoomSettingsDraft.nestRead</c> is Some); absent (collapsed) otherwise.
    /// Greyed unless the viewer may set policy. Staged into the shared draft by
    /// the press, committed by Save.
    /// </summary>
    private void PaintRoomNestRead()
    {
        if (_roomDraft is not { } draft || draft.nestRead is not { } reads)
        {
            RoomNestReadToggle.Visibility = Visibility.Collapsed;
            return;
        }
        RoomNestReadToggle.Content = RoomLabels.NestReadLabel(reads);
        RoomNestReadToggle.IsChecked = reads;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            RoomNestReadToggle, reads ? "true" : "false");
        RoomNestReadToggle.IsEnabled = _vm?.SelectedDetail?.capabilities.canSetPolicy == true;
        RoomNestReadToggle.Visibility = Visibility.Visible;
    }

    private void RoomNestRead_Click(object sender, RoutedEventArgs e)
    {
        if (_roomDraft is not { } draft || draft.nestRead is null) return;
        _roomDraft = FaunaConversationsMethods.RoomSettingsToggleNestRead(draft);
        RestageRoomExtras();
    }

    /// <summary>
    /// <c>room-pending-invite[i]</c> + its withdraw button: what stands pending on
    /// the room, only once the home nest has served this viewer a list and only
    /// while it is not empty — the section is absent for <c>None</c> and for an
    /// empty answer alike. The sentence is the shared
    /// <c>RoomPendingInviteText</c>; <c>lapsed</c> rides HelpText.
    /// </summary>
    private void PaintRoomPendingInvites()
    {
        var pending = _vm?.SelectedDetail?.room?.pendingInvites;
        if (pending is null || pending.Length == 0)
        {
            RoomPendingInvitesSection.Visibility = Visibility.Collapsed;
            _roomPendingRows.Clear();
            return;
        }
        var wanted = new HashSet<string>(pending.Select(p => p.inviteeActorHex));
        for (var i = _roomPendingRows.Count - 1; i >= 0; i--)
        {
            if (!wanted.Contains(_roomPendingRows[i].InviteeActorHex)) _roomPendingRows.RemoveAt(i);
        }
        for (var i = 0; i < pending.Length; i++)
        {
            var text = FaunaConversationsMethods.RoomPendingInviteText(pending[i]);
            if (i < _roomPendingRows.Count && _roomPendingRows[i].InviteeActorHex == pending[i].inviteeActorHex)
                _roomPendingRows[i].Update(text, pending[i].lapsed);
            else
                _roomPendingRows.Insert(i, new RoomPendingInviteRow(pending[i].inviteeActorHex, text, pending[i].lapsed));
        }
        RoomPendingInvitesSection.Visibility = Visibility.Visible;
    }

    private async void RoomPendingInviteWithdraw_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _roomThreadId is not { } id) return;
        if (sender is not Button { Tag: string inviteeActorHex }) return;
        // Acts at once — outside Save's staged set — and the manager re-reads the
        // list on success. A refusal is already on Snapshot.error and lands on the
        // editor's own error-message through Refresh().
        await _vm.WithdrawRoomInvite(id, inviteeActorHex);
        Refresh();
    }

    /// <summary>The withdrawal is the room plane's online-only
    /// <c>room.revoke_invite</c> door (<c>offline_class.rs</c>).</summary>
    private void RoomPendingInviteWithdraw_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button button) button.FaunaGate("fauna.conversations.room.revoke_invite");
    }

    /// <summary>
    /// <c>room-labeler-toggle[i]</c> + <c>room-labeler-inspect-button[i]</c> per
    /// catalog labeler a room may name. Every decision is shared Rust: which kinds
    /// a room may name (<c>RoomMayNameLabelerKind</c>), the <c>checked</c> mark
    /// (<c>RoomSettingsLabelerStaged</c>) and whether a row may act
    /// (<c>RoomSettingsLabelerToggleLive</c> — a full set greys only the rows it
    /// does not name); this adds the one capability term. Absent where the draft
    /// has no set to stage (an end-to-end room, or a set that did not verify).
    /// Built once per catalog shape and restaged in place — the row objects, and
    /// so their automation peers, survive every staging gesture.
    /// </summary>
    private void PaintRoomLabelers()
    {
        if (_roomDraft is not { } draft || draft.labelers is null)
        {
            RoomLabelersSection.Visibility = Visibility.Collapsed;
            return;
        }
        var snapshot = _roomCatalog?.Snapshot();
        var entries = snapshot?.entries ?? Array.Empty<LabelerCatalogEntry>();

        var shape = new List<(uint Index, LabelerCatalogEntry Entry)>();
        for (var i = 0; i < entries.Length; i++)
        {
            if (FaunaConversationsMethods.RoomMayNameLabelerKind(entries[i].artifactKind))
                shape.Add(((uint)i, entries[i]));
        }
        var sameShape = shape.Count == _roomLabelerRows.Count
            && shape.Select((s, i) => _roomLabelerRows[i].Matches(s.Index, s.Entry.labelerId)).All(m => m);
        if (!sameShape)
        {
            _roomLabelerRows = shape
                .Select(s => new RoomLabelerRow(s.Index, s.Entry.labelerId, s.Entry.artifactKind))
                .ToList();
            RoomLabelersList.ItemsSource = _roomLabelerRows;
        }
        var canSetPolicy = _vm?.SelectedDetail?.capabilities.canSetPolicy == true;
        foreach (var row in _roomLabelerRows)
        {
            row.Restage(
                staged: FaunaConversationsMethods.RoomSettingsLabelerStaged(draft, row.LabelerId),
                toggleEnabled: canSetPolicy
                    && FaunaConversationsMethods.RoomSettingsLabelerToggleLive(draft, row.LabelerId));
        }
        RoomLabelerInspectPanel.Render(snapshot?.inspecting);
        RoomLabelersSection.Visibility = Visibility.Visible;
    }

    private void RoomLabelerToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_roomDraft is not { } draft) return;
        if (sender is not ToggleButton { Tag: string labelerId }) return;
        _roomDraft = FaunaConversationsMethods.RoomSettingsToggleLabeler(draft, labelerId);
        RestageRoomExtras();
    }

    /// <summary>Inspecting is a read any member may make; it opens the catalog's own
    /// inspect view IN PLACE, so the editor and its staged draft stay up.</summary>
    private async void RoomLabelerInspect_Click(object sender, RoutedEventArgs e)
    {
        if (_roomCatalog is not { } catalog) return;
        if (sender is not Button { Tag: uint index }) return;
        await catalog.Inspect(index);
        PaintRoomLabelers();
    }

    /// <summary><c>labeler-inspect-close-button</c> — back to the editor's labeler
    /// rows; <c>close_inspect()</c> ticks the observer itself.</summary>
    private void RoomLabelerInspect_CloseRequested(object? sender, EventArgs e)
        => _roomCatalog?.CloseInspect();

    private async Task StartRoomCatalogAsync(string threadId)
    {
        if (_rpc is null) return;
        try
        {
            var observer = new LabelerCatalogNotifyObserver(OnRoomCatalogChanged);
            var machine = await _rpc.BuildLabelerCatalogMachineAsync(observer);
            // The editor may have closed (or moved to another room) while the
            // machine was building: then this one belongs to nobody.
            if (_roomThreadId != threadId) return;
            _roomCatalogObserver = observer;
            _roomCatalog = machine;
            await machine.Refresh();
            if (_roomThreadId == threadId) PaintRoomLabelers();
        }
        catch (Exception ex)
        {
            // The labeler rows simply stay empty; the rest of the editor is
            // unaffected by a catalog that could not be read.
            FaunaApp.Core.Logs.ShellLog.Warn("ConversationsPage", $"room labeler catalog unavailable: {ex.Message}");
        }
    }

    private void OnRoomCatalogChanged()
    {
        if (RoomSettingsOverlay.Visibility == Visibility.Visible) PaintRoomLabelers();
    }

    // ── The invitations atop the list ────────────────────────────────────

    /// <summary>The signature of the invitations last painted, so a tick that
    /// changed nothing rebuilds nothing (a rebuild would destroy the very
    /// accept button a driver just pressed).</summary>
    private string _roomInvitationsPainted = "";

    /// <summary>
    /// <c>room-invitation[i]</c> + its accept/decline pair, atop the list. The rows
    /// are already verified against their signers in shared Rust and the sentence
    /// is the shared <c>RoomInvitationText</c>. Absent (collapsed) while none
    /// stands.
    /// </summary>
    private void PaintRoomInvitations(RoomInvitationSnapshot[] invitations)
    {
        var rows = invitations
            .Select(i => new RoomInvitationRow(
                i.id,
                FaunaConversationsMethods.RoomInvitationText(i),
                FaunaConversationsMethods.RoomRoleAttrToken(i.@role)))
            .ToList();
        var signature = string.Join("\n", rows.Select(r => $"{r.Id}|{r.RoleToken}|{r.Text}"));
        if (signature == _roomInvitationsPainted) return;
        _roomInvitationsPainted = signature;
        RoomInvitationsList.ItemsSource = rows;
        RoomInvitationsList.Visibility = rows.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Accepting is the room plane's online-only
    /// <c>room.accept_invite</c> ceremony; declining (<c>inbox.ack</c>) is
    /// offline-safe and stays UNGATED.</summary>
    private void RoomInvitationAccept_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button button) button.FaunaGate("fauna.conversations.room.accept_invite");
    }

    private async void RoomInvitationAccept_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { Tag: long id }) return;
        // The manager SELECTS the room's thread on success, switching the right
        // pane's surface: keep a half-written reply first, like `+` does.
        FlushActiveComposeDraft();
        await _vm.AcceptRoomInvitation(id);
        Refresh();
    }

    private async void RoomInvitationDecline_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { Tag: long id }) return;
        await _vm.DeclineRoomInvitation(id);
        Refresh();
    }

    private void CloseRoomSettings()
    {
        RoomLeaveConfirmOverlay.Visibility = Visibility.Collapsed;
        RoomSettingsOverlay.Visibility = Visibility.Collapsed;
        RoomSettingsError.Visibility = Visibility.Collapsed;
        RoomSettingsError.Text = "";
        RoomMembersList.ItemsSource = null;
        _roomRows = new List<RoomMemberRow>();
        RoomPendingInvitesList.ItemsSource = null;
        _roomPendingRows.Clear();
        RoomLabelersList.ItemsSource = null;
        _roomLabelerRows = new List<RoomLabelerRow>();
        RoomLabelerInspectPanel.Render(null);
        RoomNestReadToggle.Visibility = Visibility.Collapsed;
        RoomPendingInvitesSection.Visibility = Visibility.Collapsed;
        RoomLabelersSection.Visibility = Visibility.Collapsed;
        _roomCatalog = null;
        _roomCatalogObserver = null;
        _roomDraft = null;
        _roomThreadId = null;
        _roomParticipants = Array.Empty<TypedAddress>();
        _roomDisplays = Array.Empty<string>();
    }

    /// <summary>
    /// <c>thread-member-chip</c> tap on a membership-change-capable thread:
    /// drop <paramref name="addr"/> from the group (posts the FaunaMls
    /// Commit, no Welcome). No try/catch — a refused op restores the
    /// participant and surfaces on <c>Snapshot.error</c>, read every Refresh
    /// tick by <c>ActiveSendErrorReason</c> (same as <c>RenameThread</c>;
    /// mirrors linux's `detail.rs` fire-and-forget shape).
    /// </summary>
    private async Task RemoveMemberAsync(TypedAddress addr)
    {
        if (_vm?.SelectedThreadId is { } id)
        {
            await _vm.RemoveParticipant(id, addr);
        }
    }

    /// <summary>
    /// Load the owner's open review roster (<c>member_reviews_list</c>) —
    /// called once on <see cref="Page_Loaded"/>/reconnect and again after
    /// every <see cref="MemberReviewKeepAsync"/>. Never throws (mirrors
    /// linux/android's ANY-failure-degrades-to-empty posture — an absent
    /// review roster is not a user-facing error, and the marks it would have
    /// carried simply stay unpainted until the next successful load).
    /// </summary>
    private async Task LoadMemberReviewRosterAsync()
    {
        if (_rpc is null)
        {
            FaunaApp.Core.Logs.ShellLog.Info("ConversationsPage",
                "member-review roster not loaded: no WS-RPC façade on this page");
            return;
        }
        try
        {
            _memberReviewRoster = await _rpc.MemberReviewsListAsync();
            FaunaApp.Core.Logs.ShellLog.Info("ConversationsPage",
                $"member-review roster loaded: {_memberReviewRoster.Count} open review(s)");
        }
        catch (Exception ex)
        {
            // Degrades to empty by design, but a swallow that says nothing made a
            // missing chip mark undiagnosable — log the type only (no message text).
            FaunaApp.Core.Logs.ShellLog.Warn("ConversationsPage",
                $"member-review roster load failed, degrading to empty: {ex.GetType().Name}");
            _memberReviewRoster = Array.Empty<FfiMemberReview>();
        }
        Refresh();
    }

    /// <summary>
    /// <c>thread-member-keep-button</c>: record Keep for <paramref name="personHex"/>
    /// (<c>member_review_keep</c>), then re-fetch the roster so every chip
    /// repaints from the nest's own answer — never an optimistic flip.
    /// </summary>
    private async Task MemberReviewKeepAsync(string personHex)
    {
        if (_rpc is null) return;
        try
        {
            var person = Convert.FromHexString(personHex);
            await _rpc.MemberReviewKeepAsync(person);
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
        await LoadMemberReviewRosterAsync();
    }

    private void RenameInput_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            RenameConfirm_Click(sender, new RoutedEventArgs());
            e.Handled = true;
        }
    }

    private async void RenameConfirm_Click(object sender, RoutedEventArgs e)
    {
        RenameOverlay.Visibility = Visibility.Collapsed;
        if (_vm?.SelectedThreadId is { } id && !string.IsNullOrWhiteSpace(RenameInput.Text))
        {
            await _vm.RenameThread(id, RenameInput.Text.Trim());
        }
    }

    private void RenameCancel_Click(object sender, RoutedEventArgs e)
    {
        RenameOverlay.Visibility = Visibility.Collapsed;
    }

    private async void AddParticipantConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ConfirmAddParticipant();
        // confirm_add_participant clears the snapshot's add_participant slot;
        // Refresh() (fired by the snapshot observer) collapses AddParticipantOverlay.
    }

    private void AddParticipantCancel_Click(object sender, RoutedEventArgs e)
    {
        _vm?.CancelAddParticipant();
    }

    // ── Reporting (moderation.md § User-initiated reporting) ─────

    /// <summary>The report sheet's failure line, held across observer ticks (see the
    /// error surface in <see cref="Refresh"/>); <c>null</c> when there is none.</summary>
    private string? _reportError;

    private void ReportPageError(string message)
    {
        _reportError = message.Length > 0 ? message : null;
        Refresh();
    }

    /// <summary>Open the shared report sheet on the received message
    /// <paramref name="m"/>. A landed report is acknowledged OUTSIDE the closed sheet
    /// (<c>report-status</c>); the sheet has already stored the reporter-side hide, so
    /// the re-bind below paints "You reported this" at once. A failed send keeps the
    /// sheet open and reports on <c>error-message</c>; a failed block/hide lands there
    /// BESIDE the acknowledgement.</summary>
    private async Task OpenReportSheetAsync(MessageSnapshot m)
    {
        if (_rpc is null || m.planeRef is not { } plane) return;
        var target = uniffi.fauna_ffi.FaunaFfiMethods.ReportMessageTarget(
            plane.scope, plane.recordDigest, SenderActorHex(m.sender), m.body);
        // None for a mail or bridged message — it paints no report verb at all.
        if (target is null) return;
        ReportStatusText.Visibility = Visibility.Collapsed;
        _reportError = null;
        var outcome = await Controls.ReportSheetDialog.OpenAsync(
            this.XamlRoot, _rpc, target, ReportPageError);
        if (outcome is null) return;
        ReportStatusText.Text = outcome.Acknowledgement ?? "";
        ReportStatusText.Visibility = Visibility.Visible;
        _reportError = outcome.FollowUpError;
        // The hide list changed: re-bind the messages (each verdict is read at bind).
        Refresh();
    }

    // ── Helpers ──────────────────────────────────────────────────

    /// <summary>The sender's lowercase-hex actor id for a Fauna-rail address, else
    /// <c>null</c> — a mail or bridged sender has none. Routes a report to the
    /// sender's home nest and keys the hide of everything they sent.</summary>
    private static string? SenderActorHex(TypedAddress sender) =>
        sender is TypedAddress.Fauna fauna
            ? Convert.ToHexString(fauna.@actorId).ToLowerInvariant()
            : null;

    private static FaunaApp.Controls.DmMessageView ToMessageView(MessageSnapshot m, bool selected)
    {
        var from = string.IsNullOrEmpty(m.senderDisplay) ? FaunaConversationsMethods.TypedAddressDisplay(m.sender) : m.senderDisplay;
        // Guardian Notify (family-safety.md § Guardian Notify): count any
        // GUARDIAN-floor enforcement on this message — never the own-threshold
        // collapse the ContentVerdict below also carries. A no-op unless the
        // ward's content_notify knob is on (GuardianNotifyCache's own gate).
        var guardianEnforcedCategories = uniffi.fauna_ffi.FaunaFfiMethods.GuardianEnforcedCategories(
            m.labels, FaunaApp.Core.Services.ContentPolicyCache.Current.ContentPolicy);
        if (guardianEnforcedCategories.Length > 0)
            FaunaApp.Core.Services.GuardianNotifyCache.Record(m.messageId, guardianEnforcedCategories);
        // The viewer's own reports are the verdict's third input (moderation.md §
        // Corollary): a reported message hides under the plane record DIGEST — the
        // id a report names, never the message id — and a reported sender's messages
        // hide with them. Null for a message with no plane ref (mail, bridged): it
        // can be neither reported nor hidden.
        var reportKey = m.planeRef?.recordDigest;
        var region = FaunaApp.Core.Services.ContentPolicyCache.RenderFor(
            m.labels, FaunaApp.Core.Services.RegionSubject.Message(
                m.messageId, m.body, reportKey, SenderActorHex(m.sender)));
        return new FaunaApp.Controls.DmMessageView(
            Id: m.messageId,
            From: from,
            Encrypted: m.badges.encrypted,
            Signed: m.badges.signed,
            Verified: m.badges.verified,
            // Per-message bubble timestamp through the SAME shared bucketer the
            // conversation-list row uses (value-formatting.md § Conversation
            // timestamp); we only supply live `now` + the machine's local UTC
            // offset, via the app's one named door (value-formatting.md §
            // Absolute local timestamp display — the app-side one-door rule).
            Timestamp: ValueFormat.ConversationTimestamp(
                DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
                m.timestampMs,
                DeviceOffset.UtcOffsetSeconds()),
            // Reactions & message delete (conversations.md § Reactions & message
            // delete): the manager folds the reaction log + deleted tombstone onto
            // the snapshot, so the bubble renders them off MessageSnapshot directly.
            // ReactionGroup is internal (UniFFI) — map to the bubble's public
            // ReactionGroupVm so the public render surface carries no internal type.
            IsOwn: m.isOwn,
            CanDelete: m.canDelete,
            Deleted: m.deleted,
            Reactions: m.reactions
                .Select(r => new FaunaApp.Controls.ReactionGroupVm(r.emoji, r.count, r.reactedByMe))
                .ToList(),
            // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1): the
            // shared poll_inbound_conv projects a withheld message into a tombstone snapshot
            // carrying legalTakedownRef; the bubble collapses to the shared tombstone.
            LegalTakedownRef: m.legalTakedownRef,
            // Muted-keyword collapse (moderation.md § Muted keywords;
            // content-moderation-and-ranking.md § Q3): client-only, post-decrypt match
            // over the session-local MutedKeywordsCache (never shared-Rust state) — the
            // ONE call that combines the shared matcher with the session reveal set.
            Muted: FaunaApp.Core.Services.MutedKeywordsCache.IsMuted(m.body, m.messageId),
            // Content-policy render enforcement (family-safety.md § Content policy): the
            // conversations render is one of the TWO surfaces the policy binds at (the feed
            // read model is the other). The verdict composes the guardian's per-category
            // floor with the viewer's own spam/phishing thresholds strictest-wins ENTIRELY in
            // shared Rust — the same one call FeedPostItem makes, so a floor can never be
            // enforced differently on a message than on a post.
            //
            // The region content policy composes in as the third source
            // (region-blocking.md § Where it composes): one call, the verdict and —
            // when the region drove it — the placeholder painted ahead of the family arm.
            ContentVerdict: region.Verdict,
            Region: region.Placeholder,
            // The viewer's own report is what hid it: the blocked-notice then reads
            // "You reported this" (`source="reported"`), not the family-policy words.
            Reported: region.Reported,
            // The report verb is offered on a RECEIVED message that has a plane
            // identity to be reported against (the shared report_message_target
            // answers none for mail / bridged — the verb paints nothing there).
            CanReport: !m.isOwn && reportKey is not null,
            // ...and its OWN session reveal set (separate from the muted one above), keyed
            // on the message id so a Refresh re-bind keeps a revealed message revealed.
            ContentRevealed: FaunaApp.Core.Services.ContentPolicyCache.IsRevealed(m.messageId),
            // The `SearchNav.Mail` deep-link marker (conversations.md § The
            // selected message) — read-time-resolved by the caller off
            // `ThreadDetail.selectedMessageId`, never re-derived here.
            Selected: selected);
    }

    public async void ShowImageLightbox(BitmapImage image)
    {
        ImageLightboxDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(ImageLightboxDialog, prepare: () => LightboxImage.Source = image);
    }
}

/// <summary>
/// XAML-friendly wrapper around <see cref="ThreadSummary"/>. Exposes only
/// public-typed properties (string, uint, Visibility) so the
/// <c>conversation-item</c> DataTemplate can resolve <c>x:DataType</c>
/// at runtime — internal classes here cause WinUI 3's ListView container
/// generator to skip row materialization (rows render zero), even though
/// x:Bind compile-time codegen succeeds. The constructor and the
/// <see cref="ProtocolGlyphFor"/> helper stay <c>internal</c> so the
/// UniFFI-generated <c>internal</c> types (<see cref="ThreadSummary"/>,
/// <see cref="Rail"/>) don't leak through a public surface.
/// </summary>
public sealed class ThreadRow
{
    internal ThreadRow(ThreadSummary t)
    {
        ThreadIdValue = t.threadId;
        // Display label only — empty-label fallback single-sourced in shared Rust
        // (thread_label_display): blank → "(no subject)", non-empty verbatim. The
        // raw label stays the rename value (read straight off SelectedDetail.label).
        // conversations.md § Where logic lives → Thread label display.
        Label = Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ThreadLabelDisplay(t.label));
        Snippet = t.snippet ?? "";
        UnreadCount = t.unreadCount;
        ProtocolGlyph = ProtocolTextFor(t.glyph, t.bridge);
        ProtocolBridgeId = t.bridge?.id ?? "";
        GuardianLabel = t.guardianState is { } gs
            ? uniffi.fauna_conversations.FaunaConversationsMethods.GuardianStateLabel(gs)
            : "";
        GuardianToken = t.guardianState is { } gt
            ? uniffi.fauna_conversations.FaunaConversationsMethods.GuardianStateAttrToken(gt)
            : "";
        GuardianVis = t.guardianState is null ? Visibility.Collapsed : Visibility.Visible;
        UnreadVis = t.unreadCount > 0 ? Visibility.Visible : Visibility.Collapsed;
        // Shared contextual last-activity timestamp (today → local clock /
        // Yesterday / weekday / older → date), per conversations.md § Layout +
        // value-formatting.md § Conversation timestamp. The calendar bucketing
        // lives in shared Rust (ValueFormat.ConversationTimestamp →
        // conversation_timestamp_display); this row only supplies the live `now`
        // and the machine's local UTC offset, via the app's one named door
        // (value-formatting.md § Absolute local timestamp display). No
        // hand-rolled time logic here.
        Timestamp = ValueFormat.ConversationTimestamp(
            DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            t.lastActivityMs,
            DeviceOffset.UtcOffsetSeconds());
    }

    public string ThreadIdValue { get; }
    public string Label { get; }
    public string Snippet { get; }
    public uint UnreadCount { get; }
    /// <summary>The <c>protocol-icon</c> text: the snapshot's glyph, and on a bridged
    /// room the bridge's declared label beside it.</summary>
    public string ProtocolGlyph { get; }

    /// <summary>The bridge id on <c>protocol-icon</c>'s <c>bridge</c> attribute
    /// (HelpText); empty off a bridged room.</summary>
    public string ProtocolBridgeId { get; }

    /// <summary><c>conversation-guardian-state</c>: the shared label, the wire word
    /// on its <c>state</c> attribute (HelpText), and Collapsed — absent — while the
    /// nest reports no state for the room's peer.</summary>
    public string GuardianLabel { get; }
    public string GuardianToken { get; }
    public Visibility GuardianVis { get; }
    public Visibility UnreadVis { get; }

    /// <summary>The shared contextual last-activity timestamp shown in the row's
    /// top-right (conversations.md § Layout list-row timestamp).</summary>
    public string Timestamp { get; }

    /// <summary>The indexed <c>protocol-icon</c> glyph for a thread's source concept,
    /// resolved through the shared <c>SourceGlyph</c> token (render-model.md § Deltas
    /// → D5) rather than a per-app <c>Rail</c> switch. Both call sites pass the
    /// snapshot's precomputed <c>glyph</c> (<c>ThreadSummary.glyph</c> /
    /// <c>ThreadDetail.glyph</c>); windows keeps only the one <c>SourceGlyph → emoji</c>
    /// map (<c>SourceGlyphAsset</c>), shared with the feed badge so the rail and badge
    /// can't drift.</summary>
    internal static string ProtocolGlyphFor(uniffi.fauna_core.SourceGlyph glyph)
        => FaunaApp.Core.Helpers.SourceGlyphAsset.Emoji(glyph);

    /// <summary>The glyph plus, on a bridged room, the bridge's DECLARED label so two
    /// bridges sharing a glyph read apart (<c>conversations.md</c> § Where logic
    /// lives → <i>The <c>Bridged</c> adapter</i>, ruling 2 (a)). No app-side mapping.</summary>
    internal static string ProtocolTextFor(
        uniffi.fauna_core.SourceGlyph glyph, uniffi.fauna_core.BridgeIdentitySnapshot? bridge)
        => bridge is null ? ProtocolGlyphFor(glyph) : $"{ProtocolGlyphFor(glyph)} {bridge.label}";
}

/// <summary>
/// One member's row in the room policy editor — the admin switch and the
/// hand-over control, indexed like the chips. Every value on it is READ OFF THE
/// SHARED DRAFT at rebuild time (<c>RoomSettingsIsEligible</c> /
/// <c>RoomSettingsAdminAt</c> / <c>RoomSettingsTransferStagedAt</c>), so nothing
/// here tracks state of its own and the at-most-one-staged hand-over rule needs
/// no per-widget bookkeeping.
/// </summary>
public sealed class RoomMemberRow : INotifyPropertyChanged
{
    internal RoomMemberRow(int index, string display)
    {
        Index = index;
        Display = display;
    }

    /// <summary>The row's index in the roster the editor painted — what the
    /// toggles carry in their Tag and hand back to the draft, which resolves it
    /// to an identity rather than trusting the position.</summary>
    public int Index { get; }

    public string Display { get; }

    public bool AdminStaged { get; private set; }

    public bool TransferStaged { get; private set; }

    /// <summary>Greyed unless the viewer may appoint admins AND this row is
    /// eligible at all (a Fauna member who is not the owner) — never hidden.</summary>
    public bool AdminEnabled { get; private set; }

    /// <summary>Greyed unless the viewer may transfer ownership AND this row is
    /// eligible — never hidden, and never live on the owner's own row.</summary>
    public bool TransferEnabled { get; private set; }

    /// <summary>The `checked` attribute, carried in HelpText.</summary>
    public string AdminChecked => AdminStaged ? "true" : "false";

    /// <summary>The `checked` attribute, carried in HelpText.</summary>
    public string TransferChecked => TransferStaged ? "true" : "false";

    public string AdminLabel => RoomLabels.AdminToggleLabel(AdminStaged);

    public string TransferLabel => RoomLabels.TransferToggleLabel(TransferStaged);

    public event PropertyChangedEventHandler? PropertyChanged;

    /// <summary>
    /// Re-read this row off the staged draft, IN PLACE.
    ///
    /// <para>In place is the whole point: the row objects — and therefore the
    /// two ToggleButtons and their UIA automation peers — must SURVIVE a staging
    /// gesture. Rebuilding the list instead would destroy the very button that
    /// raised the click while the automation client that pressed it still holds
    /// a provider reference to it, which is precisely the shape that crashed
    /// this app with a stowed <c>E_UNEXPECTED</c> on <c>BackupsPage</c>
    /// in 2026-09 (7/7 runs — under
    /// UIA, "rebuild the identical thing" is not a no-op). linux can tear its rows
    /// down and rebuild them because GTK has no such peer lifetime; windows
    /// cannot.</para>
    ///
    /// <para>The notifications are raised UNCONDITIONALLY rather than on a
    /// change: a click flips a ToggleButton's own <c>IsChecked</c> locally
    /// before the draft has spoken, so the binding must be re-asserted even when
    /// the staged value did not move — that is what puts a refused or
    /// un-staged toggle back where the draft says it belongs. Re-raising costs a
    /// binding re-evaluation and destroys nothing.</para>
    /// </summary>
    internal void Restage(bool adminStaged, bool transferStaged, bool adminEnabled, bool transferEnabled)
    {
        AdminStaged = adminStaged;
        TransferStaged = transferStaged;
        AdminEnabled = adminEnabled;
        TransferEnabled = transferEnabled;
        foreach (var name in new[]
                 {
                     nameof(AdminStaged), nameof(TransferStaged),
                     nameof(AdminEnabled), nameof(TransferEnabled),
                     nameof(AdminChecked), nameof(TransferChecked),
                     nameof(AdminLabel), nameof(TransferLabel),
                 })
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        }
    }
}

/// <summary>
/// One standing community-room invitation, atop the conversation list
/// (<c>room-invitation[i]</c>). Plain strings and a number — the binding only
/// reflects PUBLIC properties of public types (see <see cref="ChipItem.Address"/>
/// for the measured trap with internal UniFFI types).
/// </summary>
public sealed class RoomInvitationRow
{
    internal RoomInvitationRow(long id, string text, string roleToken)
    {
        Id = id;
        Text = text;
        RoleToken = roleToken;
    }

    /// <summary>The handle accept and decline name it by — opaque, carried in each
    /// button's Tag.</summary>
    public long Id { get; }

    /// <summary>The row's sentence — the shared <c>RoomInvitationText</c>.</summary>
    public string Text { get; }

    /// <summary>The <c>role</c> attribute (<c>admin | member</c>), in HelpText.</summary>
    public string RoleToken { get; }

    public string AcceptLabel => FaunaApp.Core.Services.Strings.Get("common/accept");

    public string DeclineLabel => FaunaApp.Core.Services.Strings.Get("common/decline");
}

/// <summary>
/// One row of the room editor's pending invitations
/// (<c>room-pending-invite[i]</c> + its withdraw button), updated IN PLACE when
/// the served list changes (<see cref="RoomMemberRow.Restage"/> says why).
/// </summary>
public sealed class RoomPendingInviteRow : INotifyPropertyChanged
{
    internal RoomPendingInviteRow(string inviteeActorHex, string text, bool lapsed)
    {
        InviteeActorHex = inviteeActorHex;
        _text = text;
        _lapsed = lapsed;
    }

    private string _text;
    private bool _lapsed;

    /// <summary>The withdraw button's Tag — the row's own invitee, never an index.</summary>
    public string InviteeActorHex { get; }

    public string Text => _text;

    /// <summary>The <c>lapsed</c> attribute (<c>true</c> when the accept door would
    /// refuse the invitation today), in HelpText.</summary>
    public string LapsedToken => _lapsed ? "true" : "false";

    public string WithdrawLabel => FaunaApp.Core.Services.Strings.Get("conversations/unified/room_pending_invite_withdraw");

    public event PropertyChangedEventHandler? PropertyChanged;

    internal void Update(string text, bool lapsed)
    {
        if (text == _text && lapsed == _lapsed) return;
        _text = text;
        _lapsed = lapsed;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Text)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LapsedToken)));
    }
}

/// <summary>
/// One catalog labeler a room may name, in the room editor's labeler set
/// (<c>room-labeler-toggle[i]</c> + <c>room-labeler-inspect-button[i]</c>). Every
/// value is READ OFF THE SHARED DRAFT by <see cref="Restage"/> and the row objects
/// outlive every staging gesture (<see cref="RoomMemberRow.Restage"/>).
/// </summary>
public sealed class RoomLabelerRow : INotifyPropertyChanged
{
    internal RoomLabelerRow(uint catalogIndex, string labelerId, string kind)
    {
        CatalogIndex = catalogIndex;
        LabelerId = labelerId;
        _kind = kind;
    }

    private readonly string _kind;

    /// <summary>The inspect button's Tag: the entry's index into the catalog
    /// SNAPSHOT (not into the filtered rows) — what <c>Inspect</c> is addressed by.</summary>
    public uint CatalogIndex { get; }

    /// <summary>The toggle's Tag: the labeler id the shared draft stages by.</summary>
    public string LabelerId { get; }

    public bool Staged { get; private set; }

    public bool ToggleEnabled { get; private set; }

    /// <summary>The `checked` attribute, carried in HelpText.</summary>
    public string Checked => Staged ? "true" : "false";

    public string Label => RoomLabels.LabelerToggleText(_kind, LabelerId, Staged);

    public string InspectLabel => FaunaApp.Core.Services.Strings.Get("labeler_catalog/inspect");

    public event PropertyChangedEventHandler? PropertyChanged;

    internal bool Matches(uint catalogIndex, string labelerId)
        => CatalogIndex == catalogIndex && LabelerId == labelerId;

    internal void Restage(bool staged, bool toggleEnabled)
    {
        Staged = staged;
        ToggleEnabled = toggleEnabled;
        // Unconditional, for the reason RoomMemberRow.Restage gives: a click flips a
        // ToggleButton's own IsChecked before the draft has spoken.
        foreach (var name in new[] { nameof(Staged), nameof(ToggleEnabled), nameof(Checked), nameof(Label) })
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        }
    }
}

public sealed class ChipItem
{
    internal ChipItem(
        string display,
        string? personHex = null,
        TypedAddress? address = null,
        string? roleToken = null,
        bool removeEnabled = true)
    {
        Display = display;
        PersonHex = personHex;
        _address = address;
        RoleToken = roleToken;
        RemoveEnabled = removeEnabled;
    }

    private readonly TypedAddress? _address;

    public string Display { get; }

    /// <summary>The participant's raw actor id as lowercase hex — set only
    /// while under open review (an unreviewed or non-Fauna chip carries
    /// null). The <c>thread-member-keep-button</c>'s Tag, and what
    /// <see cref="ReviewVisibility"/> gates on.</summary>
    public string? PersonHex { get; }

    /// <summary>The participant's full typed address, bound at paint from
    /// <c>ThreadDetail.participants[i]</c> (index-parallel with the display
    /// strings) — never re-derived by index at tap time, so a roster that
    /// shifts between paint and tap still removes the person the chip named
    /// (<c>TypedAddress::same_participant</c> keys on actor id). The
    /// <c>thread-member-chip</c>'s Tag. <c>null</c> for a recipient-picker
    /// row (add-participant / new-thread), which never binds to remove.</summary>
    /// <summary>Exposed as <c>object?</c>, not <c>TypedAddress?</c> — WinUI's
    /// classic <c>{Binding}</c> (which <c>thread-member-chip</c>'s Tag uses)
    /// only reflects PUBLIC properties, and <c>TypedAddress</c> is an
    /// internal UniFFI type; a public property of an internal type does not
    /// compile (CS0053) and, when worked around with an internal property
    /// instead, the binding silently resolves to null (measured: `thread-member-chip` clicked but Tag read null). The runtime
    /// value is still the real <c>TypedAddress</c>, recovered via pattern
    /// matching (<c>Tag: TypedAddress addr</c>) in the click handler.</summary>
    public object? Address => _address;

    /// <summary>The member's role on a governed room — the <c>role</c>
    /// attribute a driver reads off <c>thread-member-chip[i]</c>, carried in
    /// <c>AutomationProperties.HelpText</c>. <c>null</c> on a policy-less room and
    /// on every non-room thread: there are no roles to mark there, which is not
    /// the same as "everyone is a member".</summary>
    public string? RoleToken { get; }

    /// <summary>Whether this chip's tap removes its participant — the rail's
    /// membership mutable AND the viewer's role permitted. <c>false</c> greys
    /// the chip (<c>ui/conversations.md</c> § Architectural rules 5: greyed,
    /// never hidden), which is also what makes the automation gate refuse to
    /// drive it. <c>true</c> for a recipient-picker row and for every
    /// informational (mail) chip, neither of which this flag speaks for.</summary>
    public bool RemoveEnabled { get; }

    public Visibility ReviewVisibility => PersonHex is null ? Visibility.Collapsed : Visibility.Visible;
}
