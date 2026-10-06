using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Feed;
using FaunaApp.Helpers;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_conversations;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Feed page. Renders entirely off the shared <see cref="FfiFeedManager"/>
/// snapshot via <see cref="FeedViewModel"/> + a UI-thread <see cref="FeedNotifyObserver"/>
/// — the post list, search (a re-query, never a client-side filter), feed
/// create/delete, compose, the embedded quoted-post card, and bridge subscribe
/// all run in shared Rust (feed.md § State &amp; data shape, ratified 2026-06-14).
/// The page owns only the bound <see cref="Posts"/> / <see cref="Feeds"/>
/// projections it rebuilds from the snapshot on every observer tick (the
/// <c>ThreadRow</c> pattern: public wrappers so the DataTemplates materialize).
/// </summary>
public sealed partial class FeedPage : Page
{
    private FeedViewModel? _viewModel;
    private FeedNotifyObserver? _observer;
    private INestRpcClient? _rpc;
    private ICryptoService? _crypto;
    private ISessionAccount? _account;

    /// <summary>The shared-Rust conversations session built at login (null in E2E before
    /// <c>BuildE2eConvSessionAsync</c> lands, or when no session could be built) — the room-post
    /// key seam <see cref="Page_Loaded"/> installs onto the feed manager it builds/resolves
    /// (`ui/feed.md` § Encryption at rest → Room-restricted — the app half). Mirrors
    /// <c>ConversationsPage</c>/<c>DevicesPage</c>/<c>FoldersPage</c>'s own <c>_convSession</c>
    /// field.</summary>
    private ConversationsSession? _convSession;

    /// <summary>Lazily-built, session-lived <c>FfiWebClient</c> for the ⋯-menu's
    /// own-post web-publishing verbs (web-content-hosting.md § Published-post
    /// management) — built over the shared, auto-reconnecting WS-RPC connection
    /// (mirrors <c>SettingsWebPage.EnsureViewModelAsync</c>), never a per-open
    /// one-shot connection.</summary>
    private uniffi.fauna_ffi.FfiWebClient? _webClient;

    /// <summary>
    /// The CURRENT nest HTTP client, resolved per call — never captured.
    /// <c>clients.Nest</c> is disposed by <c>App.DisposeNestClients</c> on the next
    /// re-login while this page instance can outlive that hand-off, so a captured
    /// reference starts throwing <see cref="ObjectDisposedException"/>
    /// (<c>apps/windows.md</c> § Client lifetime). Same rule the static
    /// <see cref="_imageLoader"/> already follows.
    /// </summary>
    private static INestHttpClient? Nest => App.CurrentNest;

    /// <summary>Engagement-cue viewport-dwell capture shell (engagement-cues.md
    /// §§ Cue vocabulary &amp; derivation / At rest; task-6 of the
    /// personalization-port plan) — wired in <see cref="Page_Loaded"/>, stopped
    /// + flushed in <see cref="OnNavigatedFrom"/>.</summary>
    private CueViewportObserver? _cueObserver;

    /// <summary>search.md § User actions (SearchNav.Post) — set when
    /// SearchResultsPage routed a Post search hit here; consumed once in
    /// Page_Loaded, right after the initial hydrate (<see cref="OpenPostDetailByIdAsync"/>).</summary>
    private string? _deepLinkPostId;

#if PAYMENTS
    /// <summary>The flat tip-attribution window, built in code and dropped into
    /// <c>TipListDialogHost</c> — see the field's PAYMENTS wiring in
    /// <c>Page_Loaded</c> and dynamic-features.md § Platform-family surface
    /// excision. Null until the first <c>Page_Loaded</c>.</summary>
    private Views.Payments.PostTipListDialog? _tipListDialog;
#endif

    /// <summary>Page-owned render projections of the snapshot (rebuilt each tick).</summary>
    public ObservableCollection<FeedPostItem> Posts { get; } = new();
    public ObservableCollection<FeedDefinition> Feeds { get; } = new();

    // Singleton loader used by x:Bind on the post-image element. Static so the
    // XAML-compiler-friendly `local:FeedPage.GetImageLoader()` call in FeedPage.xaml
    // has a stable target regardless of which FeedPage instance hosts the row.
    private static BlobImageLoader? _imageLoader;
    public static BlobImageLoader? GetImageLoader() => _imageLoader;

    /// <summary>
    /// Exposes the compose bar for state-protocol injection (e.g. compose.file E2E test command).
    /// </summary>
    internal Controls.FeedComposeBar FeedComposeBar => ComposeBar;

    public FeedPage()
    {
        this.InitializeComponent();
        // A region relay fold that changed the answer re-reads every card: the
        // region placeholder is part of FeedPostItem.ContentEquals, so Refresh
        // rebuilds exactly the rows whose verdict moved (region-blocking.md § How an
        // app obtains its region's policy). Raised off the UI thread — marshal back.
        Loaded += (_, _) => RegionPlaneHost.Changed += OnRegionChanged;
        Unloaded += (_, _) => RegionPlaneHost.Changed -= OnRegionChanged;
    }

    private void OnRegionChanged() => DispatcherQueue.TryEnqueue(Refresh);

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _crypto = clients.Crypto;
            _account = clients.Account;
            _convSession = clients.ConvSession;
            // Resolve the CURRENT nest client per call — never capture `clients.Nest`.
            // This loader is static (below), so it outlives every ServiceClients
            // hand-off; the client it was handed gets disposed on the next re-login.
            // Resolve the feed manager per call too: it is built later (Page_Loaded,
            // below) than this loader, is rebuilt on re-auth, and holds the per-post keys
            // that open a tier-restricted post's attachments.
            _imageLoader ??= new BlobImageLoader(
                () => App.CurrentNest, () => Core.FeedManagerHost.Current);
            _deepLinkPostId = clients.DeepLinkPostId;

