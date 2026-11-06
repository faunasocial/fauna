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
public sealed partial class FeedPage : Page, IPostCardPage
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

    /// <summary>
    /// The CURRENT nest HTTP client, resolved per call — never captured.
    /// <c>clients.Nest</c> is disposed by <c>App.DisposeNestClients</c> on the next
    /// re-login while this page instance can outlive that hand-off, so a captured
    /// reference starts throwing <see cref="ObjectDisposedException"/>
    /// (<c>apps/windows.md</c> § Client lifetime). Same rule the static
    /// <see cref="PostCardBind.GetImageLoader"/> already follows.
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

    /// <summary>The home of a room-search post hit's room when that room is homed on
    /// another nest (<c>room-search.md</c> § The surface → <i>Opening a hit</i>): the post
    /// opens through <c>resolve_post_from</c> with it. Null for every other deep link.</summary>
    private string? _deepLinkPostHomeUrl;

    /// <summary>The post list's cards — the shared post-card behaviour (the profile's
    /// Posts tab runs the same one over its author window), acting on this page's
    /// timeline view model through <see cref="IPostCardPage"/>.</summary>
    private readonly PostCardHost _cards;

    /// <summary>Page-owned render projections of the snapshot (rebuilt each tick).</summary>
    public ObservableCollection<FeedPostItem> Posts => _cards.Posts;
    public ObservableCollection<FeedDefinition> Feeds { get; } = new();

    /// <summary>
    /// Exposes the compose bar for state-protocol injection (e.g. compose.file E2E test command).
    /// </summary>
    internal Controls.FeedComposeBar FeedComposeBar => ComposeBar;

    public FeedPage()
    {
        this.InitializeComponent();
        _cards = new PostCardHost(this, PostsList, "feed", TipListDialogHost);
        // A region relay fold that changed the answer re-reads every card: the
        // region placeholder is part of FeedPostItem.ContentEquals, so Refresh
        // rebuilds exactly the rows whose verdict moved (region-blocking.md § How an
        // app obtains its region's policy). Raised off the UI thread — marshal back.
        Loaded += (_, _) => RegionPlaneHost.Changed += OnRegionChanged;
        Unloaded += (_, _) => RegionPlaneHost.Changed -= OnRegionChanged;
        // The same re-read when the session nest's rules move (nest-rules.md § A
        // change is announced): the nest placeholder and the guardian's reason are
        // part of FeedPostItem.ContentEquals too. Raised off the UI thread.
        Loaded += (_, _) => NestRulesHost.Changed += OnRegionChanged;
        Unloaded += (_, _) => NestRulesHost.Changed -= OnRegionChanged;
        // The same re-read when the private contact overlay moves: AuthorLabel is part
        // of FeedPostItem.ContentEquals, so exactly the cards whose author was renamed
        // rebuild. Raised off the UI thread — marshal back.
        Loaded += (_, _) => ContactOverlayNames.Moved += OnRegionChanged;
        Unloaded += (_, _) => ContactOverlayNames.Moved -= OnRegionChanged;
    }

    private void OnRegionChanged() => DispatcherQueue.TryEnqueue(Refresh);

    /// <summary>
    /// Feed pages currently attached to the session's feed manager — the VM built and
    /// its render hooked, the page's <see cref="OnNavigatedFrom"/> teardown not yet run.
    /// At most one while the Feed page is on screen, and 0 everywhere else; anything
    /// higher, or a 1 off the feed, is a left page still re-rendering on every manager
    /// tick (<c>apps/windows.md</c> § Client lifetime, the third rule). Surfaced in the
    /// e2e state protocol (<c>diagnostics.live_feed_pages</c>) so the leak fails as
    /// itself, beside <see cref="MainPage.LiveSyncAgentPollers"/>.
    /// </summary>
    internal static int LiveFeedPages;

    /// <summary>Whether this instance counts in <see cref="LiveFeedPages"/>.</summary>
    private bool _attached;

    /// <summary>
    /// Set by <see cref="OnNavigatedFrom"/>: this page has left the screen for good
    /// (FeedPage is not cached, so a later visit is a new instance). <see cref="Page_Loaded"/>
    /// is <c>async void</c> and may resume after it — it then builds nothing, and
    /// <see cref="Refresh"/> / <see cref="ReportPageError"/> paint nothing: a left page
    /// writing the app-wide error mirror would blank, or fake, the error of the page
    /// that is on screen.
    /// </summary>
    private bool _navigatedAway;

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
        => Refresh();

    /// <summary>The page's half of its teardown: stop rendering and detach from the
    /// session's feed manager, which outlives the page.</summary>
    private void DetachFromManager()
    {
        if (_viewModel is not null)
        {
            _viewModel.PropertyChanged -= ViewModel_PropertyChanged;
            _viewModel.DetachFromManager();
        }
        if (_attached)
        {
            _attached = false;
            System.Threading.Interlocked.Decrement(ref LiveFeedPages);
        }
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _crypto = clients.Crypto;
            _account = clients.Account;
            _convSession = clients.ConvSession;
            _deepLinkPostId = clients.DeepLinkPostId;
            _deepLinkPostHomeUrl = clients.DeepLinkPostHomeUrl;

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
        // Runs on every exit, the root-frame ones included (sign-out, account switch,
        // the e2e reset — MainPage ends its inner page, Views.NestedFrame), and on those
        // it runs AFTER the actor change: nothing below may assume a live session.
        _navigatedAway = true;
        // Stop the cue-capture tick + scroll hook and flush every tracked card —
        // mirrors linux's unmap handling (off-screen is off-viewport). Must run
        // before the page is discarded: FeedPage isn't cached
        // (NavigationCacheMode stays Disabled, the default), so a live
        // DispatcherTimer left running would otherwise keep firing forever,
        // sampling a detached ListView, and pin this whole page in memory.
        _cueObserver?.Unwire();
        base.OnNavigatedFrom(e);
        // Stop rendering off the session's manager, which outlives this page: its
        // observer used to stay registered, so every later mutation re-rendered this
        // left page and blanked the app-wide error mirror under whatever page was on
        // screen (DetachFromManager's own doc).
        DetachFromManager();
        // Unsubscribe the VM's reconnect re-hydrate handler — the INestRpcClient
        // seam is app-lifetime, so an un-cleaned VM would leak (and re-fetch into
        // an orphaned snapshot observer).
        _viewModel?.CleanupReconnect();
        _viewModel?.CleanupStoreChange();
        // Draft-persistence v2, feed leg: navigating away destroys the compose
        // control, so flush a still-pending debounced save NOW in case the
        // debounce window hadn't elapsed — fire-and-forget, best-effort (mirrors
        // ConversationsPage.Page_Unloaded's same flush for its own rail).
        // Pending-only (not unconditional): a navigation away from a page nothing
        // was typed into has nothing armed — an unconditional save here wrote a
        // spurious empty snapshot indistinguishable from a real one to anything
        // polling the rail (see FeedDraftsService.FlushIfPendingAsync).
        _ = App.FeedDrafts?.FlushIfPendingAsync();
        if (MainPage.Current is not null && ReferenceEquals(MainPage.Current.ActiveFeedPage, this))
            MainPage.Current.ActiveFeedPage = null;
    }

    /// <summary>
    /// Pair the session's feed manager with the drafts sync held at login and paint the
    /// restored compose onto this page's compose bar — whenever the restore lands.
    /// Fired from <see cref="Page_Loaded"/>, never awaited there.
    /// </summary>
    private async Task RestoreFeedDraftsAsync(
        uniffi.fauna_ffi.FfiFeedManager manager, uniffi.fauna_ffi.IFfiDraftsSync feedDraftsSync)
    {
        // The session's one manager and the sync handle held at login are paired
        // once — FeedDraftsService.EnsureAsync restores only for a new (manager,
        // sync) pair, never on a repeat load. This page is not cached, so every
        // load still repaints its fresh compose bar from the manager's snapshot
        // below. Best-effort: a restore failure must never block the feed page.
        Core.Logs.E2eTrace.Write("[feed-drafts] RestoreFeedDrafts: start");
        try
        {
            var (feedDrafts, ranRestore) = await FeedDraftsService.EnsureAsync(
                App.FeedDrafts, feedDraftsSync, manager, () => new ManagerFeedDraftStore(manager));
            App.FeedDrafts = feedDrafts;
            Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreFeedDrafts: restore ran={ranRestore}");
            var restoredCompose = manager.Snapshot().@compose;
            Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreFeedDrafts: restored text={restoredCompose.@text.Length} chars, tags={restoredCompose.@tags.Length} chars");
            // This page instance's compose bar is new and knows nothing about
            // the manager's compose state or text the user already typed.
            // RestoreDraftIfEmpty only writes into empty fields (never
            // clobbers in-progress typing) and suppresses ComposeChanged for
            // its own assignment (never a restore->forward->re-save loop). For
            // the skipped (already-typed) case, sync the CURRENT UI content
            // into the manager explicitly — the suppressed assignment above
            // cannot reach it via the event.
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
            Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreFeedDrafts: restore threw: {ex.GetType().Name}: {ex.Message}");
        }
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
        if (_rpc is null || _navigatedAway) return;
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
                _rpc, () => _rpc.BuildFeedManagerAsync(_account?.ResolveDeviceId()));
            // The session's post-blob plane — built before the first card paints so
            // a foreign-homed post's bytes never fall back to this nest.
            await Core.PostBlobHostsHost.GetOrBuildAsync(_rpc);
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
            // Left while the builds above were awaited: OnNavigatedFrom has already
            // run its teardown with nothing to tear down, so anything built from here
            // on — the VM's observer on the session manager, the cue-capture timer —
            // would stay attached to a page that is gone, for good.
            if (_navigatedAway) return;
            _viewModel = new FeedViewModel(manager, _observer, _rpc);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            _attached = true;
            System.Threading.Interlocked.Increment(ref LiveFeedPages);
            FeedList.ItemsSource = Feeds;

            // Draft-persistence v2, feed leg (reserved-folders.md § Drafts Sync;
            // feed.md § Persistence) — NOT awaited: the shared DraftsSync retries a
            // launch read lost to the transport for minutes (conversation-drafts.md
            // § Persistence), and the feed page must not wait on it. Landing late is
            // safe because the manager's restore FILLS (PostDrafts::fill).
            if (App.FeedDraftsSync is { } feedDraftsSync)
                _ = RestoreFeedDraftsAsync(manager, feedDraftsSync);

            // Engagement-cue capture shell (engagement-cues.md §§ Cue vocabulary &
            // derivation / At rest, task-6): wire the viewport observer BEFORE
            // hydrating — mirrors linux's wire() then hydrate_cues() sequencing
            // (puts are suppressed pre-hydrate anyway, so the ordering between
            // wiring and hydrate-completing is not itself a race). The hydrate runs
            // ONCE per manager (FeedManagerHost.HydrateCuesOnceAsync): it replaces
            // the live engine, so a later Feed-page load re-running it would discard
            // every verdict folded since. A hydrate failure (an unopenable rollup,
            // never a merely-absent one) is reported on error-message and the page
            // loads on, as linux's and tui's do; the report is a page error
            // (ReportPageError), so it stays on the error line after the feed's own
            // load below rather than being cleared by its first snapshot.
            _cueObserver = new CueViewportObserver(PostsList, () => _viewModel?.Manager, ReportPageError);
            _cueObserver.Wire();
            try
            {
                await Core.FeedManagerHost.HydrateCuesOnceAsync(manager);
            }
            catch (System.Exception ex)
            {
                ShellLog.Warn(nameof(FeedPage), $"cue rollup hydrate failed: {ex.Message}");
                ReportPageError(Core.Services.Strings.Error(ex));
            }

            // Layer-B opt-in (engagement-cues.md § Layer B): the shared producer
            // contributes only while the manager's cached opt-in is true, and that
            // cache is process memory — fill it from the persisted opt-in here so
            // a relaunch (or a second device) is honoured without the user opening
            // Personalization. Independent of the cue hydrate and non-fatal: logged,
            // never painted, never skipping the reload below. A repeatable read, so
            // running it on every feed visit is safe.
            try
            {
                await manager.HydrateSignalOptin();
            }
            catch (System.Exception ex)
            {
                ShellLog.Warn(nameof(FeedPage), $"signal opt-in hydrate failed: {ex.Message}");
            }

            // Initial hydrate: refresh the selector + bridge-feed lists and load the
            // current (local) feed's first page. The snapshot observer drives Refresh().
            await _viewModel.ReloadCommand.ExecuteAsync(null);
            // Left while the hydrate ran: the teardown already detached what was built.
            if (_navigatedAway) return;
            Refresh();

            // search.md § User actions (SearchNav.Post) — fire-and-forget: the
            // dialog it opens blocks on ShowAsync until the user closes it, and
            // awaiting it here would leave LoadingRing spinning (and the
            // `finally` below unreached) for as long as the dialog stays open.
            if (_deepLinkPostId is { } deepLinkPostId)
            {
                var deepLinkHome = _deepLinkPostHomeUrl;
                _deepLinkPostId = null;
                _deepLinkPostHomeUrl = null;
                _ = OpenPostDetailByIdAsync(deepLinkPostId, deepLinkHome);
            }
        }
        catch (System.Exception ex) when (_navigatedAway)
        {
            // A load the page left before it finished, failing on a session the
            // actor change already ended: never this screen's error to paint.
            ShellLog.Info(nameof(FeedPage), $"feed page load ended with the page: {ex.GetType().Name}: {ex.Message}");
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
        // A failure reported after the page left (a cue emit flushed at teardown, an
        // unlock that resolved late) is this page's, and the page is gone: never paint
        // it on the app-wide mirror under the page now on screen.
        if (_navigatedAway) return;
        _pageError = message;
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = message;
    }

    // ── IPostCardPage — what the post list's cards (PostCardHost) act on and paint on.
    //    The cards act on this page's timeline view model; their failures land on this
    //    page's error-message exactly as the handlers did when they lived here. ──

    FeedViewModel? IPostCardPage.CardViewModel => _viewModel;
    INestRpcClient? IPostCardPage.Rpc => _rpc;
    ICryptoService? IPostCardPage.Crypto => _crypto;
    ISessionAccount? IPostCardPage.Account => _account;

    void IPostCardPage.ReportPageError(string message) => ReportPageError(message);

    /// <summary>A verb that lands clears a previous verb's error, as web's does.</summary>
    void IPostCardPage.ClearPageError()
    {
        if (_pageError is null) return;
        _pageError = null;
        Refresh();
    }

    void IPostCardPage.ShowError(string message)
    {
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = message;
    }

    void IPostCardPage.ShowReportStatus(string? acknowledgement)
    {
        ReportStatusText.Text = acknowledgement ?? "";
        ReportStatusText.Visibility = acknowledgement is null ? Visibility.Collapsed : Visibility.Visible;
    }

    void IPostCardPage.RefreshCards() => Refresh();

    /// <summary>Open the page's lightbox overlay (<c>image-lightbox</c>) on a picture
    /// already loaded — a card's, or the post detail's.</summary>
    public void ShowImageLightbox(Microsoft.UI.Xaml.Media.Imaging.BitmapImage image) => Lightbox.Show(image);

    /// <summary>A card's activation opens its detail dialog. A REPOST ROW's own detail
    /// would be blank (the repost post is empty by construction) — activation opens the
    /// ORIGINAL's detail instead (feed.md § Interaction bar → Repost, ratified
    /// 2026-08-10), mirroring linux/web/apple. <see cref="OpenPostDetailByIdAsync"/>
    /// resolves through the same deep-link door a search hit uses, so an original outside
    /// the loaded window still opens with real content instead of degrading to a no-op.</summary>
    Task IPostCardPage.OpenPostDetailAsync(FeedPostItem item)
        => item.IsRepostRow
            ? OpenPostDetailByIdAsync(item.RepostedPostId!)
            : ShowPostDetailDialogAsync(item);

    /// <summary>The last fire-and-forget failure reported via <see cref="ReportPageError"/>.
    /// Held separately from the snapshot's own error because <see cref="Refresh"/> runs on
    /// every observer tick and would otherwise clear it on the next tick — see the note
    /// there. Reset on each page load.</summary>
    private string? _pageError;

    /// <summary>Rebuild every bound surface from the latest snapshot. Cheap; runs
    /// on every observer tick (mirrors ConversationsPage.Refresh()).</summary>
    private void Refresh()
    {
        if (_viewModel is null || _navigatedAway) return;

        SyncFeeds(_viewModel.Feeds);
        // The imported-posts selection's notice + clear ← snapshot.imported_selected.
        if (_viewModel.ImportedPlatformLabel is { } importedPlatform)
        {
            ImportedNotice.Text = S.Format("feed/imported_notice", importedPlatform);
            ImportedClearButton.Content = S.Get("feed/imported_clear");
            ImportedRow.Visibility = Visibility.Visible;
        }
        else
        {
            ImportedRow.Visibility = Visibility.Collapsed;
        }
        _cards.Sync(_viewModel.Posts);
        _repaintDetailLabel?.Invoke();
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
        // there is one; otherwise the page error persists until the next load. An open
        // thread page's own failure leads ahead of both (FeedPage.Thread.cs).
        var err = OpenThreadPageError();
        if (string.IsNullOrEmpty(err)) err = _viewModel.ErrorText;
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

        PaintThreadPage();

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

    private async void ImportedClear_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.ClearImportedAsync();
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
    private async Task OpenPostDetailByIdAsync(string postId, string? homeNestUrl = null)
    {
        if (_viewModel is null) return;

        if (FindRenderedPost(postId) is { } loaded)
        {
            await ShowPostDetailDialogAsync(new FeedPostItem(loaded, _viewModel.Manager, _viewModel.BridgeRoster));
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

        await Controls.Dialogs.ShowAsync(dialog, prepare: () =>
        {
            TrackDetailDialog(dialog);
            _ = ResolveAndFillPostDetailAsync(dialog, postId, homeNestUrl);
        });
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
    private async Task ResolveAndFillPostDetailAsync(
        ContentDialog dialog, string postId, string? homeNestUrl = null)
    {
        if (_viewModel is null) return;
        try
        {
            // A post found in a room homed on another nest resolves through the
            // deep-link door's `from` form — the room's recorded home, never a guess;
            // otherwise this nest (`room-search.md` § The surface → Opening a hit).
            if (homeNestUrl is { Length: > 0 })
                await _viewModel.Manager.ResolvePostFrom(postId, homeNestUrl);
            else
                await _viewModel.Manager.ResolvePost(postId);
            if (FindRenderedPost(postId) is { } resolved)
            {
                var resolvedItem = new FeedPostItem(resolved, _viewModel.Manager, _viewModel.BridgeRoster);
                var panel = BuildPostDetailPanel(resolvedItem, out var bodyText, out var postImageInsertIndex);
                dialog.Content = new ScrollViewer { Content = panel, MaxHeight = 400 };
                if (resolvedItem.GatedTier is { Length: > 0 } && !resolvedItem.GatedUnlocked
                    && !resolvedItem.DetailWithholdsBody)
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
    /// <summary>The repaint of the open post detail's <c>content-label-badge</c>, or
    /// <c>null</c> when no detail is open. Run on every feed tick (<see cref="Refresh"/>),
    /// because the room labeler's verdict reaches the post after the dialog was built.</summary>
    private Action? _repaintDetailLabel;

    /// <summary>The detail dialog's <c>content-label-badge</c>: Collapsed — absent — until the
    /// post carries a label, then styled entirely from the shared <c>content_label_style</c>
    /// map through <see cref="FeedPostItem"/>'s own badge properties (the list card's source),
    /// never a category string or colour of this file's.</summary>
    private Border BuildDetailLabelBadge(string postId)
    {
        var icon = new TextBlock { FontSize = 10, VerticalAlignment = VerticalAlignment.Center };
        var text = new TextBlock { FontSize = 10, VerticalAlignment = VerticalAlignment.Center };
        var badge = new Border
        {
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(4, 1, 4, 1),
            HorizontalAlignment = HorizontalAlignment.Left,
            Visibility = Visibility.Collapsed,
            Child = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 3,
                Children = { icon, text },
            },
        };
        AutomationProperties.SetAutomationId(badge, Ids.ContentLabelBadge);

        void Repaint()
        {
            if (_viewModel is null || FindRenderedPost(postId) is not { } post) return;
            var labelled = new FeedPostItem(post, _viewModel.Manager, _viewModel.BridgeRoster);
            if (!labelled.HasContentLabel)
            {
                badge.Visibility = Visibility.Collapsed;
                return;
            }
            icon.Text = labelled.ContentLabelIcon;
            text.Text = labelled.ContentLabelText;
            text.Foreground = HexColorToBrushConverter.FromHex(labelled.ContentLabelAccent);
            badge.Background = HexColorToBrushConverter.FromHex(labelled.ContentLabelTint, 38);
            AutomationProperties.SetName(badge, labelled.ContentLabelText);
            badge.Visibility = Visibility.Visible;
        }

        Repaint();
        _repaintDetailLabel = Repaint;
        return badge;
    }

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
        panel.Loaded += (_, _) =>
        {
            RegionPlaneHost.WitnessBlocked(witnessKey, item.IsRegionBlocked);
            NestRulesHost.WitnessBlocked(witnessKey, item.IsNestBlocked);
        };
        panel.Unloaded += (_, _) =>
        {
            RegionPlaneHost.Forget(witnessKey);
            NestRulesHost.Forget(witnessKey);
        };

        // A collapse's reveal (region, nest or family — one shared reveal set)
        // repaints this dialog in place: rebuild the panel for the now-revealed item
        // and move its children across, keeping this root (and its witness hooks).
        void RevealInPlace()
        {
            item.RevealContent();
            var fresh = BuildPostDetailPanel(item, out _, out _);
            var children = fresh.Children.ToList();
            fresh.Children.Clear();
            panel.Children.Clear();
            foreach (var child in children) panel.Children.Add(child);
        }

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

        // Admin-blocked post (takedown-and-block.md § Admin block of a reported item):
        // for any viewer but its author the nest withheld the body, so the shared
        // notice stands in the body area exactly as the tombstone above does (tui's
        // post-detail arm). The author's own read keeps the body; the notice then
        // paints right after it, below.
        if (item.AdminBlockNotice is { } blockedNotice && item.IsAdminBlockedInPlace)
        {
            bodyText = new TextBlock
            {
                Text = blockedNotice,
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
            var placeholder = RegionPlaceholderPanel.Build(region, onReveal: RevealInPlace);
            placeholder.Loaded += (_, _) => RegionPlaneHost.WitnessPainted(witnessKey, region.IsBlock);
            panel.Children.Add(placeholder);
            // No feed-post-detail-body: the body is withheld. The out param still needs
            // an element; the callers skip the gated-unlock repaint for a withheld post.
            bodyText = new TextBlock();
            postImageInsertIndex = panel.Children.Count;
            return panel;
        }

        // Nest placeholder (nest-rules.md § The nest source in the engine): a rule of
        // the nest this post was read through withholds it — the frame naming the
        // nest, its domain and the admin's reason VERBATIM in place of the whole post,
        // right after the region arm exactly as on the list card. A `collapse`
        // reveals through the shared reveal set; a `block` has no reveal.
        if (item.Nest is { } nest && item.ShowNestPlaceholder)
        {
            var placeholder = NestPlaceholderPanel.Build(nest, onReveal: RevealInPlace);
            placeholder.Loaded += (_, _) => NestRulesHost.WitnessPainted(witnessKey, nest.IsBlock);
            panel.Children.Add(placeholder);
            bodyText = new TextBlock();
            postImageInsertIndex = panel.Children.Count;
            return panel;
        }

        // The family arm composes here too (family-safety.md § Content policy;
        // nest-rules.md § Implementation status today — web, linux, android and apple
        // each closed this hole): a guardian-withheld post opened from its card paints
        // the same placeholder — the fixed frame, the guardian rule's reason beside it
        // (`content-policy-blocked-reason`) — never its body. A `block` has no reveal;
        // a `collapse` shares the card's reveal set. The detail arm leaves out the
        // card's muted collapse: the viewer opened this post on purpose.
        var detailArm = item.DetailArm;
        if (detailArm is SocialRenderArm.ContentBlocked or SocialRenderArm.ContentCollapsed)
        {
            var collapsed = detailArm == SocialRenderArm.ContentCollapsed;
            panel.Children.Add(ContentPolicyPlaceholderPanel.Build(
                collapsed,
                collapsed ? S.Get("family/content_collapsed_notice") : item.ContentBlockedNoticeText,
                item.GuardianReason,
                onReveal: RevealInPlace));
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
        // The author's own blocked post keeps its body; the shared notice paints right
        // after it, with no id of its own (none approved — tui rides it as chrome).
        if (item.AdminBlockNotice is { } authorNotice)
        {
            panel.Children.Add(new TextBlock
            {
                Text = authorNotice,
                TextWrapping = TextWrapping.Wrap,
                FontStyle = Windows.UI.Text.FontStyle.Italic,
                Opacity = 0.6,
                FontSize = 12,
            });
        }

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

        // Content-label badge (`content-label-badge`, moderation.md § Per-row badge data
        // path) — the detail twin of the list card's badge. A post opened through the
        // deep-link door has no post-card, so the detail is where a member of the room
        // sees the room labeler's verdict, and the verdict is merged only once the post
        // unseals — so the badge is repainted from the manager's own snapshot on every feed
        // tick while this dialog is open (RepaintDetailLabel), not painted once.
        panel.Children.Add(BuildDetailLabelBadge(item.PostId));

        // Post image (post-image) — painted from the folded `Image` block in the document
        // (render-model.md § D6), the same media the list card shows, via the shared blob
        // loader attached property (async, non-blocking; the bytes load stays client glue —
        // render-model.md § The boundary). Closes the gap where the detail omitted the image.
        // A gated post's pre-unlock teaser carries no Image block yet, so this is null here;
        // postImageInsertIndex marks the slot so UnlockGatedDetailAsync can insert the SAME
        // element once the unlock lands.
        postImageInsertIndex = panel.Children.Count;
        if (BuildPostDetailImageElement(item.Document, item.BlobHome) is { } postImage)
            panel.Children.Add(postImage);

        // Video thumbnail (video-thumbnail) — the D6b `Video` sibling of `Image`
        // (render-model.md § Implementation status today), the detail-dialog twin of the
        // list card's XAML render above: the SAME VideoThumbnailView, so the detail plays
        // inline exactly as the card does (§ D6c → Inline playback), off the same block. A
        // bridged post's `ProxiedVideo` paints its nest-relative path where the hash goes
        // (§ D6c → Proxied video), is never byte-loaded before the tap, and plays off the
        // ticketed path the shared decision answers for it.
        if (DocumentRenderer.MediaVideoBlock(item.Document) is { } videoBlock)
        {
            var video = new Controls.VideoThumbnailView
            {
                Block = videoBlock,
                Home = item.BlobHome,
                Margin = new Thickness(0, 4, 0, 0),
            };
            AutomationProperties.SetAutomationId(video, Ids.VideoThumbnail);
            panel.Children.Add(video);
        }

        // The author alone carries the id — the same resolved label the card paints (the
        // viewer's nickname, else the public name, else the canonical short id), as web's
        // and linux's detail do — so "which post did this open" reads as an author, not a
        // sentence; the time sits beside it. (It was one hand-rolled English line,
        // "by ‹author› · ‹time›", which no other app's detail painted.)
        var authorRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        // The author activates their profile (ui.yaml `click feed-post-detail-author →
        // profile`; the id stays `type: text`) — a borderless link so UIA offers Invoke.
        var authorText = new HyperlinkButton
        {
            Content = item.AuthorLabel,
            Opacity = 0.6,
            Padding = new Thickness(0),
            MinHeight = 0,
            Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorPrimaryBrush"],
        };
        authorText.Click += (_, _) =>
        {
            _detailDialog?.Hide();
            _cards.OpenAuthorProfile(item.AuthorHex, item.HomeNestUrl);
        };
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
            tipListButton.Click += (_, _) => _cards.OpenTipList(item);
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

        // Reaction pills (feed.md ruling 4): the same per-emoji pills the list card paints,
        // unscoped here in post_detail.
        if (item.HasReactionPills)
            panel.Children.Add(_cards.BuildDetailReactionPills(item));

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

        // *View Thread* (bridges.md § Bluesky-native thread view): a Bluesky post has a
        // conversation to open, which takes this dialog's place (FeedPage.Thread.cs).
        if (item.OffersThreadView)
            panel.Children.Add(BuildViewThreadButton(item));

        return panel;
    }

    /// <summary>Builds the detail dialog's <c>post-image</c> element from <paramref
    /// name="document"/>'s folded <c>Image</c> block — <c>null</c> when the document carries
    /// no media (most posts). The one construction site for BOTH the initial
    /// <see cref="BuildPostDetailPanel"/> build and <see cref="UnlockGatedDetailAsync"/>'s
    /// post-unlock insert, so a gated post's pre-unlock teaser (no Image block) and its
    /// unsealed document (one now folded in) paint through the identical element.
    /// <para>A bridged post's <c>ProxiedImage</c> (render-model.md § D6c) paints here too
    /// when the document has no blob image: an Image loaded by its nest-relative path, over
    /// a placeholder that stands in until the bytes land, both inside a Button that carries
    /// the <c>post-image</c> id — the list card's own shape (<c>PostImageButton</c> over
    /// <c>PostImagePlaceholder</c>). The id cannot sit on the Image there, as it does for a
    /// blob: an Image with no picture has no size, so UIA would read the slot offscreen for
    /// exactly as long as the placeholder stands in. The Button has no click: the lightbox
    /// is a page overlay, which the dialog covers.</para></summary>
    private static FrameworkElement? BuildPostDetailImageElement(
        uniffi.fauna_core.RenderDocument document, object? home = null)
    {
        if (PostCardBind.GetImageLoader() is not { } loader) return null;
        var mediaHash = DocumentRenderer.MediaImageHash(document);
        var proxiedPath = mediaHash is null ? DocumentRenderer.MediaProxiedPath(document) : null;
        if (mediaHash is null && proxiedPath is null) return null;

        var postImage = new Image
        {
            MaxHeight = 300,
            Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        ImageHashBind.SetLoader(postImage, loader);
        if (mediaHash is not null)
        {
            postImage.Margin = new Thickness(0, 4, 0, 4);
            AutomationProperties.SetAutomationId(postImage, Ids.PostImage);
            ImageHashBind.SetHome(postImage, home);
            ImageHashBind.SetHash(postImage, mediaHash);
            return postImage;
        }

        var placeholder = new Border
        {
            Width = 160,
            Height = 100,
            CornerRadius = new CornerRadius(4),
            HorizontalAlignment = HorizontalAlignment.Left,
            Background = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemControlBackgroundBaseLowBrush"],
            Visibility = Visibility.Collapsed,
            Child = new TextBlock
            {
                Text = "\uE8B9",
                FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
                FontSize = 24,
                Opacity = 0.5,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            },
        };
        var slot = new Grid();
        slot.Children.Add(placeholder);
        slot.Children.Add(postImage);
        var button = new Button
        {
            Content = slot,
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent),
            BorderThickness = new Thickness(0),
            Padding = new Thickness(0),
            HorizontalAlignment = HorizontalAlignment.Left,
            HorizontalContentAlignment = HorizontalAlignment.Left,
            Margin = new Thickness(0, 4, 0, 4),
            Visibility = Visibility.Collapsed,
        };
        AutomationProperties.SetAutomationId(button, Ids.PostImage);
        ImageHashBind.SetVisibilityTarget(postImage, button);
        ImageHashBind.SetPlaceholder(postImage, placeholder);
        ImageHashBind.SetProxiedPath(postImage, proxiedPath);
        return button;
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
            TrackDetailDialog(dialog);
            // Gated post: the body painted above is the public TEASER (item.Document). Kick
            // off the unlock — fetch the sealed blob + decrypt (author custody / subscriber
            // KeyBlob) — and repaint feed-post-detail-body with the full body. bodyText is a
            // live element in the shown dialog, so the async repaint lands on the open dialog.
            // Fire-once: skip if already unlocked (feed.md § Encryption at rest — detail-open
            // unlock).
            if (item.GatedTier is { Length: > 0 } && !item.GatedUnlocked && !item.DetailWithholdsBody)
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
            // The sealed body rests on the post's own home: through the shared plane
            // with the home read off the post, unchanged (transport-trust.md § The
            // post arm) — this nest's own plane when the post has no foreign home.
            var hosts = Core.PostBlobHostsHost.Current;
            var home = FindRenderedPost(postId) is { } homed
                ? uniffi.fauna_ffi.FaunaFfiMethods.PostBlobHome(homed)
                : null;
            var blob = hosts is not null
                ? await hosts.Get(home, hash)
                : home is null
                    ? await nest.GetBlobAsync(hash)
                    : throw new System.InvalidOperationException("no post-blob plane for a foreign-homed post");

            step = "decrypt under a held period key";
            await _viewModel.UnlockGatedPostAsync(postId, blob);

            // Re-read the now-unlocked post from the MANAGER, not from `_viewModel.Posts`.
            // `FeedViewModel.Snapshot` memoizes (`_cachedSnapshot ??= _manager.Snapshot()`)
            // and is invalidated only by the observer's PropertyChanged, which
            // FeedNotifyObserver hops onto the dispatcher — so the cache is still holding the
            // PRE-unlock PostSummary when this continuation resumes, and the guard below
            // reads `gatedUnlocked: false` for a post the manager has already unsealed. The
            // manager's own Snapshot() is the authoritative post-mutation read.
            // A post opened through the deep-link door (a room-search hit) lives in the
            // snapshot's `deepLinkedPost` slot, not `posts` — so the union lookup, which
            // reads the manager's own Snapshot() all the same.
            var updated = FindRenderedPost(postId);
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
            if (BuildPostDetailImageElement(updated.document, home) is { } postImage)
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
        // Each option is a FactorOption: the painted label as its Name, the stable
        // factor key as its Tag and AutomationId (see FactorOption). The built-in
        // head (engagement, trending, …) comes from the shared FFI (feed.md § Where
        // logic lives → Feed factor-picker built-ins), so a new built-in reaches
        // every app without a per-app literal; subscribed labeler:<hex> factors and
        // trained topics append once the fetches below resolve.
        foreach (var option in FaunaFfiMethods.BuiltinFactorOptions())
            factorCombo.Items.Add(FactorOption(S.Resolve(option.label), option.value));
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
                // A labeler has no display name anywhere in the system: its key IS
                // what the picker paints.
                factorCombo.Items.Add(FactorOption(entry.factor, entry.factor));
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(FeedPage), $"factor options fetch failed: {ex.Message}");
        }
    }

    /// <summary>One <c>feed-factor-select</c> option. Its Name is the label it paints —
    /// what a screen reader reads, and what the bridge's <c>options</c> attribute answers,
    /// so the picker lists a trained topic by the name the user gave it and a Layer-B
    /// signal as "Watched to the end", never <c>topic:&lt;hex&gt;</c> or
    /// <c>signal:watch-complete</c> (the cross-app <c>option_texts</c> contract,
    /// <c>drivers/base.py</c>; topic-factors.md § Authoring surface &amp; picker). The
    /// stable key is its Tag (what the composition stores) and its AutomationId, which the
    /// bridge's <c>Select</c> matches as well as the Name, so <c>driver.select</c> keeps
    /// naming the key exactly as on every other app.</summary>
    private static ComboBoxItem FactorOption(string label, string key)
    {
        var item = new ComboBoxItem { Content = label, Tag = key };
        AutomationProperties.SetName(item, label);
        AutomationProperties.SetAutomationId(item, key);
        return item;
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
                factorCombo.Items.Add(FactorOption(row.name, key));
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
    /// differently. The sale arm exists only where the payments plane does: a store-safe
    /// composer offers no "Sell this post…" answer, so its audience never carries one
    /// (dynamic-features.md § Platform-family surface excision → The price-and-route class).</summary>
    private static void StageAudience(FeedViewModel vm, Controls.ComposeAudience audience)
    {
#if PAYMENTS
        if (audience.Sell is { } s)
        {
            vm.UpdateComposeSell(s.Price, s.AskingPrice, s.SubscribersGetItFree, audience.GatePreview);
            return;
        }
#endif
        if (audience.GateRoom is not null)
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
#if PAYMENTS
        var sell = audience.Sell;
#endif
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
#if PAYMENTS
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
#endif

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
#if PAYMENTS
            if (sell is { } sellFields)
            {
                var priceHint = string.IsNullOrWhiteSpace(sellFields.Price) ? null : sellFields.Price;
                sealedBlob = await _viewModel.PrepareSellPostAsync(
                    priceHint, sellFields.AskingPrice, sellFields.SubscribersGetItFree);
            }
            else
#endif
            {
                sealedBlob = await _viewModel.PrepareGatedBlobAsync();
            }
            // Which arm the submit takes is the first question about a post that went
            // out but not as written: a restricted audience (a restored draft's sale,
            // say) sends the teaser as the visible body.
            Core.Logs.E2eTrace.Write($"[feed-compose] submit arm={(sealedBlob is null ? "plain" : "restricted")}");
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

}