            ComposeBar.PostRequested += OnComposePostRequested;
            ComposeBar.ComposeDialogRequested += () => ComposeDialog_Click(this, new RoutedEventArgs());
            // Draft-persistence v2, feed leg (reserved-folders.md § Drafts Sync):
            // live-forward every compose edit into the manager (so
            // DraftsSnapshotBytes() reflects it) and schedule a debounced save.
            // _viewModel is null until Page_Loaded builds the manager — a keystroke
            // before then has nothing to forward into yet.
            ComposeBar.ComposeChanged += (text, tags) =>
            {
                Core.Logs.E2eTrace.Write($"[feed-drafts] ComposeChanged forwarded: text len={text.Length}, tags len={tags.Length}, _viewModel={( _viewModel is null ? "null" : "set")}, App.FeedDrafts={(App.FeedDrafts is null ? "null" : "set(" + App.FeedDrafts.GetHashCode() + ")")}");
                // Content-only edit — carries the already-staged attachment through
                // untouched (ui/feed.md § Persistence → Attachments by content address).
                _viewModel?.UpdateComposeText(text, tags);
                App.FeedDrafts?.ScheduleSave();
            };
            // The user's OWN attachment pick/remove gesture — the ONE path allowed to
            // change attached_file; ComposeChanged above must never touch it.
            ComposeBar.AttachmentStaged += (attached) =>
            {
                _viewModel?.UpdateCompose(ComposeBar.ComposeText, ComposeBar.TagsText, attached);
                App.FeedDrafts?.ScheduleSave();
            };
            // The audience, AS IT IS PICKED (ui/feed.md § Persistence → Only user-authored
            // input rests): a half-written post's tier / room / sale and its teaser ride the
            // same draft rail as its text. The submit still re-stages it (the ordering rule
            // in OnComposePostRequested) — this is persistence, not the submit's source.
            ComposeBar.AudienceChanged += (audience) =>
            {
                if (_viewModel is null) return;
                StageAudience(_viewModel, audience);
                App.FeedDrafts?.ScheduleSave();
            };
            ComposeBar.GatePreviewChanged += (preview) =>
            {
                _viewModel?.UpdateComposePreview(preview);
                App.FeedDrafts?.ScheduleSave();
            };
        }
        if (MainPage.Current is not null) MainPage.Current.ActiveFeedPage = this;
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        // Stop the cue-capture tick + scroll hook and flush every tracked card —
        // mirrors linux's unmap handling (off-screen is off-viewport). Must run
        // before the page is discarded: FeedPage isn't cached
        // (NavigationCacheMode stays Disabled, the default), so a live
        // DispatcherTimer left running would otherwise keep firing forever,
        // sampling a detached ListView, and pin this whole page in memory.
        _cueObserver?.Unwire();
        base.OnNavigatedFrom(e);
        // Unsubscribe the VM's reconnect re-hydrate handler — the INestRpcClient
        // seam is app-lifetime, so an un-cleaned VM would leak (and re-fetch into
        // an orphaned snapshot observer).
        _viewModel?.CleanupReconnect();
        // Draft-persistence v2, feed leg: navigating away destroys the compose
        // control, so flush a still-pending debounced save NOW in case the
        // debounce window hadn't elapsed — fire-and-forget, best-effort (mirrors
        // ConversationsPage.Page_Unloaded's same flush for its own rail).
        // Pending-only (not unconditional): FfiFeedManager rebuilds on every
        // Feed-page load, so a torn-down instance nothing was ever typed into
        // has nothing armed — an unconditional save here wrote a spurious empty
        // snapshot indistinguishable from a real one to anything polling the
        // rail (see FeedDraftsService.FlushIfPendingAsync).
        _ = App.FeedDrafts?.FlushIfPendingAsync();
        if (MainPage.Current is not null && ReferenceEquals(MainPage.Current.ActiveFeedPage, this))
            MainPage.Current.ActiveFeedPage = null;
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // Page identity, because "which FeedPage am I looking at" is a recurring
        // diagnostic question here: this page is NOT cached (NavigationCacheMode
        // stays Disabled), so every navigation back to the feed runs a new
        // instance through this handler. The manager it observes no longer
        // follows that lifetime (Core.FeedManagerHost), which is exactly the
        // distinction this line makes readable in a trace.
        Core.Logs.E2eTrace.Write($"[feed-page] Page_Loaded enter instance={GetHashCode()} rpc={(_rpc is null ? "null" : "set")}");
        if (_rpc is null) return;
        _pageError = null; // a fresh load supersedes any prior fire-and-forget failure
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;

        // Build the manager + initial hydrate inside a guard: this is an `async void`
        // event handler, so an un-caught throw (e.g. a transient connect failure)
        // would vanish silently and leave the page with a null VM — the failure mode
        // the pre-snapshot LoadCommand's internal try/catch used to prevent.
        try
        {
            _observer = new FeedNotifyObserver();
            // The session's ONE manager (FeedManagerHost), not a fresh instance
            // per page load: feed.md § State & data shape makes FeedManager the
            // direct analogue of ConversationsManager, whose windows host is
            // already session-scoped, and the ratified rebuild boundary is
            // re-auth/account-switch. Keyed on `_rpc`, so a re-auth (a fresh
            // NestRpcClient) still rebuilds.
            var manager = await Core.FeedManagerHost.GetOrBuildAsync(
                _rpc, () => _rpc.BuildFeedManagerAsync());
            // The room-post key seam (ui/feed.md § Encryption at rest → Room-restricted —
            // the app half): install it wherever the feed manager and the conversations
            // session first coexist. Production always has a session here already (built
            // at login, before StartMainAppAsync ever navigates into the app); the e2e path
            // can reach here BEFORE its own session finishes building, in which case
            // App.BuildE2eConvSessionAsync backfills it onto this manager on completion.
            // Idempotent — safe to re-install on every load of a manager that persists
            // across Feed-page navigations. Must run before ReloadCommand below: refresh_feeds
            // reads own_rooms through this seam.
            if (_convSession is not null)
            {
                manager.SetRoomPostKeys(_convSession);
            }
            _viewModel = new FeedViewModel(manager, _observer, _rpc);
            _viewModel.PropertyChanged += (_, _) => Refresh();
            PostsList.ItemsSource = Posts;
            FeedList.ItemsSource = Feeds;

#if PAYMENTS
            // §§ TipDisplayHost / TipListDialogHost — the payments plane lives in its
            // own removable build item (dynamic-features.md § Platform-family surface
            // excision); wire it the same way ProfilePage wires PaymentsAuthorSections,
            // one seam further out since this one is per-card rather than a single
            // section.
            _tipListDialog = new Views.Payments.PostTipListDialog();
            TipListDialogHost.Content = _tipListDialog;
            PostsList.ContainerContentChanging += PostsList_ContainerContentChanging;
#endif

            // Draft-persistence v2, feed leg (reserved-folders.md § Drafts Sync;
            // feed.md § Persistence): pair the sync handle held at login with THIS
            // freshly-built manager — FfiFeedManager rebuilds every Feed-page load,
            // unlike the conversations manager, so App.FeedDrafts is rebuilt here
            // too (android's shape: re-run restore per fresh manager rather than
            // once per session). Retire any previous instance's pending debounce
            // timer first — it is bound to a now-stale manager. Best-effort:
            // App.FeedDraftsSync can be null (not logged in this way, or the login
            // wiring hasn't finished) and a restore failure must never block the
            // feed page loading.
            App.FeedDrafts?.Dispose();
            Core.Logs.E2eTrace.Write($"[feed-drafts] Page_Loaded: App.FeedDraftsSync={(App.FeedDraftsSync is null ? "null" : "set")}");
            if (App.FeedDraftsSync is { } feedDraftsSync)
            {
                var feedDrafts = new FeedDraftsService(feedDraftsSync, new ManagerFeedDraftStore(manager));
                App.FeedDrafts = feedDrafts;
                try
                {
                    await feedDrafts.RestoreOnLaunchAsync();
                    var restoredCompose = manager.Snapshot().@compose;
                    Core.Logs.E2eTrace.Write($"[feed-drafts] Page_Loaded: restored text={restoredCompose.@text.Length} chars, tags={restoredCompose.@tags.Length} chars");
                    // Page_Loaded can fire more than once per login (a repeat "feed"
                    // nav command rebuilds the manager each time), and this fresh
                    // manager instance knows nothing about text the user already
                    // typed before it existed. RestoreDraftIfEmpty only writes into
                    // empty fields (never clobbers in-progress typing) and suppresses
                    // ComposeChanged for its own assignment (never a restore->forward
                    // ->re-save loop). For the skipped (already-typed) case, sync the
                    // CURRENT UI content into the fresh manager explicitly — the
                    // suppressed assignment above cannot reach it via the event.
                    var alreadyTyped = !string.IsNullOrEmpty(ComposeBar.ComposeText) || !string.IsNullOrEmpty(ComposeBar.TagsText);
                    ComposeBar.RestoreDraftIfEmpty(restoredCompose.@text, restoredCompose.@tags);
                    // The audience's text fields come back here too; its ANSWER is painted by
                    // Refresh (PaintAudience), once the tier/room it names is offered.
                    ComposeBar.RestoreAudienceFieldsIfUntouched(restoredCompose);
                    if (alreadyTyped)
                    {
                        // Content-only sync — carries the just-restored attachment
                        // through rather than dropping it (ui/feed.md § Persistence →
                        // Attachments by content address).
                        _viewModel?.UpdateComposeText(ComposeBar.ComposeText, ComposeBar.TagsText);
                    }
                }
                catch (System.Exception ex)
                {
                    Core.Logs.E2eTrace.Write($"[feed-drafts] Page_Loaded: restore threw: {ex.GetType().Name}: {ex.Message}");
                }
            }

            // Engagement-cue capture shell (engagement-cues.md §§ Cue vocabulary &
            // derivation / At rest, task-6): wire the viewport observer BEFORE
            // hydrating — mirrors linux's wire() then hydrate_cues() sequencing
            // (puts are suppressed pre-hydrate anyway, so the ordering between
            // wiring and hydrate-completing is not itself a race). A hydrate
            // failure (an unopenable rollup, never a merely-absent one) propagates
            // to the catch below and surfaces on error-message, same as every
            // other load-time failure here.
            _cueObserver = new CueViewportObserver(PostsList, () => _viewModel?.Manager, ReportPageError);
            _cueObserver.Wire();
            await manager.HydrateCues();

            // Initial hydrate: refresh the selector + bridge-feed lists and load the
            // current (local) feed's first page. The snapshot observer drives Refresh().
            await _viewModel.ReloadCommand.ExecuteAsync(null);
            Refresh();

            // search.md § User actions (SearchNav.Post) — fire-and-forget: the
            // dialog it opens blocks on ShowAsync until the user closes it, and
            // awaiting it here would leave LoadingRing spinning (and the
            // `finally` below unreached) for as long as the dialog stays open.
            if (_deepLinkPostId is { } deepLinkPostId)
            {
                _deepLinkPostId = null;
                _ = OpenPostDetailByIdAsync(deepLinkPostId);
            }
        }
        catch (System.Exception ex)
        {
            ShellLog.Error(nameof(FeedPage), $"feed page load failed: {ex.Message}");
            var msg = Core.Services.Strings.Error(ex);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Surfaces a background/fire-and-forget failure on this page's
    /// <c>error-message</c> — the same mirror + bar every other load/action failure
    /// here uses. Two callers today: a <see cref="_cueObserver"/> emit-time failure
    /// (a <c>RecordObservation</c> RPC error) and the gated-post detail unseal
    /// (<see cref="UnlockGatedDetailAsync"/>). Both run on the UI thread — neither
    /// uses <c>ConfigureAwait(false)</c>, so their continuations resume here the
    /// same way every other awaited manager call on this page does.</summary>
    private void ReportPageError(string message)
    {
        _pageError = message;
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = message;
    }

    /// <summary>The last fire-and-forget failure reported via <see cref="ReportPageError"/>.
    /// Held separately from the snapshot's own error because <see cref="Refresh"/> runs on
    /// every observer tick and would otherwise clear it on the next tick — see the note
    /// there. Reset on each page load.</summary>
    private string? _pageError;

    /// <summary>Rebuild every bound surface from the latest snapshot. Cheap; runs
    /// on every observer tick (mirrors ConversationsPage.Refresh()).</summary>
    private void Refresh()
    {
        if (_viewModel is null) return;

        SyncFeeds(_viewModel.Feeds);
        SyncPosts(_viewModel.Posts);
        SyncBridgeFeeds(_viewModel.BridgeFeeds);
        SyncAvailableBridges(_viewModel.AvailableBridges);
        // Keep the compose-gate-tier-select options current with the author's own tiers
        // (snapshot.own_tiers, refreshed for free by refresh_feeds → refresh_own_tiers); the
        // compose bar rebuilds its option set only on an actual change (feed.md § Encryption
        // at rest — gate-to-tier authoring).
        ComposeBar.SetGateTiers(_viewModel.OwnTierNames);
        // The room segment of the same select (snapshot.own_rooms) — re-read alongside the
        // tiers on every feed observer tick AND on the conversations plane's own change tick
        // (RoomsRefreshObserver), so a room joined/left reaches the composer without
        // re-entering the feed (ui/feed.md § Encryption at rest → Room-restricted — the app
        // half, *The rooms offered*).
        ComposeBar.SetOwnRooms(_viewModel.OwnRooms);
        // …and the answer the manager holds, after the options it resolves against — how a
        // restored draft's audience reaches the select (ui/feed.md § Persistence).
        ComposeBar.PaintAudience(_viewModel.Compose);

        // Page-level error (`error-message`) — carried by ErrorBar itself, a
        // CopyableInfoBar whose inner InfoBar stamps the id, so opening the bar is
        // what makes the error observable and closing it is what makes it absent.
        // (The old 1px TextBlock mirror is gone: it was Collapsed, so it realized no
        // UIA peer and the id was unreadable on this page — while the bar the user
        // sees carried no id at all.)
        // A fire-and-forget failure (`_pageError`, see ReportPageError) is NOT snapshot-
        // derived, so it must survive here: Refresh() runs on every observer tick, and
        // clearing on an empty snapshot error used to erase such a failure within
        // milliseconds of it being raised — the reader (and any e2e reading
        // `error-message`) then saw nothing at all. The snapshot error still wins when
        // there is one; otherwise the page error persists until the next load.
        var err = _viewModel.ErrorText;
        if (string.IsNullOrEmpty(err)) err = _pageError;
        if (!string.IsNullOrEmpty(err))
        {
            ErrorBar.Message = err; ErrorBar.IsOpen = true; App.CurrentErrorMessage = err;
        }
        else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }

        // Compose file readiness (`compose-file-ready` + `compose-file-remove`) —
        // rendered off the snapshot's staged attachment, not only a local pick, so a
        // restored draft's handle shows too (ui/feed.md § Persistence → Attachments by
        // content address).
        ComposeBar.RenderAttachedFile(_viewModel.Compose.attachedFile);

        // Composer error (`compose-error`).
        var cerr = _viewModel.ComposeErrorText;
        if (!string.IsNullOrEmpty(cerr))
        {
            ComposeErrorBar.Message = cerr; ComposeErrorBar.IsOpen = true;
            ComposeErrorBar.Visibility = Visibility.Visible;
        }
        else { ComposeErrorBar.IsOpen = false; ComposeErrorBar.Visibility = Visibility.Collapsed; }

        // Loading ring (initial fetch / re-query in flight).
        LoadingRing.IsActive = _viewModel.IsLoading;
        LoadingRing.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;

        // Empty states (`feed-empty-state` / `feed-no-results`) — the shared decision's
        // answer, never this page's own reading of status/posts/search term.
        var empty = _viewModel.EmptyState;
        FeedEmptyStateText.Visibility = empty == FeedEmptyState.NoPosts ? Visibility.Visible : Visibility.Collapsed;
        FeedNoResultsText.Visibility = empty == FeedEmptyState.NoMatches ? Visibility.Visible : Visibility.Collapsed;

        // Push to the E2E state bridge.
        AppDataSnapshot.SetFeedPosts(Posts);
    }

    /// <summary>Rebuild the feed-rail rows from the snapshot's feed list. Guarded by
    /// a feed-id diff so the frequent post-resolution ticks don't churn the rail's
    /// ListView selection.</summary>
    private void SyncFeeds(IReadOnlyList<FeedSummaryView> snapshot)
    {
        var same = snapshot.Count == Feeds.Count
            && snapshot.Select(f => f.feedId).SequenceEqual(Feeds.Select(f => f.FeedId));
        if (!same)
        {
            Feeds.Clear();
            foreach (var f in snapshot)
                Feeds.Add(new FeedDefinition { FeedId = f.feedId, Name = f.name, Combination = f.combination });
        }

        // Restore the rail selection to the snapshot's selected feed without
        // re-firing SelectionChanged (which would re-issue SelectFeed).
        var selectedId = _viewModel?.SelectedFeedId;
        FeedList.SelectionChanged -= FeedList_SelectionChanged;
        FeedList.SelectedItem = selectedId is null
            ? null
            : Feeds.FirstOrDefault(f => f.FeedId == selectedId);
        FeedList.SelectionChanged += FeedList_SelectionChanged;

        // Trending pseudo-entry (trending.md § The Trending feed): toggle its
        // selected/unselected visual state from the snapshot. It's a plain Button
        // outside FeedList, so it can't ride the ListView's native SelectedItem
        // highlight the feed-item rows use above — an AccentButtonStyle swap is
        // the windows-idiomatic stand-in (mirrors the create-feed dialog's own
        // AccentButtonStyle-for-primary-action usage).
        var trendingSelected = _viewModel?.TrendingSelected ?? false;
        TrendingFeedButton.Style = trendingSelected
            ? (Style)Application.Current.Resources["AccentButtonStyle"]
            : null;
    }

    /// <summary>Rebuild the post-card rows from the snapshot's ordered, deduplicated
    /// post list, kicking off the lazy per-post media + quoted-post resolution
    /// (mirrors linux render_posts).</summary>
    private void SyncPosts(IReadOnlyList<PostSummary> snapshot)
    {
        // Reconcile the bound rows against the snapshot IN PLACE: an UNCHANGED post keeps its
        // existing FeedPostItem — and so its already-realized container plus any in-flight
        // /api/v1/blob/<hash> image load — while only a post whose rendered content actually
        // changed (media/quote/link-preview folded into the document, counts bumped, a remote
        // image revealed) rebuilds its row to paint the change. The prior Posts.Clear()+rebuild
        // every observer tick tore down a stable post's in-flight image fetch before it settled
        // (each fire-once resolve trigger below re-emits a notify → Refresh → SyncPosts), so the
        // post-image never appeared — the feed image-render gap (render-model.md § D6; feed.md
        // § State & data shape). Generalizes the SyncFeeds/SyncAvailableBridges id-diff guard
        // to per-row content via FeedPostItem.ContentEquals.
        var target = new List<FeedPostItem>(snapshot.Count);
        foreach (var p in snapshot)
            target.Add(new FeedPostItem(p, _viewModel?.Manager));
        ObservableCollectionReconcile.Reconcile(
            Posts, target, item => item.PostId, static (a, b) => a.ContentEquals(b));

        // Kick off the lazy per-post media + quoted-post + link-preview resolution. Each trigger
        // is fire-once (the guards below); the manager folds the resolved block + re-emits, and
        // the reconcile above then rebuilds ONLY that post's row to paint the embed, leaving
        // every sibling row (and its in-flight load) in place.
        foreach (var p in snapshot)
        {
            // Trigger media resolution: the manager resolves the blob hash, folds an `Image`
            // block into the document + re-emits → next Refresh rebuilds the row and paints the
            // post-image from the document (render-model.md § D6). Fire-once on the unresolved
            // sibling hash so the notify loop terminates once it's set.
            if (p.hasMedia && p.mediaHash is null && _viewModel is not null)
                _ = _viewModel.ResolveMediaAsync(p.postId);
            // Trigger the quoted-post fold: the manager folds a `QuotedPost` block into the
            // document + re-emits **idempotently** → next Refresh repaints the quoted-post card
            // from the document. Fire-once — skip once the block is already folded (mirrors
            // linux/android; the idempotent re-emit is the loop-safety backstop, not the gate).
            if (p.quotedPostId is { } qid && DocumentRenderer.QuotedPost(p.document) is null && _viewModel is not null)
                _ = _viewModel.ResolveQuotedPostAsync(qid);
            // A REPOST ROW's original folds in through the SAME QuotedPost embed —
            // resolve_quoted_post resolves either field, so a repost target needs no new
            // render machinery, just the same fire-once trigger keyed on reposted_post_id
            // instead of quoted_post_id (feed.md § Interaction bar → Repost, ratified
            // 2026-08-10).
            if (p.repostedPostId is { } rid && DocumentRenderer.QuotedPost(p.document) is null && _viewModel is not null)
                _ = _viewModel.ResolveQuotedPostAsync(rid);
            // Trigger link-preview resolution (render-model.md § D4): for EACH folded
            // `LinkPreview` block still in `Resolving`, the shared
            // `FeedManager::resolve_link_preview` resolves the url + re-emits → next Refresh
            // paints a `link-preview-card` per `Resolved` state. Fire-once on the `Resolving`
            // state — once terminal the block is `Resolved`/`Failed`, so this won't re-fire (the
            // cached, idempotent resolve is the in-flight backstop, mirroring the media/quoted
            // triggers above). The shared `resolving_link_preview_urls` face yields every
            // unresolved url in body order, so a post with two bare URLs resolves BOTH — the
            // single-preview `FirstOrDefault` twin this replaced would have stranded the second
            // in `Resolving` forever (mirrors linux's identical loop).
            if (_viewModel is not null)
            {
                foreach (var url in DocumentRenderer.ResolvingLinkPreviewUrls(p.document))
                    _ = _viewModel.ResolveLinkPreviewAsync(url);
            }
            // Trigger the sold-post buyer teaser resolve (gap (2c), monetization.md § Per-post
            // pay-to-unlock): the manager folds a resolved offer into `unlock_offer` + re-emits
            // → next Refresh paints gated-post-price/-payment-link/-buy-button. Fire-once on
            // unresolved — the shared resolve_post_unlock_offer itself no-ops (no I/O, no
            // re-emit) unless gatedTier names a post-unlock-* tier, so gating on "any gated
            // post with no offer yet" (rather than duplicating the tier-prefix check here) is
            // correct AND cheap, matching linux's identical trigger.
            if (p.gatedTier is not null && p.unlockOffer is null && _viewModel is not null)
                _ = _viewModel.ResolvePostUnlockOfferAsync(p.postId);
#if PAYMENTS
            // Trigger the tip attribution resolve (monetization.md § Tips): no
            // "does this post have tips" signal exists in the feed projection (unlike
            // gatedTier above), so this fires for every post. The shared resolver
            // writes a view on every outcome — including "no tips" and a transport
            // error — so `tips == null` is a safe, terminating fire-once
            // gate (mirrors ResolveMediaAsync). Gated: the resolve itself sends a
            // wire kind excised entirely in a store-safe deployment (mirrors
            // FeedViewModel.ResolvePostTipsAsync's own guard) — reading/displaying an
            // already-resolved p.tips is NOT gated.
            if (p.tips is null && _viewModel is not null)
                _ = _viewModel.ResolvePostTipsAsync(p.postId);
#endif   // PAYMENTS
            // Trigger the C2PA-provenance check (media.md § C2PA provenance): the SAME
            // authenticated blob GET the list-card image already performs also returns
            // the `x-c2pa` header (BlobImageLoader.LoadWithC2paAsync shares its cache
            // with the plain LoadAsync the image binding calls below — no extra
            // request). Fire-once via FeedPostItem's own checked flag: unlike every
            // trigger above, there is no manager/snapshot-side signal to gate on — this
            // is pure client render glue, the same class as linux/android/apple's own
            // per-app c2pa-badge wiring. Looked up post-Reconcile so the check lands on
            // the STABLE row instance a content-unchanged tick kept (never the
            // throwaway `target` item Reconcile may have discarded).
            if (p.hasMedia && p.mediaHash is { Length: > 0 } mediaHash && GetImageLoader() is { } imgLoader)
            {
                var row = Posts.FirstOrDefault(i => i.PostId == p.postId);
                if (row is { C2paChecked: false })
                    _ = TriggerC2paCheckAsync(row, imgLoader, mediaHash);
            }
        }

#if PAYMENTS
        // Tip state that arrived on THIS pass (the resolve above) must reach cards whose
        // container was loaded on an earlier one — PostTipDisplay.Bind snapshots, and a
        // non-recycling container is loaded once. See RefreshTipDisplays.
        RefreshTipDisplays();
#endif   // PAYMENTS
    }

    /// <summary>Resolves <paramref name="hash"/>'s `x-c2pa` hint (cache-shared with the
    /// list-card image's own load, see the trigger in <see cref="SyncPosts"/>) and
    /// records it on <paramref name="item"/>. Safe if the row has since scrolled out of
    /// <see cref="Posts"/> or been replaced by a content-changed reconcile: the result
    /// lands on the SPECIFIC captured instance, never a shared control, so a stale
    /// completion just updates an orphaned view-model with no listener — the class of
    /// bug <c>ImageHashBind</c>'s request-version guard exists for does not arise here.</summary>
    private static async Task TriggerC2paCheckAsync(FeedPostItem item, BlobImageLoader loader, string hash)
    {
        var (_, hasC2pa) = await loader.LoadWithC2paAsync(hash);
        item.SetHasC2pa(hasC2pa);
    }

    /// <summary>Wire the single bridge-feed-unsubscribe affordance to the snapshot's
    /// subscribed bridge feeds (the `bridge-feed-unsubscribe-button` rows). Shown when
    /// at least one bridge feed is subscribed; its Tag carries the row id to remove.</summary>
    private void SyncBridgeFeeds(IReadOnlyList<BridgeFeedView> bridgeFeeds)
    {
        if (bridgeFeeds.Count > 0)
        {
            BridgeFeedUnsubscribeBtn.Tag = bridgeFeeds[0].id;
            BridgeFeedUnsubscribeBtn.Visibility = Visibility.Visible;
        }
        else
        {
            BridgeFeedUnsubscribeBtn.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Drive the bridge-subscribe selector from the nest's available
    /// bridges (the feed-selector capability gate — version-compatibility.md
    /// § Dimension 3). The `bridge-form-bridge-select` options are the
    /// snapshot's `{id,name}` list (NOT a hard-coded protocol list), and the
    /// `bridge-feed-subscribe-toggle` reveal affordance is hidden when the nest
    /// serves no bridges — matching web/linux/android/apple.</summary>
    private void SyncAvailableBridges(IReadOnlyList<AvailableBridge> bridges)
    {
        BridgeFeedSubscribeToggle.Visibility =
            bridges.Count > 0 ? Visibility.Visible : Visibility.Collapsed;

        // Rebuild the option set only when it changed (the frequent post-resolution
        // ticks must not churn the dropdown / drop the user's in-progress selection).
        //
        // Hand-built items, not an ItemsSource: `bridge-form-bridge-select` is a
        // stable-KEY select on every app — the bridge `id` is what `select` takes, the
        // human `name` is paint-only (tui's `BridgeKind`, the same key-vs-display split
        // as the factor and rule-type selects) — and the FlaUI bridge's `Select` matches
        // the item's UIA Name EXACTLY. So Content is the visible name, Tag the id, and
        // the UIA Name the id. An ItemsSource + DisplayMemberPath="name" named every
        // option by its display text, so a cross-app `select("nostr")` never matched
        // `Nostr` (test_feed_bridge_subscribe.py).
        var currentIds = BridgeTypeSelector.Items.OfType<ComboBoxItem>().Select(i => i.Tag as string);
        var same = currentIds.SequenceEqual(bridges.Select(b => b.id));
        if (!same)
        {
            BridgeTypeSelector.Items.Clear();
            foreach (var bridge in bridges)
            {
                var item = new ComboBoxItem { Content = bridge.name, Tag = bridge.id };
                AutomationProperties.SetName(item, bridge.id);
                BridgeTypeSelector.Items.Add(item);
            }
            if (bridges.Count > 0) BridgeTypeSelector.SelectedIndex = 0;
        }

        // A nest that lost its last bridge mid-session: close the open form so a
        // stale, unsubscribable selector can't linger.
        if (bridges.Count == 0)
            BridgeFeedPanel.Visibility = Visibility.Collapsed;
    }

    private void BridgeFeedSubscribeToggle_Click(object sender, RoutedEventArgs e)
    {
        BridgeFeedPanel.Visibility = Visibility.Visible;
    }

    /// <summary>Per-row <c>feed-item</c> click — select THAT row's feed by its own
    /// <c>FeedId</c> Tag, exactly as <see cref="RowDeleteFeed_Click"/> deletes by its
    /// own, never "whichever feed the ListView thinks is selected".
    ///
    /// <para>The row root had to become a Button for this to exist at all: the FlaUI
    /// bridge drives a click through the first UIA pattern the element supports and
    /// falls back to a physical mouse click only when it supports none — which a bare
    /// Grid does. That fallback never moved the ListView's selection, so
    /// <see cref="FeedList_SelectionChanged"/> never fired and
    /// <c>FeedManager::select_feed</c> was never called; the page kept serving the
    /// previously-selected feed with nothing downstream able to tell. <c>SyncFeeds</c> still paints the rail's
    /// highlight from the snapshot, so selection state stays the manager's.</para></summary>
    private async void FeedItem_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is not Button { Tag: string feedId } || string.IsNullOrEmpty(feedId)) return;
        await _viewModel.SelectFeedAsync(feedId);
    }

    private async void FeedList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        // The feed rail's only route into `FeedManager::select_feed`, so a click
        // that never reaches here composes nothing and the page keeps serving
        // the previously-selected feed — indistinguishable from success on a
        // first page (every nest-side key is 0, so a composed feed's first page
        // is the same posts in the same order the local feed already showed).
        // Traced because exactly that silence cost a session.
        var picked = FeedList.SelectedItem as FeedDefinition;
        Core.Logs.E2eTrace.Write(
            $"[feed-select] SelectionChanged: item={picked?.FeedId ?? "<none>"} vm={(_viewModel is null ? "null" : "set")}");
        if (_viewModel is null) return;
        // Deselection (e.g. after delete) leaves the local feed active.
        if (picked is not { } feed) return;
        await _viewModel.SelectFeedAsync(feed.FeedId);
    }

    private async void LocalFeed_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.SelectFeedAsync(null);
    }

    /// <summary>Select the built-in Trending virtual feed (`feed-trending-item`,
    /// trending.md § The Trending feed) — mirrors <see cref="LocalFeed_Click"/>.</summary>
    private async void TrendingFeed_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.SelectTrendingFeedAsync();
    }

    /// <summary>Per-row `feed-delete-button` (tui/apple precedent: one button per
    /// `feed-item`, wired to that row's own feed id — never "whichever feed is
    /// currently selected"). The Tag carries FeedId straight from the row's own
    /// DataTemplate binding, so this never touches ListView selection state.</summary>
    private async void RowDeleteFeed_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is not Button { Tag: string feedId } || string.IsNullOrEmpty(feedId)) return;
        // delete_feed refreshes the feed list + re-selects the local feed in the
        // snapshot; SyncFeeds restores the rail from it.
        await _viewModel.DeleteFeedAsync(feedId);
    }

    private async void RefreshButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.ReloadCommand.ExecuteAsync(null);
    }

    private async void FeedSearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (_viewModel is null) return;
        var query = sender.Text?.Trim() ?? "";
        FeedSearchClearButton.Visibility = string.IsNullOrEmpty(query) ? Visibility.Collapsed : Visibility.Visible;
        // Search is a re-query of the selected feed in shared Rust (feed.md
        // § Where logic lives → Search filter) — NEVER a client-side filter over the
        // loaded list. An empty term clears the search.
        await _viewModel.SetSearchQueryAsync(string.IsNullOrEmpty(query) ? null : query);
    }

    private void FeedSearchClear_Click(object sender, RoutedEventArgs e)
    {
        // Clearing the box fires TextChanged → SetSearchQueryAsync(null) → re-query.
        FeedSearchBox.Text = "";
        FeedSearchClearButton.Visibility = Visibility.Collapsed;
    }

    private async void LoadMoreButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.LoadMoreAsync();
    }

    private async void LikeButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("like", () => _viewModel!.LikeAsync(postId));
    }

    /// <summary>Run one interaction-bar verb and put a rejection on <c>error-message</c>
    /// rather than let it escape the <c>async void</c> click handler unseen:
    /// <see cref="FeedViewModel.VerbErrorCopy"/> reads a stated refusal in the user's
    /// language (words under a restricted post — <c>ui/feed.md</c> § Encryption at rest,
    /// ruling 6) and keeps any other failure's own text. A verb that lands clears a
    /// previous verb's error, as web's does.</summary>
    private async Task RunVerbAsync(string verb, Func<Task> run)
    {
        try
        {
            await run();
            if (_pageError is not null)
            {
                _pageError = null;
                Refresh();
            }
        }
        catch (Exception ex)
        {
            ShellLog.Error(nameof(FeedPage), $"feed {verb} failed: {ex.Message}");
            ReportPageError(FeedViewModel.VerbErrorCopy(ex));
        }
    }

    /// <summary>Opens the reply compose surface (<c>feed-reply-dialog</c>) and, on submit,
    /// composes a real reply through <c>FeedViewModel.ReplyAsync</c> (<c>FfiFeedManager::reply</c>)
    /// — NOT <c>InteractAsync(id, "reply", body)</c>, whose native arm discards the typed text
    /// outright (feed.md § Interaction bar; § Implementation status today). Submit/cancel live as
    /// real <c>Button</c>s in the dialog's <c>Content</c>, mirroring <see cref="CreateFeed_Click"/>'s
    /// create/cancel row: a <c>ContentDialog</c>'s template Primary/Close buttons cannot carry a
    /// custom <c>AutomationId</c> (see the comment on that row), and <c>feed-reply-submit-button</c>
    /// needs one. No-op on blank/whitespace-only text (mirrors linux's <c>build_reply_dialog</c>
    /// guard) — unlike quote, an empty reply is meaningless.</summary>
    private async void ReplyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string postId) return;

        var textBox = new TextBox
        {
            PlaceholderText = S.Get("feed/post/write_reply"),
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            Height = 100,
        };
        AutomationProperties.SetAutomationId(textBox, Ids.FeedReplyTextField);

        ContentDialog? replyDialog = null;
        bool shouldReply = false;

        var buttonRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        var cancelBtn = new Button { Content = S.Get("common/cancel") };
        cancelBtn.Click += (s, _) => replyDialog?.Hide();
        var submitBtn = new Button
        {
            Content = S.Get("common/reply"),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        AutomationProperties.SetAutomationId(submitBtn, Ids.FeedReplySubmitButton);
        submitBtn.Click += (s, _) =>
        {
            if (string.IsNullOrWhiteSpace(textBox.Text)) return;
            shouldReply = true;
            replyDialog?.Hide();
        };
        buttonRow.Children.Add(cancelBtn);
        buttonRow.Children.Add(submitBtn);

        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(textBox);
        panel.Children.Add(buttonRow);

        var dialog = new ContentDialog
        {
            Title = S.Get("common/reply"),
            Content = panel,
            XamlRoot = this.XamlRoot,
        };
        AutomationProperties.SetAutomationId(dialog, Ids.FeedReplyDialog);
        replyDialog = dialog;

        await Controls.Dialogs.ShowAsync(dialog);

        if (shouldReply)
        {
            var body = textBox.Text.Trim();
            await RunVerbAsync("reply", () => _viewModel!.ReplyAsync(postId, body));
        }
    }

    /// <summary>Repost / un-repost toggle — through <c>FeedViewModel.RepostAsync</c>
    /// (<c>FfiFeedManager::repost</c>), NOT the raw <c>InteractAsync(id, "repost")</c>: that
    /// one-way call creates no repost post on a native target and can never take one back
    /// (feed.md § Interaction bar → Repost, ratified 2026-08-10).
    /// Matches <see cref="LikeButton_Click"/>'s shape.</summary>
    private async void RepostButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("repost", () => _viewModel!.RepostAsync(postId));
    }

    /// <summary>Quote-repost — a repost WITH commentary (<c>Reference::Quote</c>) — through
    /// <c>FeedViewModel.QuoteAsync</c> (<c>FfiFeedManager::quote</c>), NOT
    /// <c>InteractAsync(id, "quote")</c>: the nest's native arm never composes a post from
    /// that door (feed.md § Interaction bar; § Implementation status today). Fires with an
    /// empty body — the ratified direct-tap shape (matches tui/linux/web/android/macos/ios);
    /// a commentary composer is a separate fleet-wide follow-on. The nest bumps the target's
    /// <c>quote_count</c> when the composed quoting post lands referencing it.</summary>
    private async void QuoteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string postId) return;
        await RunVerbAsync("quote", () => _viewModel!.QuoteAsync(postId, ""));
    }

    /// <summary>Dispatch <c>load-remote-content-button</c> to the manager (D3): the manager
    /// flips the in-memory reveal set for this post and re-emits; the next <c>Snapshot()</c>
    /// projects <c>RemoteImage.revealed=true</c> → Refresh rebuilds the row →
    /// FeedPostBodyBind repaints with fetched images (render-model.md § D3;
    /// html-mail.md § Security &amp; privacy). NO local flip.</summary>
    private void LoadRemoteContent_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null && sender is FrameworkElement fe && fe.DataContext is FeedPostItem item)
            _viewModel.RevealRemoteImages(item.PostId);
    }

    // ── Sold-post buyer teaser (gap (2c), monetization.md § Per-post pay-to-unlock) ──────

    /// <summary>Open the sold-post offer's external payment_url (<c>gated-post-payment-link</c>)
    /// — mirrors <c>ProfilePage.OfferPaymentLink_Click</c>, plus the shared
    /// <c>uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl</c> guard (F-CL2
    /// anti-phishing-redirect class — the nest/author-supplied url is
    /// untrusted): a non-https url shows <c>subscriptions/unsafe_payment_url</c> on
    /// `error-message` instead of launching.</summary>
    private void UnlockOfferPaymentLink_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem post } || string.IsNullOrEmpty(post.UnlockOfferPaymentUrl))
            return;
        if (!uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl(post.UnlockOfferPaymentUrl))
        {
            var msg = S.Get("subscriptions/unsafe_payment_url");
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
            return;
        }
        FaunaApp.Services.UrlOpener.Open(post.UnlockOfferPaymentUrl, "Feed");
    }

    /// <summary>Buy the sold post's unlock tier off the resolved teaser offer
    /// (<c>gated-post-buy-button</c>) — the existing subscribe flow against the resolved offer's
    /// tier, queued pending the author's own §2 approve. A successful call mutates the
    /// manager's own snapshot + notifies (mirrors <see cref="DispatchTrainAsync"/>'s
    /// error-surfacing convention).</summary>
    private async void BuyUnlockOffer_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button { Tag: FeedPostItem post }) return;
        try
        {
            await _viewModel.BuyUnlockOfferAsync(post.PostId);
        }
        catch (Exception ex)
        {
            var msg = S.Format("feed/error_buy_unlock", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>Per-card <c>TipDisplayHost</c> fill, driven by the host's own
    /// <c>Loaded</c> (see the ContentControl's comment in FeedPage.xaml for why that
    /// and not <c>ContainerContentChanging</c>).
    ///
    /// <para>Declared unconditionally, body under <c>PAYMENTS</c>: a DataTemplate event
    /// handler is resolved against this class at XAML-compile time and XAML carries no
    /// <c>#if</c>, so hiding the whole method behind the excision would break the
    /// store-safe build that the excision exists to produce.</para></summary>
    private void TipDisplayHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is ContentControl host && host.DataContext is FeedPostItem post)
            BindTipDisplay(host, post);
#endif   // PAYMENTS
    }

#if PAYMENTS
    /// <summary>Fill (or first-create) one realized post-card's <c>TipDisplayHost</c>
    /// with a bound <see cref="Views.Payments.PostTipDisplay"/> — the per-card twin of
    /// <c>ProfilePage.Page_Loaded</c>'s single <c>PaymentsSectionsHost</c> fill, done
    /// per container because the plane is per-card rather than a single page section.
    /// Idempotent: the control is created once per host and re-bound thereafter.</summary>
    private void BindTipDisplay(ContentControl host, FeedPostItem post)
    {
        if (host.Content is not Views.Payments.PostTipDisplay display)
        {
            display = new Views.Payments.PostTipDisplay();
            display.OpenTipListRequested += (_, item) => OpenTipList(item);
            host.Content = display;
        }
        display.Bind(post);
    }

    /// <summary>Re-bind every ALREADY-REALIZED card's tip display, after a snapshot pass
    /// has reconciled <see cref="Posts"/>.
    ///
    /// <para>This is the half <c>Loaded</c> cannot cover. <see cref="Views.Payments.PostTipDisplay.Bind"/>
    /// SNAPSHOTS its item's values into <c>Text</c>/<c>Visibility</c> rather than binding
    /// them, and a container on a non-recycling panel is loaded exactly once — so a tip
    /// state that arrives LATER (the `ResolvePostTipsAsync` round trip in
    /// <see cref="SyncPosts"/>, which is the whole live path) would paint nothing without
    /// a pass like this. Containers not yet generated are simply skipped; their own
    /// <c>Loaded</c> binds them with the same fresh item.</para></summary>
    private void RefreshTipDisplays()
    {
        foreach (var post in Posts)
        {
            if (PostsList.ContainerFromItem(post) is not ContentControl container) continue;
            if (container.ContentTemplateRoot is not FrameworkElement root) continue;
            if (root.FindName("TipDisplayHost") is not ContentControl host) continue;
            BindTipDisplay(host, post);
        }
    }

    /// <summary>The virtualizing-panel path, kept deliberately and WIRED BUT DORMANT:
    /// with this list's non-virtualizing <c>ItemsPanel</c> override it does not fire at
    /// all (that is the defect), and it would be the right seam again the moment
    /// the panel becomes virtualizing — recycling reuses a container without re-raising
    /// <c>Loaded</c>. Left in place rather than deleted so a future panel change does not
    /// silently lose the re-bind; do not read its existence as evidence that it runs.</summary>
    private void PostsList_ContainerContentChanging(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        if (args.Item is not FeedPostItem post) return;
        if (args.ItemContainer?.ContentTemplateRoot is not FrameworkElement root) return;
        if (root.FindName("TipDisplayHost") is not ContentControl host) return;
        BindTipDisplay(host, post);
    }

    /// <summary>Open the tip attribution window (monetization.md § Tips) for one
    /// post — flat, not scoped: exactly one open at a time. An inline Border +
    /// Visibility toggle, NOT a <c>ContentDialog</c> — the AdminDnsPage RenameSheet
    /// precedent ("a modal ContentDialog does NOT surface ... to the FlaUI
    /// is_visible tree") held here too: a ContentDialog shape measured
    /// 0x0/isOffscreen the moment this row's own e2e checked <c>is_visible</c> right
    /// after open, with no wait. Shared by the list card's
    /// <c>post-tip-list-button</c> (via
    /// <see cref="Views.Payments.PostTipDisplay.OpenTipListRequested"/>) and the
    /// detail dialog's own tip row. The item's precomputed
    /// <see cref="FeedPostItem.TipListTitle"/> / <see cref="FeedPostItem.TipSenders"/>
    /// are set once at construction (Tips is an immutable snapshot field), so this
    /// just paints them — no fresh read.</summary>
    private void OpenTipList(FeedPostItem post) => _tipListDialog?.Show(post.TipListTitle, post.TipSenders);
#endif

    // ── Trained-topic training verbs (post-card ⋯ overflow;
    //    topic-factors.md § Training signals) ─────────────────────────────

    /// <summary>Open post's ⋯ overflow (<c>feed-post-actions-button</c> →
    /// <c>feed-post-actions-menu</c>): the two training verbs, own-post delete, and
    /// the own-post web-publishing verbs (web-content-hosting.md § Published-post
    /// management), checked against the post's current example marker for the
    /// feed's in-context training target (<c>FeedManager::train_target_factor</c> —
    /// a feed whose composition singles out one trained factor trains it directly;
    /// otherwise the verb opens the factor-target sheet). Mirrors
    /// <c>DmMessageBubble.ShowDeleteConfirm</c>'s explicit <c>ShowAt</c> (not a
    /// pre-assigned <c>Button.Flyout</c> — this menu's content depends on state
    /// read fresh at open time).
    ///
    /// <c>async</c> so the web verbs' origin (whether the actor has a serving
    /// address to build copy links on) is resolved BEFORE the menu paints — tui's
    /// trap #2: painting a *disabled* copy verb off unread state tells a creator
    /// with a working address that they have none, and a <c>MenuFlyoutItem</c>'s
    /// state can't be live-patched once shown the way a data-bound row can.</summary>
    private async void PostActionsButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button { Tag: FeedPostItem post } btn) return;
        var manager = _viewModel.Manager;
        var targetFactor = manager.TrainTargetFactor();

        // Own-post gates (feed.md § State & data shape → Post deletion, IDs
        // user-approved 2026-07-16; web-content-hosting.md § Published-post
        // management) — gated to the local actor's own post, the same shape
        // linux (client.actor_id() == post.author) and web (isOwn derived from
        // $identity) already ship. FeedPostItem carries only the author's hex
        // actor id (no is_own flag from the FFI snapshot), so the comparison
        // happens here against the page's own ICryptoService.
        var ownActorHex = _crypto?.HasKey == true ? _crypto.ActorIdHex : null;
        var isOwn = ownActorHex is not null && post.AuthorHex == ownActorHex;

        // Resolve the site-link origin ONLY when this own post could show a copy
        // verb (already published — WebSlug set). An unpublished post's sole verb
        // (Publish to web) needs no origin, so this round trip is skipped for the
        // common case of opening the menu on an ordinary own post.
        uniffi.fauna_client_web.SiteLinkView? siteLink = null;
        if (isOwn && post.WebSlug is { Length: > 0 } && _rpc is not null && _account is not null)
        {
            try
            {
                var web = await GetWebClientAsync();
                var handle = _account.Handle;
                var resolved = await WebOrigin.ResolveAsync(web, handle);
                siteLink = resolved.SiteLink;
            }
            catch (Exception)
            {
                siteLink = null; // treat a failed resolve as "no origin" — dead, not silently live
            }
        }

        var menu = new MenuFlyout();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(menu, Ids.FeedPostActionsMenu);
        // Copy verbs must survive their own Click without the flyout auto-dismissing
        // (a MenuFlyoutItem click always closes its parent MenuFlyout — see
        // ShowDeletePostConfirm's doc comment below) so the e2e can read the
        // `copied` attr right after clicking, the same "menu stays open after a
        // copy" shape web's PostCard.svelte uses (only publish/unpublish close it).
        var suppressClose = false;
        menu.Closing += (_, args) =>
        {
            if (!suppressClose) return;
            args.Cancel = true;
            suppressClose = false;
        };

        void AddVerb(TrainVerb verb, string labelKey, string id)
        {
            var marked = targetFactor is { } f && manager.ExampleLabelFor(post.PostId, f) == verb;
            var item = new MenuFlyoutItem { Text = S.Get(labelKey) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, id);
            // The checked marker rides HelpText (get_attr(item, "state") maps any
            // non-disabled/non-name attr to HelpText — reference_windows_flaui_state_attr_helptext).
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(item, marked ? "on" : "off");
            item.Click += async (_, _) =>
            {
                if (targetFactor is { } factor)
                    await DispatchTrainAsync(post.PostId, factor, verb, marked);
                else
                    OpenTrainTargetSheet(btn, post.PostId, verb);
            };
            menu.Items.Add(item);
        }

        AddVerb(TrainVerb.MoreLikeThis, "feed/more_like_this", "feed-post-more-like-this");
        AddVerb(TrainVerb.LessLikeThis, "feed/less_like_this", "feed-post-less-like-this");

        if (isOwn)
        {
            var del = new MenuFlyoutItem { Text = S.Get("feed/delete_post") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(del, Ids.FeedPostDeleteButton);
            del.Click += (_, _) => ShowDeletePostConfirm(btn, post.PostId);
            menu.Items.Add(del);

            BuildWebPublishVerbs(menu, post, siteLink, () => suppressClose = true);
        }

        menu.ShowAt(btn);
    }

    /// <summary>The own-post web-publishing verbs (web-content-hosting.md
    /// § Published-post management; presence rules ui/feed.md § User actions).
    ///
    /// Everything here is state-derived off the post the snapshot already holds —
    /// <see cref="FeedPostItem.WebSlug"/> / <see cref="FeedPostItem.GatedTier"/> —
    /// never a per-row query. <paramref name="siteLink"/> is the caller's already-
    /// resolved origin (or null — no origin, or not an own published post, in
    /// which case the copy verbs are simply not offered a live link).
    ///
    /// <paramref name="markCopyPending"/> is called by a copy verb's Click just
    /// before it writes to the clipboard, so the caller's <c>Closing</c> handler
    /// keeps the menu open long enough for the `copied` attr to be read.</summary>
    private void BuildWebPublishVerbs(
        MenuFlyout menu, FeedPostItem post, uniffi.fauna_client_web.SiteLinkView? siteLink,
        Action markCopyPending)
    {
        if (post.WebSlug is not { Length: > 0 } slug)
        {
            // Unpublished: one verb, no link affordances for a page that does not
            // exist. A default slug is the nest's to mint, so this needs no origin.
            var publish = new MenuFlyoutItem { Text = S.Get("web_publish/publish_to_web") };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(publish, Ids.FeedPostPublishWebButton);
            publish.Click += async (_, _) => await DispatchWebPublishAsync(post.PostId);
            menu.Items.Add(publish);
            return;
        }

        var origin = siteLink?.@origin;
        // Said once for the menu rather than per verb, and only when the paywall
        // affordance is actually present below (monetization.md § Pillar 2 →
        // Creator comp-link surface: the ratified ~10-minute validity).
        if (post.GatedTier is { Length: > 0 } && origin is not null)
            menu.Items.Add(new MenuFlyoutItem { Text = S.Get("web_publish/paywall_link_note"), IsEnabled = false });
        // Why the copy verbs below are dead, in the user's own terms — the ⋯ menu
        // cannot say "the toggle above" (that control is on another page).
        if (origin is null)
            menu.Items.Add(new MenuFlyoutItem { Text = S.Get("web_publish/menu_no_link_reason"), IsEnabled = false });

        var copyWeb = new MenuFlyoutItem
        {
            Text = S.Get("web_publish/copy_web_link"),
            IsEnabled = origin is not null,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyWeb, Ids.FeedPostCopyWebLinkButton);
        copyWeb.Click += (_, _) =>
        {
            if (origin is not { } o) return;
            markCopyPending();
            var url = FaunaFfiMethods.WebPostPageUrl(o, slug);
            FaunaApp.Helpers.ClipboardHelper.CopyText(url);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(copyWeb, url);
        };
        menu.Items.Add(copyWeb);

        // Gated rows only: an ungated post has no paywalled body, so the mint
        // would hand out a token for nothing.
        if (post.GatedTier is { Length: > 0 })
        {
            var copyPaywall = new MenuFlyoutItem
            {
                Text = S.Get("web_publish/copy_paywall_link"),
                IsEnabled = origin is not null,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copyPaywall, Ids.FeedPostCopyPaywallLinkButton);
            copyPaywall.Click += async (_, _) =>
            {
                if (origin is not { } o) return;
                markCopyPending();
                await DispatchCopyPaywallLinkAsync(copyPaywall, post.PostId, slug, o);
            };
            menu.Items.Add(copyPaywall);
        }

        var unpublish = new MenuFlyoutItem { Text = S.Get("web_publish/unpublish") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unpublish, Ids.FeedPostUnpublishWebButton);
        unpublish.Click += async (_, _) => await DispatchWebUnpublishAsync(post.PostId);
        menu.Items.Add(unpublish);
    }

    /// <summary>Lazily build the shared <c>FfiWebClient</c> over the session's
    /// shared, auto-reconnecting WS-RPC connection (mirrors
    /// <c>SettingsWebPage.EnsureViewModelAsync</c> — never a per-open one-shot
    /// connection).</summary>
    private async Task<uniffi.fauna_ffi.FfiWebClient> GetWebClientAsync()
    {
        if (_webClient is not null) return _webClient;
        if (_rpc is null) throw new InvalidOperationException("no RPC client");
        _webClient = await _rpc.BuildWebClientAsync();
        return _webClient;
    }

    /// <summary>Publish a post as a web page from the ⋯ menu (the nest mints the
    /// default slug — this menu offers no client-chosen slug). A successful call
    /// re-reads the current feed so the card's WebSlug (which drives the whole
    /// verb family) repaints from the nest's own answer, same as the section's own
    /// non-optimistic pattern.</summary>
    private async Task DispatchWebPublishAsync(string postIdHex)
    {
        if (_viewModel is null) return;
        try
        {
            var web = await GetWebClientAsync();
            await web.PublishSet(Convert.FromHexString(postIdHex), null);
            await _viewModel.Manager.RefreshCurrentFeed();
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_publish", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>Take a published page down from the ⋯ menu. One tap, no confirm
    /// step — unlike delete, a takedown is idempotent and reversible.</summary>
    private async Task DispatchWebUnpublishAsync(string postIdHex)
    {
        if (_viewModel is null) return;
        try
        {
            var web = await GetWebClientAsync();
            await web.PublishUnset(Convert.FromHexString(postIdHex));
            await _viewModel.Manager.RefreshCurrentFeed();
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_unpublish", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>Mint + copy a short-lived full-access paywall link. A fresh mint
    /// per click: the token is short-lived by ratified design and re-minting is
    /// free, so re-copying always yields a link that works from now rather than a
    /// cached one that already expired.</summary>
    private async Task DispatchCopyPaywallLinkAsync(MenuFlyoutItem button, string postIdHex, string slug, string origin)
    {
        try
        {
            var web = await GetWebClientAsync();
            var minted = await web.PaywallMintToken(new uniffi.fauna_client_web.PaywallTarget.PostSlug(slug));
            var url = FaunaFfiMethods.WebTokenedUrl(origin, minted.@path, minted.@token);
            FaunaApp.Helpers.ClipboardHelper.CopyText(url);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(button, url);
        }
        catch (Exception ex)
        {
            var msg = S.Format("web_publish/error_paywall_link", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>Open the delete-confirm step (<c>feed-post-delete-confirm-button</c>) as a
    /// sub-flyout anchored to the ⋯ button — mirrors
    /// <c>DmMessageBubble.ShowDeleteConfirm</c>'s established reveal-in-flyout shape
    /// verbatim (title + a single always-enabled confirm Button in its own
    /// <c>Flyout</c>): a <c>MenuFlyoutItem</c> click always closes its parent
    /// <c>MenuFlyout</c>, so the confirm step needs its own flyout rather than a
    /// second item toggled visible inside <c>menu</c>.</summary>
    private void ShowDeletePostConfirm(FrameworkElement anchor, string postId)
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 8, MinWidth = 180 };
        panel.Children.Add(new TextBlock
        {
            Text = S.Get("feed/delete_post_confirm_title"),
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        var confirm = new Button
        {
            Content = S.Get("feed/delete_post_confirm"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(confirm, Ids.FeedPostDeleteConfirmButton);
        confirm.Click += async (_, _) =>
        {
            _postDeleteConfirmFlyout?.Hide();
            await DispatchDeletePostAsync(postId);
        };
        panel.Children.Add(confirm);

        _postDeleteConfirmFlyout = new Flyout { Content = panel };
        _postDeleteConfirmFlyout.ShowAt(anchor);
    }

    private Flyout? _postDeleteConfirmFlyout;

    /// <summary>Delete an own post via the shared <c>FeedManager::delete_post</c>
    /// (mirrors <see cref="DispatchTrainAsync"/>'s error-surfacing convention). A
    /// successful call mutates the manager's own snapshot + notifies, so
    /// <see cref="Refresh"/> drops the row via the normal observer path — no direct
    /// re-render call here (same as the training verbs above).</summary>
    private async Task DispatchDeletePostAsync(string postId)
    {
        if (_viewModel is null) return;
        try
        {
            await _viewModel.Manager.DeletePost(postId);
        }
        catch (Exception ex)
        {
            var msg = S.Format("feed/error_delete", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>Run a training gesture: the marked verb again ⇒ un-train (the
    /// inverse delta), anything else ⇒ train (the manager applies forward/flip
    /// semantics — no UI-side duplicate guard, <c>TrainResult.DuplicateSignal</c>
    /// writes nothing). A successful call mutates the manager's own snapshot +
    /// notifies, so <see cref="Refresh"/> rebuilds the row via the normal observer
    /// path — no direct re-render call here.</summary>
    private async Task DispatchTrainAsync(string postId, string factor, TrainVerb verb, bool alreadyMarked)
    {
        if (_viewModel is null) return;
        try
        {
            if (alreadyMarked) await _viewModel.Manager.UntrainPost(postId, factor);
            else await _viewModel.Manager.TrainPost(postId, factor, verb);
        }
        catch (Exception ex)
        {
            var msg = S.Format("feed/error_train", ex.Message);
            ErrorBar.Message = msg; ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }

    /// <summary>The factor-target sheet (<c>feed-post-train-target-sheet</c>):
    /// shown when the current feed's composition does not single out one trained
    /// topic. Lists the user's trained factors from the sealed registry — the
    /// picker's stable key rides <c>AutomationProperties.Name</c> (the
    /// task-delegation Content/Name split), its display is the user's chosen
    /// name — and trains the picked one. Also offers a "New trained topic…" link
    /// out to the Personalization home (ui.yaml's ratified "existing factors +
    /// New trained topic…" shape) rather than duplicating the create-form inline
    /// — web's richer pattern; linux's own sheet omits it, priority #4.</summary>
    private void OpenTrainTargetSheet(FrameworkElement anchor, string postId, TrainVerb verb)
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 8, MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(panel, Ids.FeedPostTrainTargetSheet);
        panel.Children.Add(new TextBlock
        {
            Text = S.Get("feed/train_target_title"),
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });

        var combo = new ComboBox { MinWidth = 180, HorizontalAlignment = HorizontalAlignment.Stretch };
        panel.Children.Add(combo);

        var trainButton = new Button
        {
            Content = S.Get("common/save"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        panel.Children.Add(trainButton);

        var flyout = new Flyout { Content = panel };
        trainButton.Click += async (_, _) =>
        {
            if (combo.SelectedItem is ComboBoxItem { Tag: string factor })
            {
                flyout.Hide();
                await DispatchTrainAsync(postId, factor, verb, alreadyMarked: false);
            }
        };

        var createLink = new HyperlinkButton
        {
            Content = S.Get("personalization/trained_factor_create"),
            HorizontalAlignment = HorizontalAlignment.Left,
            Padding = new Thickness(0),
        };
        createLink.Click += (_, _) =>
        {
            flyout.Hide();
            MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.Personalization);
        };
        panel.Children.Add(createLink);

        flyout.ShowAt(anchor);
        // Populate async after opening — the sheet appears immediately, options
        // land when the sealed-registry read resolves (mirrors linux).
        _ = PopulateTrainTargetComboAsync(combo);
    }

    private async Task PopulateTrainTargetComboAsync(ComboBox combo)
    {
        if (_rpc is null) return;
        try
        {
            var rows = await _rpc.TrainedTopicsListAsync();
            foreach (var row in rows)
            {
                if (row.factorKey is not { } key) continue;
                var item = new ComboBoxItem { Content = row.name, Tag = key };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, key);
                combo.Items.Add(item);
            }
            if (combo.Items.Count > 0) combo.SelectedIndex = 0;
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"trained-topics fetch for target sheet failed: {ex.Message}");
        }
    }

    // Opens the post_detail dialog for the clicked card (ui.yaml feed transition
    // `click post-card → post_detail`). The post-card DataTemplate root is a
    // transparent Button (the event-card/addressbook-item/media-item idiom): a bare
    // Grid/StackPanel root exposes no UIA invocation pattern, so the FlaUI bridge's
    // physical-click fallback lands but never fires the open (reference_winui_clickable
    // _row_needs_button_root). Unlike event-card, the card's own interactive children
    // (feed-post-actions-button, load-remote-content-button, the like/reply/repost/quote
    // bar, feed-post-muted-reveal-button) stay NESTED inside this Button rather than
    // moved out as siblings — the feed e2e scopes several of them under `post-card[i]`
    // (feed.py reveal_remote_content / open_post_actions; test_feed_link_preview), and
    // WalkScope confines a scoped lookup to the post-card element's own subtree
    // (flaui-bridge/ElementFinder.cs). Each nested child keeps its own InvokePattern, so
    // FlaUI drives it directly; the outer Button's Invoke opens the detail without
    // double-firing (Invoke is direct, not pointer-routed). The ListView carries no
    // IsItemClickEnabled, so this Click is the sole open path (no ItemClick double-open).
    private async void PostCard_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        // A REPOST ROW's own detail would be blank (the repost post is empty by
        // construction) — activation opens the ORIGINAL's detail instead (feed.md
        // § Interaction bar → Repost, ratified 2026-08-10), mirroring linux/web/apple.
        // OpenPostDetailByIdAsync resolves through the same deep-link door a search
        // hit uses, so an original outside the loaded window still opens with real
        // content instead of degrading to a no-op.
        if (item.IsRepostRow)
        {
            await OpenPostDetailByIdAsync(item.RepostedPostId!);
            return;
        }
        await ShowPostDetailDialogAsync(item);
    }

    /// <summary>
    /// The `SearchNav.Post` deep-link door (search.md § User actions) — opens
    /// a post's detail dialog by id alone, whether or not the timeline ever
    /// loaded it (a search hit can name a post the feed never scrolled to).
    /// Fast path: the id is already RENDERED — the union of
    /// <c>FeedSnapshot.posts</c> and its <c>deepLinkedPost</c> slot (client-side,
    /// since <c>FeedSnapshot::find_post</c> is Rust-only, never UniFFI-exported —
    /// unlike linux, which links the crate directly) — opens immediately, no
    /// round trip, sharing <see cref="ShowPostDetailDialogAsync"/> with the
    /// ordinary post-card-click path. Slow path: switches to the SAME
    /// <c>feed-post-detail-dialog</c> immediately with a loading placeholder
    /// rather than gating the transition on <c>FfiFeedManager.ResolvePost</c>'s
    /// round trip — the product contract
    /// (<c>test_activating_a_post_search_result_navigates_to_its_post_detail</c>:
    /// "the dialog is meant to appear immediately... not gate the switch on the
    /// round trip"), mirroring linux's <c>open_post_detail_by_id</c> — then fills
    /// the SAME open dialog's content in place once the resolve lands
    /// (<see cref="ResolveAndFillPostDetailAsync"/>), the same in-place-repaint
    /// idiom <see cref="UnlockGatedDetailAsync"/> already uses on this page.
    /// </summary>
    private async Task OpenPostDetailByIdAsync(string postId)
    {
        if (_viewModel is null) return;

        if (FindRenderedPost(postId) is { } loaded)
        {
            await ShowPostDetailDialogAsync(new FeedPostItem(loaded, _viewModel.Manager));
            return;
        }

        var loadingText = new TextBlock
        {
            Text = S.Get("common/loading"),
            HorizontalAlignment = HorizontalAlignment.Center,
        };
        var dialog = new ContentDialog
        {
            Title = S.Get("common/post"),
            Content = loadingText,
            CloseButtonText = S.Get("common/close"),
            XamlRoot = this.XamlRoot,
        };
        AutomationProperties.SetAutomationId(dialog, Ids.FeedPostDetailDialog);

        await Controls.Dialogs.ShowAsync(dialog, prepare: () => _ = ResolveAndFillPostDetailAsync(dialog, postId));
    }

    /// <summary>
    /// Resolves <paramref name="postId"/> via <c>FfiFeedManager.ResolvePost</c>
    /// and fills the STILL-OPEN dialog <paramref name="dialog"/>'s content in
    /// place — an imperative dialog can't reactively repaint, so this explicit
    /// re-read + swap is required, exactly like
    /// <see cref="UnlockGatedDetailAsync"/>'s body re-paint. `Loaded`/`Fetched`
    /// AND `TakenDown` all resolve through the SAME <see cref="FindRenderedPost"/>
    /// union check below — `resolve_post` parks a tombstone `PostSummary`
    /// (`legal_takedown_ref` set, everything else default) in the deep-link
    /// slot synchronously before returning `TakenDown`
    /// (`fauna_feed::FeedManager::resolve_post`), so `FindRenderedPost` always
    /// finds SOMETHING once the await above completes; there is no separate
    /// branch to write, and <see cref="BuildPostDetailPanel"/> is what paints
    /// the tombstone. `Unavailable` (or, defensively, no snapshot hit at all)
    /// leaves the loading placeholder up rather than reverting — degrade,
    /// never crash, the same posture every pre-existing stale-id open takes
    /// for a deleted/quarantined post.
    /// </summary>
    private async Task ResolveAndFillPostDetailAsync(ContentDialog dialog, string postId)
    {
        if (_viewModel is null) return;
        try
        {
            await _viewModel.Manager.ResolvePost(postId);
            if (FindRenderedPost(postId) is { } resolved)
            {
                var resolvedItem = new FeedPostItem(resolved, _viewModel.Manager);
                var panel = BuildPostDetailPanel(resolvedItem, out var bodyText, out var postImageInsertIndex);
                dialog.Content = new ScrollViewer { Content = panel, MaxHeight = 400 };
                if (resolvedItem.GatedTier is { Length: > 0 } && !resolvedItem.GatedUnlocked
                    && !resolvedItem.ShowRegionPlaceholder)
                    _ = UnlockGatedDetailAsync(postId, bodyText, panel, postImageInsertIndex);
            }
            // Unavailable / no snapshot hit: leave the loading placeholder up — the
            // same "degrade, never crash" posture every stale-id open takes.
        }
        catch (System.Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"post search-nav resolve failed: {ex.Message}");
        }
    }

    /// <summary>The id-space lookup every <c>post_detail</c> deep-link door needs:
    /// wherever the snapshot holds the post, the timeline first, then the
    /// deep-link slot — the client-side twin of
    /// <c>fauna_feed::FeedSnapshot::find_post</c> (Rust-only, never UniFFI-exported,
    /// content-index.md's id-space-trap class: never re-scan only the loaded
    /// list). Reads the manager's OWN <c>Snapshot()</c>, never the cached
    /// <c>FeedViewModel.Snapshot</c> (see <see cref="UnlockGatedDetailAsync"/>'s
    /// note on why) — a fresh read is required right after
    /// <c>ResolvePost</c> populates <c>deepLinkedPost</c>.</summary>
    private PostSummary? FindRenderedPost(string postId)
    {
        if (_viewModel is null) return null;
        var snap = _viewModel.Manager.Snapshot();
        return snap.posts.FirstOrDefault(p => p.postId == postId)
            ?? (snap.deepLinkedPost is { } dl && dl.postId == postId ? dl : null);
    }

    // Builds the post_detail dialog's content (ui.yaml feed transition `click
    // post-card → post_detail`) for either open path — the ordinary
    // post-card-click (<see cref="ShowPostDetailDialogAsync"/>) or the
    // search-nav deep link (<see cref="ResolveAndFillPostDetailAsync"/>).
    private StackPanel BuildPostDetailPanel(FeedPostItem item, out TextBlock bodyText, out int postImageInsertIndex)
    {
        var panel = new StackPanel { Spacing = 8 };

        // Convention 17's verdict side for this surface: the item's composed verdict,
        // registered off the dialog's own root whatever arm paints below.
        var witnessKey = "detail:" + item.PostId;
        panel.Loaded += (_, _) => RegionPlaneHost.WitnessBlocked(witnessKey, item.IsRegionBlocked);
        panel.Unloaded += (_, _) => RegionPlaneHost.Forget(witnessKey);

        // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1):
        // the nest withheld this post's body under a legal obligation — surface the
        // shared tombstone IN PLACE OF the post, never a blank detail dialog, exactly
        // like the quoted-post embed's own arm below and DmMessageBubble's. Only ever
        // true for a post reached through the resolve_post deep-link door (an ordinary
        // loaded list row never carries legal_takedown_ref — search.md § `Post`
        // navigation), but checked unconditionally here since BuildPostDetailPanel is
        // the ONE door both open paths share.
        if (item.IsLegalTakedown)
        {
            bodyText = new TextBlock
            {
                Text = item.LegalTakedownDisplayBody,
                TextWrapping = TextWrapping.Wrap,
                FontStyle = Windows.UI.Text.FontStyle.Italic,
                Opacity = 0.6,
            };
            AutomationProperties.SetAutomationId(bodyText, Ids.FeedPostDetailBody);
            panel.Children.Add(bodyText);
            postImageInsertIndex = panel.Children.Count;
            return panel;
        }

        // Region placeholder (region-blocking.md § The blocked render): the region
        // content policy withholds this post — its frame, authority and reason VERBATIM
        // in place of the whole post (body, media, tags), ahead of every other arm
        // exactly as on the list card. Before this the detail composed no content-policy
        // source at all (tui's C3 build found the same gap on tui). A `collapse`
        // reveals through the shared reveal set and repaints the dialog in place.
        if (item.Region is { } region && item.ShowRegionPlaceholder)
        {
            var placeholder = RegionPlaceholderPanel.Build(region, onReveal: () =>
            {
                item.RevealContent();
                var fresh = BuildPostDetailPanel(item, out _, out _);
                var children = fresh.Children.ToList();
                fresh.Children.Clear();
                panel.Children.Clear();
                foreach (var child in children) panel.Children.Add(child);
            });
            placeholder.Loaded += (_, _) => RegionPlaneHost.WitnessPainted(witnessKey, region.IsBlock);
            panel.Children.Add(placeholder);
            // No feed-post-detail-body: the body is withheld. The out param still needs
            // an element; the callers skip the gated-unlock repaint for a withheld post.
            bodyText = new TextBlock();
            postImageInsertIndex = panel.Children.Count;
            return panel;
        }

        // Body painted from the shared RenderDocument (render-model.md § D6) via the SAME
        // DocumentPainter the list card + the conversations bubble use — not flat text. A
        // blocked remote image reveals via a per-dialog load-remote-content-button
        // (render-time only; html-mail Slice 3), mirroring the list card.
        bodyText = new TextBlock { TextWrapping = TextWrapping.Wrap };
        // ui.yaml feed `post_detail` view owns DEDICATED ids (feed-post-detail-body /
        // -author), distinct from the list card's feed-post-text / post-author — the
        // detail dialog must not reuse the card ids (they collide in the UIA tree when
        // the modal is open over the list). Matches the e2e post_detail_body() reader.
        AutomationProperties.SetAutomationId(bodyText, Ids.FeedPostDetailBody);
        DocumentPainter.Apply(bodyText, item.Document);
        panel.Children.Add(bodyText);

        if (DocumentRenderer.HasBlockedRemoteImage(item.Document))
        {
            var revealButton = new Button
            {
                Content = S.Get("conversations/detail/load_remote_content"),
                HorizontalAlignment = HorizontalAlignment.Left,
                Padding = new Thickness(8, 2, 8, 2),
                FontSize = 12,
            };
            AutomationProperties.SetAutomationId(revealButton, Ids.LoadRemoteContentButton);
            // D3: dispatch to the manager — it flips the reveal set + re-emits; the next
            // Refresh rebuilds the row with RemoteImage.revealed=true. The dialog is an
            // imperative snapshot; it can't reactively repaint, so collapse the button
            // immediately as user feedback (re-opening the detail shows the revealed image).
            revealButton.Click += (_, _) =>
            {
                _viewModel?.RevealRemoteImages(item.PostId);
                revealButton.Visibility = Visibility.Collapsed;
            };
            panel.Children.Add(revealButton);
        }

        // Body remote images (render-model.md § D3 + § Implementation status): the SAME
        // doc-remote-image panel the list card paints (RemoteImagesBind), built here
        // imperatively since the detail dialog is imperative throughout. An empty
        // RemoteImages list paints nothing, matching the ItemsControls below.
        var remoteImagesPanel = new StackPanel();
        DocumentPainter.ApplyRemoteImages(remoteImagesPanel, item.RemoteImages);
        panel.Children.Add(remoteImagesPanel);

        // Post image (post-image) — painted from the folded `Image` block in the document
        // (render-model.md § D6), the same media the list card shows, via the shared blob
        // loader attached property (async, non-blocking; the bytes load stays client glue —
        // render-model.md § The boundary). Closes the gap where the detail omitted the image.
        // A gated post's pre-unlock teaser carries no Image block yet, so this is null here;
        // postImageInsertIndex marks the slot so UnlockGatedDetailAsync can insert the SAME
        // element once the unlock lands.
        postImageInsertIndex = panel.Children.Count;
        if (BuildPostDetailImageElement(item.Document) is { } postImage)
            panel.Children.Add(postImage);

        // Video thumbnail (video-thumbnail) — the D6b `Video` sibling of `Image`
        // (render-model.md § Implementation status today), the detail-dialog twin of the
        // list card's XAML render above: play glyph + hash text, no poster frame (none
        // exists to paint — see the list card's own note).
        if (DocumentRenderer.MediaVideoHash(item.Document) is { } videoHash)
        {
            var videoRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4, Margin = new Thickness(0, 4, 0, 0) };
            AutomationProperties.SetAutomationId(videoRow, Ids.VideoThumbnail);
            AutomationProperties.SetName(videoRow, videoHash);
            videoRow.Children.Add(new TextBlock { Text = "\uE768", FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"), FontSize = 14, VerticalAlignment = VerticalAlignment.Center });
            videoRow.Children.Add(new TextBlock { Text = videoHash, FontSize = 12, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center });
            panel.Children.Add(videoRow);
        }

        // The author alone carries the id — the same short form the card paints, as web's
        // and linux's detail do — so "which post did this open" reads as an author, not a
        // sentence; the time sits beside it. (It was one hand-rolled English line,
        // "by ‹author› · ‹time›", which no other app's detail painted.)
        var authorRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        var authorText = new TextBlock { Text = item.ShortAuthor, Opacity = 0.6 };
        // Dedicated detail-dialog author id (see feed-post-detail-body above) — matches
        // the e2e post_detail_author() reader and ui.yaml `post_detail` element scope.
        AutomationProperties.SetAutomationId(authorText, Ids.FeedPostDetailAuthor);
        authorRow.Children.Add(authorText);
        authorRow.Children.Add(new TextBlock { Text = item.TimeAgo, Opacity = 0.6 });
        panel.Children.Add(authorRow);

#if PAYMENTS
        // The tip display surface (monetization.md § Tips), the detail-dialog twin of the
        // list card's PostTipDisplay — same imperative-panel treatment as the quoted-post
        // embed above (C# CAN carry an id conditionally, unlike XAML, so this stays inline
        // rather than moving to Views/Payments — dynamic-features.md § Platform-family
        // surface excision, "C#: a define"). Guarded exactly like the list card:
        // post-tip-total iff HasTipTotal, post-tip-count iff HasTips (never the same
        // condition — a post can have real tips with no summable amount).
        // post-tip-list-button opens the SAME flat post-tip-list dialog the list card
        // opens — there is only ever one open at a time.
        if (item.HasTips)
        {
            var tipRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
            if (item.HasTipTotal)
            {
                var tipTotal = new TextBlock { Text = item.TipTotalText, FontSize = 12, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center };
                AutomationProperties.SetAutomationId(tipTotal, Ids.PostTipTotal);
                tipRow.Children.Add(tipTotal);
            }
            var tipCount = new TextBlock { Text = item.TipCountText, FontSize = 12, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center };
            AutomationProperties.SetAutomationId(tipCount, Ids.PostTipCount);
            tipRow.Children.Add(tipCount);
            var tipListButton = new Button
            {
                Content = S.Get("tips/list_open"),
                Tag = item,
                FontSize = 12,
                Padding = new Thickness(4, 1, 4, 1),
                VerticalAlignment = VerticalAlignment.Center,
            };
            AutomationProperties.SetAutomationId(tipListButton, Ids.PostTipListButton);
            tipListButton.Click += (_, _) => OpenTipList(item);
            tipRow.Children.Add(tipListButton);
            panel.Children.Add(tipRow);
        }
#endif

        if (item.SourceBadges.Count > 0)
            panel.Children.Add(new TextBlock
            {
                Text = $"Source: {string.Join(", ", item.SourceBadges.Select(b => b.Label))}",
                Opacity = 0.5,
                FontSize = 12,
            });

        // Tag chips (post_detail mirrors the list card's tags).
        if (item.Tags.Count > 0)
        {
            var tagRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
            foreach (var tag in item.Tags)
            {
                var chip = new TextBlock { Text = $"#{tag}", FontSize = 11, Opacity = 0.5 };
                AutomationProperties.SetAutomationId(chip, Ids.TagChip);
                tagRow.Children.Add(chip);
            }
            panel.Children.Add(tagRow);
        }

        // Embedded quoted-post card — painted from the document-derived quote fields
        // (render-model.md § D6). The list's fire-once trigger folds the `QuotedPost` block
        // and re-emits, so the clicked (current) item already carries it. No re-trigger here:
        // an imperative dialog can't reactively repaint, and a re-emit rebuilds the list's
        // items rather than this captured one — re-opening shows a late-resolved quote.
        var quotedBorder = new Border
        {
            BorderThickness = new Thickness(3, 0, 0, 0),
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["DividerStrokeColorDefaultBrush"],
            Padding = new Thickness(8, 4, 8, 4),
            CornerRadius = new CornerRadius(4),
            Visibility = item.HasQuotedPost ? Visibility.Visible : Visibility.Collapsed,
        };
        if (item.HasQuotedPost)
        {
            var quoteStack = new StackPanel { Spacing = 2 };
            // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1): the
            // nest withheld the quoted post's envelope, so omit the author + unverified-source
            // row (author is empty) and paint only the shared tombstone (QuotedPostDisplayBody
            // below) — the imperative twin of the list card's `QuotedPostShowHeaderRow` gate,
            // mirroring web QuotedPost.svelte's `legal_takedown_ref` arm.
            if (item.QuotedPostShowHeaderRow)
            {
                // Slice 2b: the quoted author row carries the unverified-source badge iff the
                // quoted post's OWN verification FAILED — the imperative twin of the list card's
                // XAML badge (security.md § App display of unverified content).
                var quoteAuthorRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
                quoteAuthorRow.Children.Add(new TextBlock { Text = item.QuotedPostAuthor, FontSize = 11, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center });
                if (item.QuotedPostIsUnverified)
                    quoteAuthorRow.Children.Add(BuildQuotedUnverifiedBadge(item));
                // The quoted embed's own D10 audit marker, trailing the unverified one
                // exactly as on the list card.
                if (item.QuotedPostIsDelegatedOrigin)
                    quoteAuthorRow.Children.Add(BuildQuotedDelegatedOriginBadge(item));
                quoteStack.Children.Add(quoteAuthorRow);
            }
            quoteStack.Children.Add(new TextBlock { Text = item.QuotedPostDisplayBody, FontSize = 12, TextWrapping = TextWrapping.Wrap, MaxLines = 3 });
            quotedBorder.Child = quoteStack;
        }
        AutomationProperties.SetAutomationId(quotedBorder, Ids.QuotedPost);
        // A bare Border carrying only an AutomationId is pruned from the UIA tree
        // (memory reference_winui_flaui_datatemplate_name); name it (the quoted author,
        // or the tombstone text for a taken-down quote — both non-empty when HasQuotedPost)
        // so a future detail-dialog e2e can find it, mirroring the list-card XAML Border's Name.
        AutomationProperties.SetName(quotedBorder, item.QuotedPostAccessibleName);
        panel.Children.Add(quotedBorder);

        return panel;
    }

    /// <summary>Builds the detail dialog's <c>post-image</c> element from <paramref
    /// name="document"/>'s folded <c>Image</c> block — <c>null</c> when the document carries
    /// no media (most posts). The one construction site for BOTH the initial
    /// <see cref="BuildPostDetailPanel"/> build and <see cref="UnlockGatedDetailAsync"/>'s
    /// post-unlock insert, so a gated post's pre-unlock teaser (no Image block) and its
    /// unsealed document (one now folded in) paint through the identical element.</summary>
    private static Image? BuildPostDetailImageElement(uniffi.fauna_core.RenderDocument document)
    {
        if (DocumentRenderer.MediaImageHash(document) is not { } mediaHash || GetImageLoader() is not { } loader)
            return null;
        var postImage = new Image
        {
            MaxHeight = 300,
            Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
            HorizontalAlignment = HorizontalAlignment.Left,
            Margin = new Thickness(0, 4, 0, 4),
        };
        AutomationProperties.SetAutomationId(postImage, Ids.PostImage);
        ImageHashBind.SetLoader(postImage, loader);
        ImageHashBind.SetHash(postImage, mediaHash);
        return postImage;
    }

    // Shows the post_detail dialog for the ordinary post-card-click path — the
    // post is already loaded by construction, so this is a pure paint, no
    // resolve. The search-nav deep-link door (<see cref="OpenPostDetailByIdAsync"/>)
    // shares BuildPostDetailPanel but not this method: it owns its own dialog
    // (built immediately, before any resolve) so it can fill that SAME dialog's
    // content in place once the resolve lands, rather than opening a second one.
    private async Task ShowPostDetailDialogAsync(FeedPostItem item)
    {
        var panel = BuildPostDetailPanel(item, out var bodyText, out var postImageInsertIndex);

        var dialog = new ContentDialog
        {
            Title = S.Get("common/post"),
            Content = new ScrollViewer { Content = panel, MaxHeight = 400 },
            CloseButtonText = S.Get("common/close"),
            XamlRoot = this.XamlRoot,
        };
        AutomationProperties.SetAutomationId(dialog, Ids.FeedPostDetailDialog);

        // Through the gate, whose registry a login/actor swap force-closes: this dialog is
        // not tied to the frame, so it would otherwise outlive a re-login and cover the next
        // actor's feed. Same class as apple's lingering-PostDetailView-across-subscriber-
        // re-login fix — a detail belonging to the previous actor must never
        // survive the swap.
        await Controls.Dialogs.ShowAsync(dialog, prepare: () =>
        {
            // Gated post: the body painted above is the public TEASER (item.Document). Kick
            // off the unlock — fetch the sealed blob + decrypt (author custody / subscriber
            // KeyBlob) — and repaint feed-post-detail-body with the full body. bodyText is a
            // live element in the shown dialog, so the async repaint lands on the open dialog.
            // Fire-once: skip if already unlocked (feed.md § Encryption at rest — detail-open
            // unlock).
            if (item.GatedTier is { Length: > 0 } && !item.GatedUnlocked && !item.ShowRegionPlaceholder)
                _ = UnlockGatedDetailAsync(item.PostId, bodyText, panel, postImageInsertIndex);
        });
    }

    /// <summary>Unlock a gated post opened in the detail dialog and repaint its body — and, when
    /// the unsealed document folds in an <c>Image</c> block the pre-unlock teaser did not carry,
    /// insert the SAME <c>post-image</c> element <see cref="BuildPostDetailPanel"/> would have
    /// built at open time (<paramref name="postImageInsertIndex"/> marks that slot). Fetches the
    /// sealed full-body blob (<c>gated_blob_hash</c> → <c>GET /api/v1/blob/{hash}</c>), decrypts it
    /// (<c>unlock_gated_post</c> — author custody or the subscriber's KeyBlob wrap entry), then
    /// re-applies <see cref="DocumentPainter"/> to the dialog's <c>feed-post-detail-body</c> from
    /// the now-unlocked snapshot document. The imperative dialog can't reactively repaint, so this
    /// explicit re-read is required (feed.md § Encryption at rest — detail-open unlock; the
    /// reveal-remote-images nuance above). Before this, a gated post's detail NEVER gained a
    /// post-image element at all — the initial build saw only the teaser, which carries no Image
    /// block — so its paint could never be read headlessly.</summary>
    private async System.Threading.Tasks.Task UnlockGatedDetailAsync(
        string postId, TextBlock bodyText, StackPanel panel, int postImageInsertIndex)
    {
        // Every exit below reports on `error-message`. This chain is fire-and-forget over
        // an imperative dialog with NO reactive repaint fallback (unlike linux's
        // render_posts / web's Svelte refresh), so any link that gives up silently leaves
        // the reader on the teaser forever with nothing to explain it — and, because two
        // of the four break points are *guards* rather than throws, a swallowed failure
        // produced no log at all. That made the failure indistinguishable from "this post
        // has no full body", on the client and in e2e alike (conventions rule 2: every
        // page surfaces error-message; rule 6: a failure must diagnose itself).
        // Resolve the live client ONCE for this operation (never a field captured at
        // navigation time — see the Nest property).
        var nest = Nest;
        if (_viewModel is null || nest is null)
        {
            ReportPageError(S.Format("feed/error_gated_unlock", "no nest client on this page"));
            return;
        }

        var step = "resolve the sealed blob hash";
        try
        {
            var hash = await _viewModel.GatedBlobHashAsync(postId);
            if (hash is null)
            {
                // not gated / not loaded / undecodable — gated_blob_hash is author-agnostic,
                // so this is a load/decode gap, never an identity one.
                ReportPageError(S.Format("feed/error_gated_unlock", "the sealed blob hash did not resolve"));
                return;
            }

            step = "fetch the sealed blob";
            var blob = await nest.GetBlobAsync(hash);

            step = "decrypt under a held period key";
            await _viewModel.UnlockGatedPostAsync(postId, blob);

            // Re-read the now-unlocked post from the MANAGER, not from `_viewModel.Posts`.
            // `FeedViewModel.Snapshot` memoizes (`_cachedSnapshot ??= _manager.Snapshot()`)
            // and is invalidated only by the observer's PropertyChanged, which
            // FeedNotifyObserver hops onto the dispatcher — so the cache is still holding the
            // PRE-unlock PostSummary when this continuation resumes, and the guard below
            // reads `gatedUnlocked: false` for a post the manager has already unsealed. The
            // manager's own Snapshot() is the authoritative post-mutation read.
            var updated = _viewModel.Manager.Snapshot().posts
                .FirstOrDefault(p => p.postId == postId);
            if (updated is not { gatedUnlocked: true })
            {
                // unlock_gated_post returns Ok(()) WITHOUT notifying when the post is absent
                // from the manager's post list (fauna-feed/src/manager.rs — `changed == false`),
                // so a silent success that painted nothing lands here rather than in the catch.
                ReportPageError(S.Format("feed/error_gated_unlock", "the unsealed body did not land in the snapshot"));
                return;
            }

            DocumentPainter.Apply(bodyText, updated.document);

            // The pre-unlock teaser carried no Image block, so BuildPostDetailPanel added no
            // post-image element for it; insert the same element it would have built, at the
            // slot it would have taken, now that the unsealed document folds one in.
            if (BuildPostDetailImageElement(updated.document) is { } postImage)
                panel.Children.Insert(System.Math.Min(postImageInsertIndex, panel.Children.Count), postImage);
        }
        catch (System.Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"gated post unlock failed while trying to {step}: {ex.Message}");
            ReportPageError(S.Format("feed/error_gated_unlock", $"could not {step} ({ex.Message})"));
        }
    }

    /// <summary>The muted unverified-source badge for the quoted-post embed in the
    /// post-detail dialog — the imperative twin of the list card's XAML badge
    /// (security.md § App display of unverified content, Slice 2b). Reuses the
    /// <c>unverified-source-badge</c> AutomationId scoped under <c>quoted-post</c>.</summary>
    private static Border BuildQuotedUnverifiedBadge(FeedPostItem item)
    {
        var badge = new Border
        {
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(4, 1, 4, 1),
            VerticalAlignment = VerticalAlignment.Center,
            Background = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemControlBackgroundBaseLowBrush"],
        };
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 3 };
        row.Children.Add(new TextBlock
        {
            Text = "",
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
            FontSize = 10,
            Opacity = 0.7,
            VerticalAlignment = VerticalAlignment.Center,
        });
        row.Children.Add(new TextBlock { Text = item.UnverifiedSourceLabel, FontSize = 10, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center });
        badge.Child = row;
        AutomationProperties.SetAutomationId(badge, Ids.UnverifiedSourceBadge);
        AutomationProperties.SetName(badge, item.UnverifiedSourceLabel);
        ToolTipService.SetToolTip(badge, item.UnverifiedSourceTooltip);
        return badge;
    }

    /// <summary>The D10 audit marker for the quoted-post embed in the post-detail
    /// dialog — the imperative twin of the list card's XAML badge
    /// (atproto-pds-full.md &#167; Problem 1 &#8594; D10 &#8594; <i>Audit</i>). Reuses
    /// the <c>delegated-origin-badge</c> AutomationId scoped under
    /// <c>quoted-post</c>.</summary>
    private static Border BuildQuotedDelegatedOriginBadge(FeedPostItem item)
    {
        var badge = new Border
        {
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(4, 1, 4, 1),
            VerticalAlignment = VerticalAlignment.Center,
            Background = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemControlBackgroundBaseLowBrush"],
        };
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 3 };
        row.Children.Add(new TextBlock
        {
            Text = "\uE71B",
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
            FontSize = 10,
            Opacity = 0.7,
            VerticalAlignment = VerticalAlignment.Center,
        });
        row.Children.Add(new TextBlock { Text = item.DelegatedOriginLabel, FontSize = 10, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center });
        badge.Child = row;
        AutomationProperties.SetAutomationId(badge, Ids.DelegatedOriginBadge);
        AutomationProperties.SetName(badge, item.DelegatedOriginLabel);
        ToolTipService.SetToolTip(badge, item.DelegatedOriginTooltip);
        return badge;
    }

    private async void CreateFeed_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        var nameBox = new TextBox { Header = S.Get("feed/create/feed_name"), PlaceholderText = S.Get("feed/create/name_placeholder") };
        AutomationProperties.SetAutomationId(nameBox, Ids.FeedCreateFeedName);

        // Canonical rule-type catalog + per-type input kind now come from the
        // shared FFI (feed.md § Where logic lives → Feed rule-builder presentation)
        // instead of a hand-rolled array + isToggle/isLabel predicates.
        var ruleTypeCombo = new ComboBox { MinWidth = 200 };
        AutomationProperties.SetAutomationId(ruleTypeCombo, Ids.FeedRuleTypeSelect);
        // value = the canonical FilterRule variant — the logic value AND the
        // driver.select key, mirrored onto AutomationProperties.Name (the
        // reminder-combo pattern: the FlaUI bridge selects by Name, so the e2e
        // keeps passing "HasMedia" while the user sees the localized label).
        var ruleOptions = FaunaFfiMethods.RuleTypeOptions();
        foreach (var option in ruleOptions)
        {
            var item = new ComboBoxItem { Content = S.Resolve(option.label), Tag = option.value };
            AutomationProperties.SetName(item, option.value);
            ruleTypeCombo.Items.Add(item);
        }
        ruleTypeCombo.SelectedIndex = 0;

        var ruleValueBox = new TextBox { PlaceholderText = "Rule value" };
        AutomationProperties.SetAutomationId(ruleValueBox, Ids.FeedRuleValueInput);

        // Threshold input: visible only for the TextAndNumber kind (LabelBelow/
        // LabelAbove). Packs into the value string as "category:threshold" on Add.
        // The threshold rides a 0–10 scale (feed.md § Filter rule types); the
        // shared encode_filter_rule (run inside the manager's create_feed) converts
        // it to the per-mille wire value.
        var thresholdBox = new TextBox
        {
            PlaceholderText = S.Get("feed/create/rule_threshold"),
            Visibility = Visibility.Collapsed,
            Text = FaunaFfiMethods.DefaultRuleThreshold(),
        };
        AutomationProperties.SetAutomationId(thresholdBox, Ids.FeedRuleThresholdInput);

        var requiredToggle = new ToggleSwitch { Header = S.Resolve(FaunaFfiMethods.RuleRequiredLabel(true)), IsOn = true, Visibility = Visibility.Collapsed };
        AutomationProperties.SetAutomationId(requiredToggle, Ids.FeedRuleRequiredToggle);
        // The nest evaluates required:false as a genuine exclusion, not a no-op —
        // re-resolve the label on every toggle so it never states the opposite of
        // the rule being built (feed.md; ui.yaml:5333-5334).
        requiredToggle.Toggled += (s, _) => requiredToggle.Header = S.Resolve(FaunaFfiMethods.RuleRequiredLabel(requiredToggle.IsOn));

        ruleTypeCombo.SelectionChanged += (s, _) =>
        {
            var sel = (ruleTypeCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
            var kind = ruleOptions.FirstOrDefault(o => o.value == sel)?.inputKind ?? FfiRuleInputKind.Text;
            var isToggle = kind == FfiRuleInputKind.Toggle;
            var isLabel = kind == FfiRuleInputKind.TextAndNumber;
            ruleValueBox.Visibility = isToggle ? Visibility.Collapsed : Visibility.Visible;
            thresholdBox.Visibility = isLabel ? Visibility.Visible : Visibility.Collapsed;
            requiredToggle.Visibility = isToggle ? Visibility.Visible : Visibility.Collapsed;
        };

        var rulesList = new StackPanel { Spacing = 4 };
        // Pending rules as shared FilterRuleInput records; the manager encodes each
        // via encode_filter_rule on create_feed (no client-side JSON build).
        var pendingRules = new List<FilterRuleInput>();
        var addRuleButton = new Button { Content = S.Get("common/add") };
        AutomationProperties.SetAutomationId(addRuleButton, Ids.FeedAddRuleButton);

        // Gate on the shared fauna_client_feed::can_add_rule (feed.md § Add-rule
        // gating) — mirrors linux's apply_input_kind/update_sensitivity split
        // (apps/fauna-linux/src/views/feed/feed_list.rs). Recomputed on every
        // keystroke AND every type switch, and once eagerly here so the initial
        // SelectedIndex = 0 state (set above, before any handler existed) is
        // gated correctly too.
        void UpdateAddRuleGate()
        {
            var sel = (ruleTypeCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
            var kind = ruleOptions.FirstOrDefault(o => o.value == sel)?.inputKind ?? FfiRuleInputKind.Text;
            addRuleButton.IsEnabled = FaunaFfiMethods.CanAddRule(kind, ruleValueBox.Text ?? "", thresholdBox.Text ?? "");
        }
        ruleTypeCombo.SelectionChanged += (s, _) => UpdateAddRuleGate();
        ruleValueBox.TextChanged += (s, _) => UpdateAddRuleGate();
        thresholdBox.TextChanged += (s, _) => UpdateAddRuleGate();
        UpdateAddRuleGate();

        addRuleButton.Click += (s, _) =>
        {
            var type = (ruleTypeCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "";
            var kind = ruleOptions.FirstOrDefault(o => o.value == type)?.inputKind ?? FfiRuleInputKind.Text;
            bool isLabel = kind == FfiRuleInputKind.TextAndNumber;
            bool isToggle = kind == FfiRuleInputKind.Toggle;
            string val;
            if (isToggle) val = "";
            else if (isLabel)
            {
                var category = ruleValueBox.Text?.Trim() ?? "";
                var threshold = thresholdBox.Text?.Trim() ?? FaunaFfiMethods.DefaultRuleThreshold();
                val = $"{category}:{threshold}";
            }
            else val = ruleValueBox.Text?.Trim() ?? "";
            pendingRules.Add(new FilterRuleInput(type, val, requiredToggle.IsOn));
            rulesList.Children.Add(new TextBlock { Text = S.Resolve(FaunaFfiMethods.RuleSummaryLabel(type, val, requiredToggle.IsOn)), FontSize = 12 });
            ruleValueBox.Text = "";
            thresholdBox.Text = FaunaFfiMethods.DefaultRuleThreshold();
        };

        var combinationCombo = new ComboBox { MinWidth = 100 };
        AutomationProperties.SetAutomationId(combinationCombo, Ids.FeedCombinationSelect);
        var allItem = new ComboBoxItem { Content = S.Get("feed/create/mode_all"), Tag = "all", IsSelected = true };
        AutomationProperties.SetName(allItem, "all");
        combinationCombo.Items.Add(allItem);
        var anyItem = new ComboBoxItem { Content = S.Get("feed/create/mode_any"), Tag = "any" };
        AutomationProperties.SetName(anyItem, "any");
        combinationCombo.Items.Add(anyItem);
        combinationCombo.SelectedIndex = 0;

        // ── Factor-weight editor (content-moderation-and-ranking.md § Composition) ──
        // A second repeatable-entry row alongside the filter-rule builder above:
        // each Add accumulates one FactorWeightInput, and create_feed (shared Rust)
        // splits them — `global: false` onto this feed's own `composition`,
        // `global: true` read-merge-written into the caller's global factor set.
        var factorCombo = new ComboBox { MinWidth = 160, HorizontalAlignment = HorizontalAlignment.Stretch };
        AutomationProperties.SetAutomationId(factorCombo, Ids.FeedFactorSelect);
        // Tag/Name = the stable factor key (the logic value AND the driver.select
        // key); Content localizes the DISPLAY — the ruleTypeCombo convention above.
        // The built-in head (engagement, trending, …) comes from the shared FFI
        // (feed.md § Where logic lives → Feed factor-picker built-ins), so a new
        // built-in reaches every app without a per-app literal; subscribed
        // labeler:<hex> factors and trained topics append once the fetches below
        // resolve.
        foreach (var option in FaunaFfiMethods.BuiltinFactorOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(option.label), Tag = option.value };
            AutomationProperties.SetName(item, option.value);
            factorCombo.Items.Add(item);
        }
        factorCombo.SelectedIndex = 0;

        var factorWeightBox = new TextBox
        {
            Text = "1.0",
            PlaceholderText = S.Get("feed/create/factor_weight_placeholder"),
            MinWidth = 72,
        };
        AutomationProperties.SetAutomationId(factorWeightBox, Ids.FeedFactorWeightInput);

        var factorGlobalToggle = new CheckBox
        {
            Content = S.Get("feed/create/factor_global_toggle"),
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetAutomationId(factorGlobalToggle, Ids.FeedFactorGlobalToggle);

        var addFactorButton = new Button { Content = S.Get("feed/create/add_factor") };
        AutomationProperties.SetAutomationId(addFactorButton, Ids.FeedAddFactorButton);

        // Grid (star picker column + Auto columns), NOT a horizontal StackPanel: an
        // unconstrained 4-control row can grow past the dialog's visible width and
        // push the trailing Add button off-screen (FlaUI IsOffscreen ⇒ is_visible
        // false even though it renders). The star column
        // absorbs/shrinks instead, keeping the Auto-sized controls always in view.
        var factorRow = new Grid { ColumnSpacing = 8 };
        factorRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        factorRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        factorRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        factorRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(factorCombo, 0);
        Grid.SetColumn(factorWeightBox, 1);
        Grid.SetColumn(factorGlobalToggle, 2);
        Grid.SetColumn(addFactorButton, 3);
        factorRow.Children.Add(factorCombo);
        factorRow.Children.Add(factorWeightBox);
        factorRow.Children.Add(factorGlobalToggle);
        factorRow.Children.Add(addFactorButton);

        var factorsList = new StackPanel { Spacing = 4 };
        var pendingFactors = new List<FactorWeightInput>();
        addFactorButton.Click += (s, _) =>
        {
            var factor = (factorCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
            if (string.IsNullOrEmpty(factor)) return;
            // Decimal multiplier → the wire's signed per-mille weight, via the shared
            // parser: an unparseable entry falls back to the 1.0 baseline, midpoints
            // round half-away-from-zero, and a negative weight (a factor that sinks an
            // item) round-trips. Never hand-roll — C#'s Math.Round is banker's.
            var weightPermille = FaunaFfiMethods.ParseWeightPermille(factorWeightBox.Text ?? "");
            var global = factorGlobalToggle.IsChecked == true;
            pendingFactors.Add(new FactorWeightInput(factor, weightPermille, global));
            var suffix = global ? $" ({S.Get("feed/create/factor_global_toggle")})" : "";
            factorsList.Children.Add(new TextBlock
            {
                Text = $"{factor} × {FaunaFfiMethods.FormatWeightPermille(weightPermille)}{suffix}",
                FontSize = 12,
            });
            factorWeightBox.Text = "1.0";
            factorGlobalToggle.IsChecked = false;
        };

        // Grow the picker with the caller's subscribed labeler factors (raw factor id
        // — no display name exists anywhere yet, matching the labeler-catalog page's
        // own precedent). Fire-and-forget so the dialog opens immediately; the
        // continuation resumes on the UI thread (no ConfigureAwait(false) — an
        // off-thread Items mutation throws a silent COMException).
        _ = AppendSubscribedLabelerFactorsAsync(factorCombo);
        // The third picker source (topic-factors.md § Authoring surface & picker):
        // the user's trained factors from the sealed registry — stable `topic:<hex>`
        // key as the option's Tag/Name, the user's chosen name as its Content (the
        // stable-key-vs-display-label split every picker on this page follows).
        _ = AppendTrainedTopicFactorsAsync(factorCombo);

        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(nameBox);
        panel.Children.Add(new TextBlock { Text = S.Get("feed/create/filter_rules") });
        panel.Children.Add(ruleTypeCombo);
        panel.Children.Add(ruleValueBox);
        panel.Children.Add(thresholdBox);
        panel.Children.Add(requiredToggle);
        panel.Children.Add(addRuleButton);
        panel.Children.Add(rulesList);
        panel.Children.Add(new TextBlock { Text = "Combination mode:" });
        panel.Children.Add(combinationCombo);
        panel.Children.Add(new TextBlock { Text = S.Get("feed/create/factors") });
        panel.Children.Add(factorRow);
        panel.Children.Add(factorsList);

        // Create/cancel buttons live in the panel so their AutomationIds attach to
        // real clickable elements (the dialog template PrimaryButton does not).
        ContentDialog? feedDialog = null;
        bool shouldCreate = false;

        var buttonRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        var cancelBtn = new Button { Content = S.Get("common/cancel") };
        AutomationProperties.SetAutomationId(cancelBtn, Ids.FeedCreateCancel);
        cancelBtn.Click += (s, _) => feedDialog?.Hide();
        var createBtn = new Button
        {
            Content = S.Get("common/create"),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        AutomationProperties.SetAutomationId(createBtn, Ids.CreateFeed);
        createBtn.Click += (s, _) =>
        {
            shouldCreate = true;
            feedDialog?.Hide();
        };
        buttonRow.Children.Add(cancelBtn);
        buttonRow.Children.Add(createBtn);
        panel.Children.Add(buttonRow);

        var dialog = new ContentDialog
        {
            Title = S.Get("feed/create/title"),
            Content = panel,
            XamlRoot = this.XamlRoot,
        };
        feedDialog = dialog;

        await Controls.Dialogs.ShowAsync(dialog);

        if (shouldCreate && !string.IsNullOrWhiteSpace(nameBox.Text))
        {
            var combination = (combinationCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "all";
            await _viewModel.CreateFeedAsync(
                nameBox.Text.Trim(), pendingRules, combination, factors: pendingFactors);
        }
    }

    /// <summary>
    /// Appends the caller's subscribed community-labeler factors to the create-feed
    /// factor picker, so a subscribed <c>labeler:&lt;hex&gt;</c> can weight a feed
    /// alongside the shared built-in factors (<c>engagement</c>, <c>trending</c>)
    /// (content-moderation-and-ranking.md § Tier-3 community models). Reads the same
    /// <c>LabelerCatalogMachine</c> the Community-labelers page builds — web's
    /// <c>ensureFactorOptions</c> does exactly this; no extra FFI surface is needed.
    /// Best-effort: a fetch fault leaves the picker at the built-ins rather than
    /// blocking feed creation.
    /// </summary>
    private async Task AppendSubscribedLabelerFactorsAsync(ComboBox factorCombo)
    {
        if (_rpc is null) return;
        try
        {
            // The observer is required by the builder but unused here — this is a
            // one-shot read, not a live-rendered surface, so the machine is disposed
            // as soon as the snapshot (plain C# records) has been copied out of it.
            var observer = new LabelerCatalogNotifyObserver(() => { });
            using uniffi.fauna_labeler_catalog_machine.LabelerCatalogMachine machine =
                await _rpc.BuildLabelerCatalogMachineAsync(observer);
            await machine.Refresh();
            foreach (var entry in machine.Snapshot().entries)
            {
                if (!entry.subscribed) continue;
                var item = new ComboBoxItem { Content = entry.factor, Tag = entry.factor };
                AutomationProperties.SetName(item, entry.factor);
                factorCombo.Items.Add(item);
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"factor options fetch failed: {ex.Message}");
        }
    }

    /// <summary>Grow the create-feed factor picker with the owner's trained topics
    /// (topic-factors.md § Authoring surface & picker, third source alongside
    /// engagement + subscribed labelers). Fire-and-forget, same shape as
    /// <see cref="AppendSubscribedLabelerFactorsAsync"/>.</summary>
    private async Task AppendTrainedTopicFactorsAsync(ComboBox factorCombo)
    {
        if (_rpc is null) return;
        try
        {
            foreach (var row in await _rpc.TrainedTopicsListAsync())
            {
                if (row.factorKey is not { } key) continue;
                var item = new ComboBoxItem { Content = row.name, Tag = key };
                AutomationProperties.SetName(item, key);
                factorCombo.Items.Add(item);
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"trained-topic factor options fetch failed: {ex.Message}");
        }
    }

    private async void ComposeDialog_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        var textBox = new TextBox
        {
            PlaceholderText = S.Get("feed/post/whats_on_your_mind"),
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            Height = 200,
        };
        AutomationProperties.SetAutomationId(textBox, Ids.FeedComposeDialog);

        var charCount = new TextBlock { Text = "0 characters", Opacity = 0.5, FontSize = 12, HorizontalAlignment = HorizontalAlignment.Right };
        textBox.TextChanged += (s, _) => charCount.Text = $"{textBox.Text.Length} characters";

        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(textBox);
        panel.Children.Add(charCount);

        var dialog = new ContentDialog
        {
            Title = S.Get("composer/new_post"),
            Content = panel,
            PrimaryButtonText = S.Get("common/post"),
            CloseButtonText = S.Get("common/cancel"),
            XamlRoot = this.XamlRoot,
        };

        if (await Controls.Dialogs.ShowAsync(dialog) == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(textBox.Text))
        {
            // Content-only compose — carries whatever attachment the inline compose bar
            // already staged (ui/feed.md § Persistence → Attachments by content address).
            _viewModel.UpdateComposeText(textBox.Text.Trim(), "");
            await _viewModel.SubmitPostAsync();
        }
    }

    /// <summary>Stage <paramref name="audience"/> on the shared manager through the ONE setter
    /// its answer names — the three clear each other, so exactly one answer lands. Shared by
    /// the as-picked forward and the submit's step 1, so the two can never stage one answer
    /// differently.</summary>
    private static void StageAudience(FeedViewModel vm, Controls.ComposeAudience audience)
    {
        if (audience.Sell is { } s)
            vm.UpdateComposeSell(s.Price, s.AskingPrice, s.SubscribersGetItFree, audience.GatePreview);
        else if (audience.GateRoom is not null)
            vm.UpdateComposeRoom(audience.GateRoom, audience.GatePreview);
        else
            vm.UpdateComposeGate(audience.GateTier, audience.GatePreview);
    }

    /// <summary>Compose submit (`post-submit-button`): the blob upload stays client
    /// glue (feed.md § Where logic lives → Image upload), then the shared manager
    /// validates + builds + signs + creates and clears the composer.
    ///
    /// <para>The three steps below are ORDERED, and the order is the security property:
    /// stage the audience, seal for it, only then upload (media.md § Encryption at rest —
    /// "the seal is resolved BEFORE the attachment is uploaded"). tui's
    /// <c>feed/mod.rs::stage_attachment</c> is the reference leg.</para>
    ///
    /// <para>Returns whether the post actually went out — <see cref="FeedComposeBar.PostButton_Click"/>
    /// clears the boxes that still hold what was sent only on <c>true</c>. A refusal (empty text, an unresolved attachment)
    /// or an aborted gated upload return <c>false</c> and keep the composer exactly as the
    /// shared manager left it (`ui/feed.md` § Persistence → Attachments by content
    /// address).</para></summary>
    private async System.Threading.Tasks.Task<bool> OnComposePostRequested(
        string text, string tags, byte[]? bytes, Controls.ComposeAudience audience)
    {
        var sell = audience.Sell;
        if (_viewModel is null) return false;
        // Guard the submit (async void → a swallowed throw is invisible). Page-/form-
        // level failures already land in the snapshot (compose.error → compose-error);
        // this catches an unexpected throw (e.g. the blob upload) and surfaces it.
        try
        {
            // Resolve the live client ONCE for this submit (see the Nest property).
            var nest = Nest;

            // ── 1. Stage the AUDIENCE first, before a single byte is uploaded ──
            // media.md § Encryption at rest: "The seal is resolved BEFORE the attachment is
            // uploaded, never after — this is a rule, not an implementation detail." Until
            // 2026-09-07 this staged the gate/sell fields AFTER uploading the picked bytes as a
            // PublicPost (plaintext) blob, so attaching a photo and then picking a tier left a
            // readable copy of a restricted post's picture on the nest under a hash anyone can
            // fetch — blob GET is unauthenticated by design and the nest exposes no blob DELETE,
            // so the only fix is to never upload one. `update_compose_gate` / `update_compose_sell`
            // also DROP any stashed seal id, which is the other reason they must run first: a
            // seal minted before them is dead and `prepare_gated_blob` would refuse the submit.
            StageAudience(_viewModel, audience);
            if (sell is { } s)
            {
                // A SOLD post's photo seals under the tier the sale itself mints, and that
                // tier does not exist yet — so the mint is split in two and its first half
                // runs here, before the seal. Only when there IS an attachment: with none,
                // PrepareSellPostAsync runs it itself and the flow is one call, as before.
                if (bytes is not null)
                {
                    await _viewModel.StageSellTierAsync(s.SubscribersGetItFree, s.AskingPrice);
                }
            }

            // ── 2. Seal the attachment for that audience, THEN upload it ──
            // Public compose ⇒ plaintext passthrough, byte-identical to the pre-2026-09-07
            // UploadBlobAsync(PublicPost) shape. Audience-restricted ⇒ shared Rust mints this
            // post's seal_id and seals under derive_post_key(period_key, seal_id); the same id
            // then seals the TextWithMedia body in step 3, so one key opens body and photo.
            // A sell compose has no tier yet (prepare_sell_post mints it), so the shared helper
            // refuses it — the same refusal tui raises, surfaced on compose-error below.
            AttachedFile? attached;
            if (bytes is not null && nest is not null)
            {
                var prepared = await _viewModel.SealComposeAttachmentAsync(bytes);
                var hashHex = await nest.UploadPreparedBlobAsync(
                    prepared.@primary.@sidecarCbor,
                    prepared.@primary.@bytes,
                    prepared.@thumbnail?.@sidecarCbor,
                    prepared.@thumbnail?.@bytes);
                // The sealed class's sidecar says application/octet-stream; the real MIME rides
                // inside the seal, so the MediaItem must take it from the seal's own answer —
                // never from the sidecar and never from the picker's OS guess (`mediaType`). The
                // NAME is the picked file's own, staged on the manager at pick time
                // (SetAttachment → AttachmentStaged, ui/feed.md § Persistence → Attachments by
                // content address) — never the placeholder every submit used to hard-code.
                var pickedName = _viewModel.Compose.attachedFile?.name ?? "image";
                attached = new AttachedFile(pickedName, (ulong)bytes.Length, hashHex, prepared.@mediaType);
            }
            else
            {
                // No fresh bytes to seal — carry whatever the manager already has staged
                // (a restored draft's hash-less handle) through UNCHANGED, so the refusal
                // below still sees it. This is the submit-time half of the same rule the
                // keystroke/post-restore/compose-dialog sites apply: dropping to null here
                // "because this device holds no bytes" IS the silent-drop bug the ruling
                // retires (ui/feed.md § Persistence → Attachments by content address). Only
                // the user's own remove gesture (AttachmentStaged → null) may clear it.
                attached = _viewModel.Compose.attachedFile;
            }
            _viewModel.UpdateCompose(text, tags, attached);

            // ── 3. Let the SHARED manager decide gated-vs-normal ──
            // (feed.md § Encryption at rest; monetization.md § Per-post pay-to-unlock) — by its
            // prepare call's return, the exact linux/tui submit_post shape. Either path yields
            // Some(sealed) ⇒ gated: upload the OPAQUE sealed full body via the
            // PeriodRestrictedPost sidecar (NEVER re-sealed), then submit_gated_post; an upload
            // failure aborts the staged submit (compose keeps its text). None ⇒ ungated: the
            // ordinary submit. The full body never travels/rests in plaintext. Empty price →
            // null price_hint (matches linux's exact shape — a blank field commits to "no price
            // shown", not the literal empty string).
            byte[]? sealedBlob;
            if (sell is { } sellFields)
            {
                var priceHint = string.IsNullOrWhiteSpace(sellFields.Price) ? null : sellFields.Price;
                sealedBlob = await _viewModel.PrepareSellPostAsync(
                    priceHint, sellFields.AskingPrice, sellFields.SubscribersGetItFree);
            }
            else
            {
                sealedBlob = await _viewModel.PrepareGatedBlobAsync();
            }
            if (sealedBlob is not null && nest is not null)
            {
                try
                {
                    var hash = await nest.UploadSealedBlobAsync(_viewModel.GatedUploadSidecar(), sealedBlob);
                    await _viewModel.SubmitGatedPostAsync(hash);
                }
                catch (System.Exception upload)
                {
                    _viewModel.AbortGatedSubmit(Core.Services.Strings.Error(upload));
                    return false; // aborted — the staged post is dropped, draft kept for retry
                }
            }
            else
            {
                await _viewModel.SubmitPostAsync();
            }
            return true;
        }
        catch (System.Exception ex)
        {
            ShellLog.Error(nameof(FeedPage), $"compose submit failed: {ex.Message}");
            var msg = Core.Services.Strings.Error(ex);
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
            return false;
        }
    }

    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Hide an interaction count when 0 (feed.md § Interaction bar — count
    /// hidden at 0, a clean icon-only button until the post has activity), uniform
    /// across all seven apps.</summary>
    public static Visibility CountToVisibility(long count)
        => count > 0 ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Lit-state visual for the like toggle — the same AccentButtonStyle swap
    /// idiom as the Trending pseudo-entry's selected state (SyncFeeds, above): a lit
    /// feed-like-button means the next tap un-likes (feed.md § Interaction bar → Repost).</summary>
    public static Style? LikedToButtonStyle(bool liked)
        => liked ? (Style)Application.Current.Resources["AccentButtonStyle"] : null;

    /// <summary>Lit-state visual for the repost toggle — the repost twin of
    /// <see cref="LikedToButtonStyle"/>: a lit <c>feed-repost-button</c> means the next tap
    /// un-reposts (feed.md § Interaction bar → Repost, ratified 2026-08-10), matching
    /// linux's <c>.reposted</c> CSS class and web's <c>viewer_reposted</c> lit state.</summary>
    public static Style? RepostedToButtonStyle(string? viewerRepostId)
        => viewerRepostId is { Length: > 0 } ? (Style)Application.Current.Resources["AccentButtonStyle"] : null;

    /// <summary>Suppress an element on a REPOST ROW — its own interaction bar renders
    /// nothing (a repost's own counters are structurally dark; feed.md § Interaction bar →
    /// Repost, ratified 2026-08-10), mirroring tui/linux/web's <c>if !is_repost_row</c>.</summary>
    public static Visibility HideOnRepostRow(bool isRepostRow)
        => isRepostRow ? Visibility.Collapsed : Visibility.Visible;

    // ── Post-card withholding arms: muted collapse (topic-factors.md § Scoring —
    //    a mute collapses everywhere; mirrors the dm-message-muted precedent) and
    //    content policy (family-safety.md § Content policy). WHICH arm wins is
    //    decided once in FaunaApp.Core (FeedPostItem.RenderArm → SocialRenderGate,
    //    shared with the conversation bubble and unit-tested there) — this page
    //    only paints the already-resolved gates. ──

    /// <summary>The post-card's accessible Name: the placeholder of whichever arm
    /// is withholding the body — NEVER the body it hides (a blocked or collapsed
    /// body must not leak into the accessibility tree) — else the body itself.
    /// Takes the resolved gates, so the block-beats-muted ordering is not
    /// re-derived here (it lives in <c>SocialRenderGate</c>).</summary>
    public static string PostCardAccessibleName(
        string regionNotice, bool showContentBlockedNotice, bool showMutedCollapse,
        bool showContentCollapse, string bodyText)
    {
        // The region arm paints ahead of every other (region-blocking.md § The
        // blocked render), so its frame is the Name whenever it is withholding.
        if (regionNotice.Length > 0) return regionNotice;
        if (showContentBlockedNotice) return S.Get("family/content_blocked_notice");
        if (showMutedCollapse) return S.Get("feed/post_muted_placeholder");
        if (showContentCollapse) return S.Get("family/content_collapsed_notice");
        return bodyText;
    }

    /// <summary>Reveal a muted post for the session (the mute itself persists —
    /// only un-muting the word stops future collapse). Mutates <c>Revealed</c> in
    /// place on the bound <see cref="FeedPostItem"/> so x:Bind re-renders this row
    /// without a full Reconcile-triggered replace.</summary>
    private void MutedRevealButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        item.Revealed = true;
    }

    /// <summary>Reveal a content-policy <b>collapsed</b> post for the session
    /// (family-safety.md § Content policy — the floor itself persists; only the
    /// guardian relaxing it stops future collapse). Unlike the muted reveal this
    /// marks the id-keyed session set in <c>ContentPolicyCache</c>, so the reveal
    /// survives an observer tick that rebuilds this row into a new item. A
    /// <c>block</c> has no such button: the gate resolves block ahead of the
    /// reveal set, so the collapse arm never paints for one.</summary>
    private void ContentRevealButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        item.RevealContent();
    }

    // ── Convention 17's "a region Block never renders silent" walk
    //    (RegionPlaneHost.BlockRenderState, published as `region_block_render`).
    //    The two sides are registered from DIFFERENT elements on purpose: the
    //    card root carries the verdict side (its item's composed verdict, whatever
    //    arm painted), the placeholder panel the painted side (only when it is
    //    actually shown for a block). An arm that drops or hides the placeholder
    //    then reads as placeholders < blocked. ──

    private static string FeedWitnessKey(FeedPostItem item) => "feed:" + item.PostId;

    private void PostCard_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
            RegionPlaneHost.WitnessBlocked(FeedWitnessKey(item), item.IsRegionBlocked);
    }

    private void PostCard_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
            RegionPlaneHost.WitnessBlocked(FeedWitnessKey(item), false);
    }

    private void RegionPlaceholder_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item } panel)
            RegionPlaneHost.WitnessPainted(
                FeedWitnessKey(item),
                panel.Visibility == Visibility.Visible && item.Region is { IsBlock: true });
    }

    private void RegionPlaceholder_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: FeedPostItem item })
            RegionPlaneHost.WitnessPainted(FeedWitnessKey(item), false);
    }

    /// <summary>Reveal a <b>region</b>-collapsed post for the session
    /// (region-blocking.md § The blocked render — <c>collapse</c> keeps the item one
    /// reveal away). It shares the content-policy reveal set, the family reveal the
    /// design names, so the same tap lifts the composed <c>collapse</c> verdict
    /// whichever source drove it. A region <c>block</c> has no such button.</summary>
    private void RegionRevealButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: FeedPostItem item }) return;
        item.RevealContent();
    }

    private async void BridgeFeedSubscribe_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        // The subscribe kind is the selected available bridge's `id` (the
        // provider id from the nest's server-filtered list), not a hard-coded
        // protocol — version-compatibility.md § Dimension 3.
        var bridge = (BridgeTypeSelector.SelectedItem as ComboBoxItem)?.Tag as string;
        var uri = BridgeFeedUriBox.Text?.Trim();
        var name = BridgeFeedNameBox.Text?.Trim();
        if (string.IsNullOrEmpty(bridge) || string.IsNullOrEmpty(uri)) return;
        try
        {
            await _viewModel.SubscribeBridgeAsync(bridge, uri, name ?? "");
            BridgeFeedPanel.Visibility = Visibility.Collapsed;
            BridgeFeedUriBox.Text = "";
            BridgeFeedNameBox.Text = "";
        }
        catch (System.Exception ex)
        {
            ErrorBar.Message = Core.Services.Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    private void BridgeFeedCancel_Click(object sender, RoutedEventArgs e)
    {
        BridgeFeedPanel.Visibility = Visibility.Collapsed;
    }

    private async void BridgeFeedUnsubscribe_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not long id) return;
        await _viewModel.UnsubscribeBridgeAsync(id);
    }

    public void ShowImageLightbox(Microsoft.UI.Xaml.Media.Imaging.BitmapImage image)
    {
        LightboxImage.Source = image;
        ImageLightboxDialog.Visibility = Visibility.Visible;
    }

    private void ImageLightboxDialog_Click(object sender, RoutedEventArgs e)
    {
        // Click bubbles from the ScrollViewer/Image too; only dismiss when the
        // ORIGINAL source is the backdrop Button itself.
        if (!ReferenceEquals(e.OriginalSource, sender)) return;
        ImageLightboxDialog.Visibility = Visibility.Collapsed;
        LightboxImage.Source = null;
    }

    // UIA Invoke on a nested Button is direct, not pointer-routed (post-card's own
    // comment on load-remote-content-button/feed-post-actions-button), so this never
    // double-fires the post-card's own Click. Reuses the already-loaded bitmap — no
    // second fetch, matching how the card itself painted the image.
    private void PostImage_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Content: Image { Source: Microsoft.UI.Xaml.Media.Imaging.BitmapImage bmp } })
        {
            ShowImageLightbox(bmp);
        }
    }
}

public class SourceToColorConverter : IValueConverter
{
    // Windows's platform-specific post-source color map, keyed off the stable
    // SourceKind id from the shared fauna-feed classifier (PostSourceBadge.Id) —
    // the label text comes from the shared classifier, this only picks a fill
    // (docs/goal/ui/feed.md § Where logic lives).
    public object Convert(object value, System.Type targetType, object parameter, string language)
    {
        return (value as string) switch
        {
            "fauna" => Brush(45, 184, 75),         // 🌿 leaf green (matches web's fauna badge)
            "bluesky" => Brush(0, 133, 255),       // Bluesky blue
            "nostr" => Brush(139, 92, 246),        // Nostr purple
            "activitypub" => Brush(99, 100, 255),  // Fediverse indigo (#6364FF)
            "email" => Brush(245, 158, 11),        // amber
            _ => Brush(0, 0, 0, 0),                // unknown ("other"): transparent, label only
        };
    }

    private static Microsoft.UI.Xaml.Media.SolidColorBrush Brush(byte r, byte g, byte b, byte a = 255)
        => new(Windows.UI.Color.FromArgb(a, r, g, b));

    public object ConvertBack(object value, System.Type targetType, object parameter, string language)
        => throw new System.NotSupportedException();
}
