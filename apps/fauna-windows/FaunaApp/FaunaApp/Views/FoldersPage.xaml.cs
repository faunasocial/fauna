using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_conversations;
using uniffi.fauna_devices_machine;
using uniffi.fauna_ffi;
using uniffi.fauna_folders_machine;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Folders sub-page (folders.md; ui.yaml § folders): the folder
/// CONTROL PLANE — the create wizard, the folder list + per-set config (selective
/// sync + conflict policy), the candidate-model conflict surface, and the desktop
/// local-folder binding nested under each set.
///
/// A thin renderer over two seams (no business logic client-side — folders.md
/// § Where logic lives):
/// <list type="bullet">
/// <item>the shared-Rust <see cref="DevicesMachine"/> (libs/fauna-devices-machine, via
/// libs/fauna-ffi) for the folder / conflict / wizard slices (<c>Refresh</c> reads;
/// <c>DeleteFolder</c> / <c>ResolveConflict</c> / <c>SetFolderPaths</c> gestures; the
/// embedded <c>Wizard</c>) — the same page-level machine the roster
/// (<see cref="DevicesPage"/>) reads, this page renders only its folder/conflict/
/// wizard slices;</item>
/// <item>the <see cref="LocationsViewModel"/> named-pipe seam (<c>\\.\pipe\fauna-sync</c>)
/// for the per-device local-folder binding — nested under each set, contextual to it
/// (no free-text set name; D3 / O-1).</item>
/// </list>
///
/// Split out of the former top-level SyncPage + the former Settings → Sync
/// (SyncFoldersPage) by the 2026-06-28 sync/folder UI unification (spec §§ 1, 3, 4, 6).
/// The standalone "Sync conflicts" (conflicts-tab) page is retired; conflicts render here.
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI page that mutates bound state off
/// the UI thread throws a silent <c>COMException</c>
/// (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
public sealed partial class FoldersPage : Page, IAsyncLoadedPage
{
    private INestRpcClient? _rpc;

    /// <summary>e2e-only: a one-shot artificial delay (ms) applied at the start of the
    /// async <see cref="Page_Loaded"/> so the nav-readiness regression test can
    /// DETERMINISTICALLY widen the load window (set via the <c>folder_load_delay_ms</c>
    /// test command; <see cref="App"/>.HandleTestCommand). Consumed — reset to 0 — by the
    /// next load. Zero in production.</summary>
    internal static int TestLoadDelayMs;

    // ── e2e nav-readiness barrier (IAsyncLoadedPage) ──
    // Completed at the END of the async Page_Loaded so the TestAgent can hold ready=false
    // until this page has actually loaded (see IAsyncLoadedPage) — otherwise the agent
    // signals ready after the synchronous frame-nav kickoff, while _machine is still null,
    // and an immediately-following folder-add-button click is silently dropped. Reset per
    // navigation (OnNavigatedTo) so a reused (cached) instance re-arms.
    // RunContinuationsAsynchronously so completing it from Page_Loaded never resumes the
    // awaiting agent inline on the UI thread.
    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);
    public Task LoadComplete => _loadComplete.Task;

    // ── Cross-user sharing (owner side) — folders.md § Sharing ──
    // The shared-Rust conversations session (owns the live per-actor MlsEngine the
    // share_set / remove_member free-fns ride), plus the session's bearer/deviceId/crypto
    // (App.xaml.cs's session-scoped hydration provisioning loop sources the same
    // values at the App level; a Page reaches them via the ServiceClients nav param
    // instead). Null in E2E bridge mode / a conversations-session build failure at
    // login — the Sharing UI's share/remove actions guard for null rather than
    // failing the whole page load.
    private ConversationsSession? _convSession;
    private INestHttpClient? _nestHttp;
    private ISessionAccount? _account;
    private ICryptoService? _crypto;

    // ── The session's shared machine (folders / conflicts / wizard) ──
    private DevicesMachine? _machine;
    private DevicesNotifyObserver? _observer;
    /// <summary>This page's listener on the session machine's observer fan-out,
    /// disposed in <see cref="OnNavigatedFrom"/>.</summary>
    private IDisposable? _listening;

    // ── Offline co-present share ceremony (p2p.md § Offline share initiation) ──
    // The seat/panel/status/expecting-from themselves live on App (they must
    // survive this page being torn down and rebuilt across a navigation — see
    // App.CurrentOfflineShareSeat); this page only holds the load-once-per-visit
    // fetch (mirrors _canServeWebdav / _ownTiers) that RenderPendingShares and
    // RenderFolders both read to append the consent-card / landed-scope rows.
    // The M2 knock list's last fetch, kept so the ceremony's arm can repaint
    // the shared container without re-asking the nest (LoadGroupSharesAsync).
    private IReadOnlyList<uniffi.fauna_ffi.FfiPendingShare> _pendingShares =
        Array.Empty<uniffi.fauna_ffi.FfiPendingShare>();
#if P2P_SHARE
    private FfiGroupShareViews _groupShareViews =
        new FfiGroupShareViews(Array.Empty<FfiPendingGroupShare>(), Array.Empty<FfiGroupScope>());
    // The section control this page hosts (Views/P2pShare) — see
    // EnsureOfflineShareSection.
    private P2pShare.OfflineShareSection? _offlineShare;
#endif

    /// <summary>Whether the owner can serve ANY set over WebDAV (has a mail MSEK) —
    /// cached once per page-visit, re-read on nav (mirrors web/linux/android: enabling
    /// mail elsewhere re-enables the toggle without an app restart). A failed read is
    /// fail-safe <c>false</c> (webdav-server.md § Implementation status → <c>6b-2 c
    /// disable-with-hint</c>) and never breaks the rest of the page load.</summary>
    private bool _canServeWebdav;

    /// <summary>The creator's own subscription tier NAMES — the options the web-type
    /// paywall select offers (folders.md § Web paywall). Read per page-visit off the
    /// key-bearing subscriptions face (<c>SubscriptionsClient::tiers_list</c>), NOT the
    /// keyless DevicesMachine snapshot, and re-read on nav so a tier created elsewhere
    /// enables the select without an app restart (mirrors <see cref="_canServeWebdav"/>).
    /// A failed read is fail-safe EMPTY — the select renders disabled with the "create a
    /// tier first" hint rather than offering a pick that cannot succeed.</summary>
    private IReadOnlyList<string> _ownTiers = Array.Empty<string>();

    // ── Local-folder binding (the user-session fauna-sync helper over a named pipe,
    // a SEPARATE seam from the nest RPC). Best-effort: the helper is usually not
    // running (on-demand sync is opt-in), so a load failure leaves the folder lists
    // empty rather than erroring the whole page. ──
    private LocationsViewModel? _syncVm;

    /// <summary>E2E seam: when set (by the <c>sync_inject_locations</c> TestAgent command),
    /// the page builds its folder-binding VM over this in-memory channel fake instead of
    /// the live agent, because the add flow opens a native folder picker the drivers can't
    /// drive. Null in production.
    ///
    /// <para>A <b>pure render fixture</b>: the injected channel comes with its own
    /// throwaway binding model, so it never moves the session model that
    /// <c>AppDataSnapshot.GetSyncForState</c> reports. That is what stops an injected
    /// folder list from making <c>data.sync</c> claim a real engine is serving — the same
    /// line linux draws between <c>sync_add_location</c> and <c>sync_inject_locations</c>.</para>
    ///
    /// <para>Deliberately NOT `#if DEBUG || FAUNA_E2E_AGENT`-gated (testing.md convention
    /// 15): this page READS it unconditionally, so gating the field would gate the read
    /// too. Only the now-gated e2e command table writes it, so a Release build warns
    /// CS0649 "never assigned" — expected, and exactly the guarantee the convention
    /// wants: in a production build this is a `null` no-one can ever set.</para></summary>
    internal static ILocationControlChannel? TestLocationChannelOverride;

    /// <summary>The login-scoped control plane this page is currently rendering, while it is
    /// subscribed to it. Held only so <see cref="OnNavigatedFrom"/> can unsubscribe from the
    /// same instance it subscribed to (a re-login swaps the controller).</summary>
    private LocationBindingsController? _boundController;

    /// <summary>
    /// The control plane's rendered union changed — a background reconcile landed (an
    /// agent-reachable edge, or the session-install attach that pushes bindings made during
    /// login). This page is a pure RENDERER of that model: it does not drive the reconcile,
    /// which is exactly why the gap cannot come
    /// back through it — with the page closed there is simply no subscriber, and the
    /// controller reconciles anyway.
    ///
    /// <para>Raised on the convergence loop's tokio task, so it hops to the UI thread: the
    /// render mutates the VM's bound <c>Locations</c> collection, and a WinUI VM mutated off
    /// the UI thread throws a silent COMException.</para>
    /// </summary>
    private void OnBindingsChanged()
    {
        DispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                var parkedBefore = ParkedFolders();
                _syncVm?.Render();
                // A park that came or went changes a member row's SHAPE (the shared
                // binding_section decision: a demoted writer's parked binding keeps the
                // expandable row, and its body heads with the warning), so rebuild the
                // rows — RenderFolders re-opens whichever one was open.
                if (!parkedBefore.SetEquals(ParkedFolders()) && _machine is not null)
                    RenderFolders(_machine.Snapshot().folders);
            }
            catch (Exception ex)
            {
                E2eTrace.Write($"[folders] binding re-render threw: {ex.Message}");
            }
        });
    }

    // The localized machine error (snapshot.error) currently displayed, combined with
    // the folder-binding VM error in UpdateErrorBar so both seams share the one
    // canonical error-message surface.
    private string? _machineError;

    // The folder whose destructive delete-confirm dialog is open (carried from
    // folder-delete-button click to folder-delete-confirm click).
    private string? _pendingDeleteFolder;
    // The folder whose flip-to-metadata-only residency confirm dialog is open
    // (carried from the residency select's arm to folder-residency-confirm click).
    private string? _pendingResidencyFolder;
    // The currently-expanded folder row's path editors + name. Only one row is
    // expanded at a time (body realized on Expanding, cleared on Collapsed), so the
    // unindexed body IDs (folder-include-paths / -exclude-paths / -save-paths and the
    // nested folder-location-* family) resolve uniquely to it.
    private TextBox? _expandedInclude;
    private TextBox? _expandedExclude;
    private string? _expandedFolder;
    // The set whose share flow a folder-share route asked to open — set by
    // RenderFolders as it expands that row, consumed by BuildSharingSection.
    private string? _openShareFor;
    // Nested local-folder binding controls for the single open set row.
    private StackPanel? _expandedLocationContainer;
    private TextBlock? _expandedLocationEmpty;
    private TextBox? _expandedLocationInput;
    // The currently-expanded row's per-set device-activity roster container
    // (fauna.folders.devices — the ordinary sync change signal). Same
    // single-open-row lifecycle as the fields above; OnFolderChangedPushed reads
    // it to live-repaint on every fauna.sync.changed push for this exact set.
    private StackPanel? _expandedDeviceActivityContainer;

    // ── Folder creation wizard (embedded FolderWizardMachine) ──
    private bool _wizardDialogShown;
    private readonly List<CheckBox> _wizardDeviceChecks = new();
    // Per seat, the three place-flag boxes in PlaceFlagBoxes order (folders re-model
    // phase 2 slice e — these replaced the per-device role picker).
    private readonly List<CheckBox[]> _wizardDeviceFlagBoxes = new();
    // Set true while a render mutates wizard controls so their change events don't feed
    // back into the machine (which would tick the observer → re-render).
    private bool _suppressWizardEvents;

    /// <summary>
    /// Which of a seat's three flags a wizard checkbox owns. The flag MEANINGS live
    /// once, in <c>fauna_protocol::folders::PlaceFlags</c>; this only picks the field
    /// (mirrors linux's <c>PlaceFlagKind</c>).
    /// </summary>
    private enum PlaceFlagKind { Originates, Accepts, AppliesDeletes }

    /// <summary>
    /// The three place-flag checkboxes, in canonical order: element id, the i18n key of
    /// the label, the i18n key of its one-line explainer, and which flag it reads. A
    /// live user called the retired Source/Sync/Backup/Mirror role labels "a completely
    /// incomprehensible list of things" (2026-08-05); the design's answer is three boxes
    /// that each say what they do, each with its own explainer.
    /// </summary>
    private static readonly (string Id, string LabelKey, string DescKey, PlaceFlagKind Kind)[] PlaceFlagBoxes =
    {
        ("wizard-device-originates", "devices/wizard/place_originates", "devices/wizard/place_originates_desc", PlaceFlagKind.Originates),
        ("wizard-device-accepts", "devices/wizard/place_accepts", "devices/wizard/place_accepts_desc", PlaceFlagKind.Accepts),
        ("wizard-device-applies-deletes", "devices/wizard/place_applies_deletes", "devices/wizard/place_applies_deletes_desc", PlaceFlagKind.AppliesDeletes),
    };

    private static bool ReadFlag(WizardDevice d, PlaceFlagKind kind) => kind switch
    {
        PlaceFlagKind.Originates => d.originates,
        PlaceFlagKind.Accepts => d.accepts,
        _ => d.appliesDeletes,
    };

    private FolderWizardMachine? Wiz => _machine?.Wizard();

    public FoldersPage()
    {
        this.InitializeComponent();
        FolderDeleteConfirmBtn.Content = S.Get("devices/delete_folder");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // Fresh nav-readiness barrier for THIS navigation (fires before Page_Loaded;
        // robust to a reused/cached page instance). Page_Loaded completes it.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _convSession = clients.ConvSession;
            _nestHttp = clients.Nest;
            _account = clients.Account;
            _crypto = clients.Crypto;
            // Per-set device-activity live refresh (fauna.sync.changed). This page
            // has no NavigationCacheMode (a fresh instance per navigation), so the
            // subscription is torn down symmetrically in OnNavigatedFrom below —
            // otherwise it would leak onto _rpc, which outlives the page.
            //
            // ALSO subscribe to Reconnected (closed 2026-09-06, transport.md §
            // Which surfaces a push invalidates — the windows-leg audit): the
            // per-set device-activity roster used to sit entirely outside the
            // reconnect sweep, so a dropped fauna.sync.changed push across a
            // socket gap stayed unrecovered until the row was collapsed and
            // re-expanded.
            if (_rpc is not null)
            {
                _rpc.FolderChangedPushed += OnFolderChangedPushed;
                _rpc.Reconnected += OnReconnected;
            }
        }
    }

    /// <summary>Stop rendering the control plane once this page is navigated away from — the
    /// controller lives on and keeps reconciling; only the repaint is ours. Unsubscribing
    /// from the instance we stored (rather than the current one) keeps a re-login's
    /// controller swap from leaking this handler onto the new one.</summary>
    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        if (_boundController is { } controller)
        {
            controller.Changed -= OnBindingsChanged;
            _boundController = null;
        }
        if (_rpc is not null)
        {
            _rpc.FolderChangedPushed -= OnFolderChangedPushed;
            _rpc.Reconnected -= OnReconnected;
        }
        // The machine outlives this page (Core.DevicesMachineHost); only the repaint
        // is ours.
        _listening?.Dispose();
        _listening = null;
        base.OnNavigatedFrom(e);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // e2e-only one-shot slow-load injection (TestLoadDelayMs): keeps _machine null
        // for `testDelayMs` ms so the nav-readiness regression can prove an immediate
        // post-navigate add-click is honoured (pre-fix it was dropped). Consumed here.
        var testDelayMs = TestLoadDelayMs;
        TestLoadDelayMs = 0;

        // Signal the nav-readiness barrier even on the early return so the agent's
        // bounded await never hangs a navigation to a not-yet-wired page.
        if (_rpc is null) { _loadComplete.TrySetResult(); return; }
#if P2P_SHARE
        EnsureOfflineShareSection();
#endif

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            if (testDelayMs > 0) await Task.Delay(testDelayMs);
            // The two per-page-visit capability reads that gate row controls: the
            // webdav serve capability (Sync rows) and the creator's own tier list (the
            // website-enabled paywall select's options). Both are best-effort and fail SAFE —
            // `false` / EMPTY — so a failed read disables the control with its hint
            // rather than offering a gesture that cannot succeed, and never breaks the
            // rest of the page load.
            //
            // Kicked off CONCURRENTLY and awaited only after the machine build: they are
            // independent WS-RPC round trips, and serializing them AHEAD of the build
            // (as the webdav read used to be) just adds their latency to every page load
            // — widening the window in which a test/user clicks `folder-add-button`
            // before the page is ready. Overlapped, the page load costs
            // max(machine, reads) instead of their sum.
            var canServeWebdavTask = ReadCanServeWebdavAsync(_rpc);
            var ownTiersTask = ReadOwnTiersAsync(_rpc);

            // The session's ONE machine, shared with the Devices page and outliving
            // this visit (Core.DevicesMachineHost — its header says why: the followed
            // rows' last-read memory and the refresh barrier both live in it). The
            // host wires every seam once at build (the MlsQuery join-filter, the
            // foreign-set source, the followed-folder source); this page only listens
            // while it is showing — the observer ticks the UI thread → RenderPage.
            _observer = new DevicesNotifyObserver(RenderPage);
            _listening?.Dispose();
            _listening = DevicesMachineHost.Listen(_observer);
            _machine = await DevicesMachineHost.GetOrBuildAsync(_rpc, _convSession);
#if P2P_SHARE
            // The ceremony's device-local half, before any await below that
            // rides the nest (LoadGroupSharesAsync says why).
            await LoadGroupSharesAsync();
#endif
            _canServeWebdav = await canServeWebdavTask;
            _ownTiers = await ownTiersTask;
            await _machine.Refresh();

            // Recipient-side pending-share knocks (folders.md § Sharing): a PEEK,
            // fetched once per page load — re-fetched after accept/decline, never
            // pushed/polled (mirrors the e2e helper wait_for_pending_shares' own
            // "fetched on page-visible, not pushed" note). The ceremony's half
            // was read above, ahead of the nest.
            await LoadNestPendingSharesAsync();

            // The local-folder binding rides a SEPARATE seam (the user-session
            // fauna-sync helper). Build its VM + load the folder list so each set row
            // can nest its bound folders. A CollectionChanged re-render keeps the open
            // set's nested list current after a bind/unbind/mode mutation.
            // The LOGIN owns the binding control plane (optimistic rows must survive leaving
            // this page — and must exist before the agent session does), so the production VM
            // borrows it and subscribes for background repaints. The injected e2e channel is
            // a pure render fixture and gets its OWN throwaway controller, keeping the
            // login's — and so the data.sync block — untouched.
            if (TestLocationChannelOverride is { } injected)
            {
                _syncVm = new LocationsViewModel(injected, new FfiLocationBindingsModel());
            }
            else
            {
                var controller = App.CurrentLocationBindings ?? new LocationBindingsController();
                _syncVm = new LocationsViewModel(controller);
                // Page_Loaded re-runs on a re-navigated cached instance, so drop any prior
                // subscription first — a doubled handler repaints twice per reconcile.
                if (_boundController is { } previous) previous.Changed -= OnBindingsChanged;
                _boundController = controller;
                controller.Changed += OnBindingsChanged;
            }
            _syncVm.PropertyChanged += SyncVm_PropertyChanged;
            _syncVm.Locations.CollectionChanged += (_, _) => RefreshExpandedLocations();
            await _syncVm.LoadCommand.ExecuteAsync(null);

            // Sync defaults (page-level): load the persisted global default conflict
            // policy for NEW sets (fauna.state.sync-prefs) into the combo. Populated
            // + pre-selected under the suppress flag so the initial bind never echoes
            // a save.
            await LoadSyncDefaultsAsync();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
            RenderPage();
            // The initial load has rendered (success or handled error): release the
            // nav-readiness barrier. ALWAYS in finally so the agent's bounded await
            // never hangs a navigation. See IAsyncLoadedPage.
            _loadComplete.TrySetResult();
        }
    }

    /// <summary>Whether the owner can serve ANY set over WebDAV, fail-safe <c>false</c>
    /// (webdav-server.md § Implementation status → <c>6b-2 c disable-with-hint</c>).
    /// A separate method so <see cref="Page_Loaded"/> can overlap it with the machine
    /// build; <c>ConfigureAwait</c> is deliberately absent (WinUI VM/page rule).</summary>
    private static async Task<bool> ReadCanServeWebdavAsync(INestRpcClient rpc)
    {
        try { return await rpc.FoldersCanServeWebdavAsync(); }
        catch { return false; }
    }

    /// <summary>The creator's own subscription tier names for the website-enabled paywall
    /// select (folders.md § Web paywall), fail-safe EMPTY — no tiers renders the
    /// select disabled with the "create a tier first" hint. Read off the KEY-BEARING
    /// subscriptions face, not the keyless DevicesMachine snapshot.</summary>
    private static async Task<IReadOnlyList<string>> ReadOwnTiersAsync(INestRpcClient rpc)
    {
        try
        {
            var tiers = await rpc.SubscriptionTiersListAsync();
            return tiers.Select(t => t.name).ToList();
        }
        catch { return Array.Empty<string>(); }
    }

#if P2P_SHARE
    // ── Offline co-present share ceremony (p2p.md § Offline share initiation) ──
    // The section itself (its markup, its render and its four acts) lives in
    // Views/P2pShare/OfflineShareSection — the `p2p-share` member's excision
    // unit, which a store-safe build removes as an item (FaunaApp.csproj). This
    // page only hosts it and answers its two callbacks: the knock list and the
    // folder machine are this page's, and so is the error-message surface.

    /// <summary>Build and attach the section once per page instance (the page
    /// has no NavigationCacheMode, so this is once per visit).</summary>
    private void EnsureOfflineShareSection()
    {
        if (_rpc is null || _offlineShare is not null) return;
        _offlineShare = new P2pShare.OfflineShareSection();
        OfflineShareSectionHost.Content = _offlineShare;
        _offlineShare.Attach(_rpc, _crypto, OnOfflineShareActResolvedAsync, ReportOfflineShareError);
    }

    /// <summary>A ceremony act resolved: re-fetch the knock list always (the
    /// answered invitation leaves the pending list regardless of outcome), and
    /// refresh the folder machine only when the ceremony actually landed a
    /// scope (<c>OfflineShareStatusLandsAScope</c>) — a Failed ceremony has
    /// nothing new for the machine to read. Never throws: the ceremony's own
    /// half (the answered card, the landed scope row) is device-local and
    /// painted first; the rest rides the nest, which an offline ceremony may
    /// not have, so it is best-effort — its failure is the next page load's
    /// to report, never a reason to call a finished ceremony failed.</summary>
    private async Task OnOfflineShareActResolvedAsync(CeremonyStatus status)
    {
        await LoadGroupSharesAsync();
        try
        {
            await LoadNestPendingSharesAsync();
            if (_machine is not null && FaunaFfiMethods.OfflineShareStatusLandsAScope(status))
                await _machine.Refresh();
        }
        catch (Exception ex)
        {
            E2eTrace.Write($"[folders] post-ceremony refresh failed: {ex.Message}");
        }
    }

    private void ReportOfflineShareError(string message)
    {
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = ErrorBar.Message;
    }
#endif

    // ── Observer-driven render: the folder / conflict / wizard slices ──

    private void RenderPage()
    {
#if P2P_SHARE
        _offlineShare?.Render();
#endif
        if (_machine is null) return;
        var snap = _machine.Snapshot();
        // The actor's web-address opt-in, captured BEFORE the rows are built:
        // it is the second half of folder-website-toggle's tri-state hint, and a
        // row body reads it as it renders (folders.md § Audience and website
        // serving). Best-effort on the machine's side, so `null` reaches the
        // shared hint as its own hedging arm rather than as "off".
        _websiteAddressEnabled = snap.websiteAddressEnabled;
        RenderFolders(snap.folders);
        RenderFollowed(snap.followed);
        RenderConflicts(snap.conflicts);

        // Page-level error-message: the machine localizes the last read/gesture failure
        // into snapshot.error; combined with the folder-binding VM error in UpdateErrorBar.
        _machineError = snap.error is { } err ? S.Resolve(err) : null;
        UpdateErrorBar();

        RenderWizard(snap.wizard);
    }

    private void SyncVm_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(LocationsViewModel.ErrorMessage)) UpdateErrorBar();
    }

    // Surface whichever error is live on the one canonical error-message bar — the
    // machine read/gesture error takes priority over the (secondary) folder-binding
    // "helper unavailable" error.
    private void UpdateErrorBar()
    {
        var syncErr = string.IsNullOrEmpty(_syncVm?.ErrorMessage) ? null : _syncVm!.ErrorMessage;
        var msg = _machineError ?? syncErr;
        if (msg is not null)
        {
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    // ── Folders (Expander rows over FolderSummary) ──

    private void RenderFolders(IReadOnlyList<FolderSummary> folders)
    {
        // Re-open whatever row was open. A re-render rebuilds every row from
        // scratch and a row's body is realized lazily on `Expanding`, so WITHOUT
        // this any refresh snaps the user's open row shut and throws away what
        // they were looking at — including the refresh that a share or a member
        // remove performs on that very row. That is what made a landed share look
        // like it had done nothing: the nest roster had the new member, the owner's
        // "Shared with" section had ceased to exist, and re-expanding by hand was
        // the only way to see it (found driving test_folder_full_share_round_trip
        // on windows for the first time, 2026-08-10). Captured BEFORE the reset
        // below, which is what clears it.
        var reopen = _expandedFolder;
        FolderContainer.Children.Clear();
        _expandedInclude = null;
        _expandedExclude = null;
        _expandedFolder = null;
        _expandedLocationContainer = null;
        _expandedLocationEmpty = null;
        _expandedLocationInput = null;
        _expandedDeviceActivityContainer = null;
        foreach (var fs in folders)
        {
            var row = fs.role == "member" ? BuildSharedWithMeRow(fs) : BuildFolderRow(fs);
            FolderContainer.Children.Add(row);
            // Setting IsExpanded raises `Expanding`, which builds the body from the
            // summary THIS render pass carries — so the re-opened section sees the
            // set's new truth (a just-shared set's `mlsGroupId`), not the stale one
            // the pre-refresh row closed over.
            if (reopen == fs.name && row is Expander reopened)
                reopened.IsExpanded = true;
            // A folder-share route (the Explorer Share leaf on a bound folder —
            // windows.md § Shell Extension → The Share hand-off, step 4): open
            // this set's row with its folder-share-button flow revealed.
            // Consumed once; an owner row only, since only the owner shares.
            if (App.PendingFolderShare == fs.@id && fs.role != "member" && row is Expander target)
            {
                App.PendingFolderShare = null;
                _openShareFor = fs.name;
                target.IsExpanded = true;
            }
        }
        // Landed group scopes (offline co-present ceremony, p2p.md § Offline
        // share initiation): appended AFTER the M2 rows, continuing the same
        // folder-row id family — a ceremony-born set is a set. No new ids, no
        // leave button (severance is the authority's mint, not this device's),
        // no name (v1 sets are nameless — the row shows the short scope id).
        var landedScopes = 0;
#if P2P_SHARE
        foreach (var scope in _groupShareViews.scopes)
            FolderContainer.Children.Add(BuildGroupScopeRow(scope));
        landedScopes = _groupShareViews.scopes.Length;
#endif
        EmptyText.Visibility = folders.Count == 0 && landedScopes == 0
            ? Visibility.Visible : Visibility.Collapsed;
        AppDataSnapshot.SetSyncFiles(folders.Select(f =>
            new AppDataSnapshot.SyncFileSnapshot(f.name, f.name, "")));
    }

#if P2P_SHARE
    /// <summary>A landed group scope, read from the group plane's store
    /// (<c>FfiGroupShareViews.scopes</c>) rather than the ceremony record — a
    /// recorded-but-never-adopted scope is silently absent, never painted as a
    /// set the device cannot read (p2p.md § Offline share initiation → *Built —
    /// the affordance, both roles*). Badged with who shared it when this device
    /// did not mint the scope (<c>sharedBy</c> set); the initiator's own
    /// minted scope carries no badge, mirroring the ordinary owner row above.</summary>
    private FrameworkElement BuildGroupScopeRow(FfiGroupScope scope)
    {
        var row = new Grid { Padding = new Thickness(8), ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, scope.shortId);

        var name = new TextBlock
        {
            Text = scope.shortId,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Grid.SetColumn(name, 0);

        var badge = new TextBlock
        {
            Text = scope.sharedBy is { Length: > 0 } who ? S.Get("devices/shared_by").Replace("{who}", who) : string.Empty,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(badge, Ids.FolderSharedBadge);
        Grid.SetColumn(badge, 1);

        row.Children.Add(name);
        row.Children.Add(badge);
        return row;
    }
#endif

    private Expander BuildFolderRow(FolderSummary fs)
    {
        var exp = new Expander
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Header = $"{fs.name}   ({ValueFormat.ByteSize((ulong)Math.Max(0, fs.cachedTotalBytes))})",
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(exp, Ids.FolderRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(exp, fs.name);
        // Lazy body: realize only while expanded so the unindexed body IDs stay unique
        // to the single open row (mirrors Linux's adw::ExpanderRow).
        exp.Expanding += (_, _) =>
        {
            exp.Content = BuildFolderBody(fs);
            // A long folder list can push a newly-expanded row's (now taller,
            // with the Sharing section) body below the page's ScrollViewer
            // viewport — is_visible fails on an offscreen-but-realized control
            // even though it's logically Visible (reference_windows_e2e_is_
            // visible_offscreen; mirrors SettingsAccountPage / SettingsPrivacyPage's
            // StartBringIntoView after revealing a control). Calling
            // StartBringIntoView() synchronously here is a no-op: the Expander's
            // expand transition + the new body's measure/arrange pass haven't run
            // yet, so it still sees the pre-expand (collapsed) bounding rect. Defer
            // with a one-shot LayoutUpdated (mirrors EventsPage.WrapInTimeScroll's
            // deferred-scroll idiom): it fires repeatedly (cheap no-op) until the
            // expanded body has actually measured (non-zero ActualHeight), then
            // scrolls once and unsubscribes — no fixed delay, no race window.
            void OnLayout(object? s, object le)
            {
                if (exp.Content is not FrameworkElement content || content.ActualHeight <= 0)
                    return;
                exp.StartBringIntoView();
                exp.LayoutUpdated -= OnLayout;
            }
            exp.LayoutUpdated += OnLayout;
        };
        exp.Collapsed += (_, _) =>
        {
            exp.Content = null;
            if (_expandedFolder == fs.name)
            {
                _expandedInclude = null;
                _expandedExclude = null;
                _expandedFolder = null;
                _expandedLocationContainer = null;
                _expandedLocationEmpty = null;
                _expandedLocationInput = null;
                _expandedDeviceActivityContainer = null;
            }
        };
        return exp;
    }

    // ── Cross-user sharing (recipient side) — folders.md § Sharing ──
    //
    // A row for a set shared *with* this client (B3 member-list-visibility): the
    // shared machine's centralized MlsQuery join-filter (wired in Page_Loaded) only
    // lets a role == "member" row reach this snapshot once this client has actually
    // MLS-joined the group — a rostered-but-un-joined knock never reaches here, it
    // surfaces only as folder-pending-share (the page-level "Shared with you"
    // section, below). A READER row (access absent/"reader") is flat — NONE of the
    // owner affordances (share / member roster / remove / delete / path editors) and
    // no binding, since a bound folder whose edits could not upload would breach
    // file-sync.md's iron rule (mirrors linux's plain adw::ActionRow,
    // build_member_folder_row). A WRITER row (multi-writer Phase 1, access ==
    // "writer") is an Expander carrying the SAME folder-location-* binding widget an
    // owner row uses — the writer's only management affordance (mirrors linux's
    // adw::ExpanderRow, build_writer_member_folder_row).
    //
    // Which of the two is the shared `binding_section` decision's, never the access
    // alone (file-sync.md § Multi-writer shared sets → Revocation): a demotion is exactly
    // what turns the row's access to "reader", and a row keyed on the access hid the
    // demoted writer's PARKED binding — and the warning about it — on the first refresh
    // after the demotion. A parked binding keeps the expandable row whatever the access
    // now reads (macOS FoldersContent's member branch; tui/linux the same call).

    private FrameworkElement BuildSharedWithMeRow(FolderSummary fs)
        => MemberBindingSection(fs).@shown ? BuildWriterMemberFolderRow(fs) : BuildReaderMemberFolderRow(fs);

    /// <summary>The shared decision for one member row: <c>parked</c> is whether any of
    /// this device's bindings to the set carries the agent's park (folded on the status
    /// poll, <c>LocationBindingsController.PollParksAsync</c>).</summary>
    private BindingSection MemberBindingSection(FolderSummary fs)
        => uniffi.fauna_ffi.FaunaFfiMethods.BindingSection(fs.role, fs.access, ParkedFolders().Contains(fs.name));

    /// <summary>The sets any of this device's bindings to is parked.</summary>
    private HashSet<string> ParkedFolders()
        => _syncVm is null
            ? new HashSet<string>()
            : _syncVm.Locations.Where(r => r.AccessRevoked).Select(r => r.Folder).ToHashSet();

    /// <summary>The name + "Shared by ‹owner›" badge + Leave button every
    /// shared-with-me row shows, reader or writer alike — three children on a
    /// 3-column Grid (two <c>*</c> that share the remainder and trim, one
    /// <c>Auto</c> that always fits the Leave button; a StackPanel would instead
    /// give the two unbounded user-controlled text labels all the width they ask
    /// for, pushing Leave past the row's clip edge — found driving
    /// test_folder_pending_share_accept_decline on windows for the first time,
    /// 2026-08-10). The caller owns the outer <c>folder-row</c> AutomationId +
    /// Name — a bare Grid for the reader row, the enclosing Expander for the
    /// writer row.</summary>
    private Grid BuildMemberHeaderGrid(FolderSummary fs)
    {
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var name = new TextBlock
        {
            Text = fs.name,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Grid.SetColumn(name, 0);

        // "Shared by ‹…›" — the nest-precomputed FolderSummary.ownerDisplay (handle else
        // short_id; never a bare ellipsis or client-side truncation — folders.md §
        // Sharing, folder-shared-badge). Same folder-shared-badge id the owner side uses
        // for its "Shared · N" badge — one id, different meaning by context.
        var badge = new TextBlock
        {
            Text = S.Get("devices/shared_by").Replace("{who}", fs.ownerDisplay),
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(badge, Ids.FolderSharedBadge);
        Grid.SetColumn(badge, 1);

        var leave = new Button { Content = S.Get("common/leave") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(leave, Ids.FolderLeaveButton);
        Grid.SetColumn(leave, 2);
        // Always enabled (reference_windows_disabled_button_no_invoke — a bound
        // IsEnabled would make an edge case ElementNotEnabled to FlaUI); the
        // theoretically-impossible missing-mlsGroupId case is guarded inside the
        // handler instead, the same guard shape RemoveSharedMemberAsync (below) uses.
        leave.Click += async (_, _) => await LeaveSharedFolderAsync(fs);

        row.Children.Add(name);
        row.Children.Add(badge);
        row.Children.Add(leave);
        return row;
    }

    private FrameworkElement BuildReaderMemberFolderRow(FolderSummary fs)
    {
        var row = BuildMemberHeaderGrid(fs);
        row.Padding = new Thickness(8);
        // Keeps the row in the UIA content view so FlaUI's ByAutomationId resolves the
        // nested child ids (reference_winui_flaui_datatemplate_name — mirrors the owner
        // row's SetName(exp, fs.name) above).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, fs.name);
        return row;
    }

    /// <summary>An Expander for a shared-with-me row whose binding section shows — a
    /// WRITER-access row (multi-writer Phase 1), or a demoted one whose binding is parked
    /// (<see cref="MemberBindingSection"/>) — same header as the reader row, but expanding reveals the local
    /// folder-binding widget (<see cref="BuildWriterMemberBody"/>), the writer's only
    /// management affordance. Mirrors <see cref="BuildFolderRow"/>'s lazy-body +
    /// deferred-scroll + Collapsed-reset shape exactly, over a lighter body.</summary>
    private Expander BuildWriterMemberFolderRow(FolderSummary fs)
    {
        var exp = new Expander
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Header = BuildMemberHeaderGrid(fs),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(exp, Ids.FolderRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(exp, fs.name);
        exp.Expanding += (_, _) =>
        {
            exp.Content = BuildWriterMemberBody(fs);
            // Same deferred-scroll idiom as BuildFolderRow's Expanding handler
            // (reference_windows_e2e_is_visible_offscreen) — the expand transition +
            // the new body's measure/arrange pass haven't run yet at this point.
            void OnLayout(object? s, object le)
            {
                if (exp.Content is not FrameworkElement content || content.ActualHeight <= 0)
                    return;
                exp.StartBringIntoView();
                exp.LayoutUpdated -= OnLayout;
            }
            exp.LayoutUpdated += OnLayout;
        };
        exp.Collapsed += (_, _) =>
        {
            exp.Content = null;
            if (_expandedFolder == fs.name)
            {
                _expandedFolder = null;
                _expandedLocationContainer = null;
                _expandedLocationEmpty = null;
                _expandedLocationInput = null;
            }
        };
        return exp;
    }

    /// <summary>The writer-member row's expanded body — ONLY the local-folder
    /// binding (+ the D4 park warning above it), never the owner-only paths /
    /// share / roster / delete / webdav / paywall body <see cref="BuildFolderBody"/>
    /// renders: a writer contributes content, they do not manage the set.
    /// <para>A cross-nest (foreign) row — <c>fs.homeNestUrl is not null</c> — has no
    /// foreign engine legs on windows yet (only linux does today), so the binding is
    /// WITHHELD entirely: offering a bind the engine cannot honour would seal the
    /// user's edits under their own BackupKey instead of the set's real content key
    /// (file-sync.md § Multi-writer shared sets) — the silent-wrong-key class, not a
    /// fail-closed one. Same withhold macOS/tui apply.</para></summary>
    private FrameworkElement BuildWriterMemberBody(FolderSummary fs)
    {
        // Set the expanded-set context first (mirrors BuildFolderBody) so the
        // nested folder section — and the Collapsed reset above — filter to it,
        // even on the withheld (foreign) branch below.
        _expandedFolder = fs.name;

        var panel = new StackPanel { Spacing = 12, Padding = new Thickness(8, 8, 8, 8) };
        if (fs.homeNestUrl is not null) return panel;

        // `folder-access-revoked-warning` (D4) — the owner withdrew this actor's
        // write grant mid-life, the authoritative nest refused the next mint/record,
        // and the agent parked every folder bound to the set. Rendered ABOVE the
        // binding widget; the binding rows themselves stay visible and removable —
        // a park is not a deletion.
        if (MemberBindingSection(fs).@revokedWarning)
        {
            var revoked = new TextBlock
            {
                Text = S.Get("devices/access_revoked_warning"),
                Opacity = 0.8,
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(revoked, Ids.FolderAccessRevokedWarning);
            panel.Children.Add(revoked);
        }

        panel.Children.Add(BuildLocationBindingSection(fs));
        return panel;
    }

    /// <summary>
    /// <c>folder-leave-button</c>: voluntarily leave a set shared *with* the caller
    /// (<c>FaunaFfiMethods.FoldersLeave</c> — the self-scoped nest roster-drop +
    /// local <c>MlsEngine::forget_group</c>). Unlike the owner-side remove, this does
    /// NOT rotate the owner's content key ("forward secrecy from yourself is not a
    /// threat", folders.md § Sharing). On success: refresh the page (the row drops out of
    /// the list).
    /// </summary>
    private async Task LeaveSharedFolderAsync(FolderSummary fs)
    {
        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_leave_share", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // Shouldn't happen — the shared machine's MlsQuery join-filter only lets an
        // already-MLS-joined member row (which always carries mls_group_id) reach this
        // snapshot; guard defensively anyway (mirrors RemoveSharedMemberAsync below).
        if (fs.mlsGroupId is not { } groupIdHex) return;
        // Captured into locals so the non-null state survives the await below.
        var session = _convSession;
        var rpc = _rpc;

        try
        {
            await rpc.FoldersLeaveAsync(session, groupIdHex);
            if (_machine is not null) await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    // ── Cross-user sharing (recipient side) — pending-share knocks ──
    //
    // The page-level "Shared with you" section (PendingSharesContainer, XAML): a
    // stranger's (non-contact) share stages the MLS Welcome UNPROCESSED — never
    // auto-joined — until the user explicitly accepts or declines it (folders.md
    // § Sharing). Distinct from BuildSharedWithMeRow above, which only renders an
    // ALREADY-joined shared set. Listing + declining need only the nest RPC seam;
    // only accepting needs _convSession (it performs the actual MLS join).

    /// <summary>
    /// Fetch + render the recipient's staged <c>folder-pending-share</c> knocks
    /// (<c>FoldersPendingSharesAsync</c> — a PEEK, never acks a row). Called once
    /// per page load and again after a successful accept/decline (the acted-on row
    /// should disappear). No <c>_convSession</c> guard — listing needs only the
    /// nest RPC seam.
    /// </summary>
    private async Task LoadPendingSharesAsync()
    {
        if (_rpc is null) return;
#if P2P_SHARE
        await LoadGroupSharesAsync();
#endif
        await LoadNestPendingSharesAsync();
    }

    /// <summary>The M2 half of the knock list: a nest round trip, so it can
    /// fail or wait on a reconnect while the nest is unreachable — which is
    /// why the ceremony's half never waits behind it.</summary>
    private async Task LoadNestPendingSharesAsync()
    {
        if (_rpc is null) return;
        _pendingShares = await _rpc.FoldersPendingSharesAsync();
        RenderPendingShares(_pendingShares);
    }

#if P2P_SHARE
    /// <summary>The offline ceremony's half of the knock list, plus the landed
    /// group scopes: read off this device's own account store and ceremony
    /// seat, so it answers with the nest unreachable — the co-present
    /// ceremony's whole case (p2p.md § Offline share initiation: "both knock
    /// lists are fetched on page-visible rather than pushed", and the ceremony
    /// "runs with no nest involved at all"). Read and painted on its own,
    /// AHEAD of every nest round trip on the page: sequenced behind the M2
    /// peek or the machine refresh, a nest that is down kept the consent card
    /// and the landed set from ever painting. Fail-safe empty, so an
    /// unavailable ceremony plane never breaks the M2 list it shares a
    /// container with.</summary>
    private async Task LoadGroupSharesAsync()
    {
        if (_rpc is null) return;
        try { _groupShareViews = await _rpc.OfflineShareLoadGroupSharesAsync(App.CurrentOfflineShareSeat); }
        catch { _groupShareViews = new FfiGroupShareViews(Array.Empty<FfiPendingGroupShare>(), Array.Empty<FfiGroupScope>()); }
        RenderPendingShares(_pendingShares);
        if (_machine is not null) RenderFolders(_machine.Snapshot().folders);
    }
#endif

    private void RenderPendingShares(IReadOnlyList<uniffi.fauna_ffi.FfiPendingShare> shares)
    {
        PendingSharesContainer.Children.Clear();
        foreach (var share in shares)
            PendingSharesContainer.Children.Add(BuildPendingShareRow(share));
        // Group-share invitations: ONE indexed family with the M2 rows above —
        // the consent card mints no ids of its own (p2p.md § Offline share
        // initiation → *Built — the affordance, both roles*).
#if P2P_SHARE
        foreach (var invite in _groupShareViews.invitations)
            PendingSharesContainer.Children.Add(BuildGroupInvitationRow(invite));
#endif
    }

    /// <summary>
    /// One <c>folder-pending-share</c> row: the sharer identity ("Shared by
    /// ‹who›") plus accept/decline. <c>who</c> renders the shared-Rust-computed
    /// <c>FfiPendingShare.sharedByDisplay</c> verbatim — never re-derived locally
    /// (value-formatting.md § Account display label; replaces the former
    /// per-app 12-hex-char+"…" truncation, one of three drifted variants
    /// across clients) — falling back to the shared <c>common/unknown</c> label
    /// only when the share is fully unstamped (both <c>sharedBy</c> and
    /// <c>sharedByHandle</c> absent). Mirrors linux's
    /// <c>build_pending_share_row</c> (folders.rs:1018+).
    /// </summary>
    private FrameworkElement BuildPendingShareRow(uniffi.fauna_ffi.FfiPendingShare share)
    {
        // A Grid, NOT a horizontal StackPanel: the sharer label is user-controlled
        // text of unbounded length, and a StackPanel gives it all the width it asks
        // for — which pushes the buttons past the row's clip edge. The Decline button
        // (the LAST child) went first: present in the UIA tree with its label, but
        // `IsOffscreen`, i.e. genuinely unreachable for a user on a narrow window and
        // reported by the harness as `visible=False, count=1` — a real defect that
        // reads exactly like an unbuilt feature. Sizing the two buttons `Auto` and
        // letting the label absorb only the REMAINDER (with trimming) makes both
        // gestures reachable at any handle length. Found driving
        // test_folder_pending_share_accept_decline on windows for the first time,
        // 2026-08-10.
        var row = new Grid { Padding = new Thickness(8), ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderPendingShare);

        var who = share.sharedByDisplay is { Length: > 0 }
            ? share.sharedByDisplay
            : S.Get("common/unknown");
        // Keeps the row in the UIA content view so FlaUI's ByAutomationId resolves
        // the nested child ids (reference_winui_flaui_datatemplate_name — the same
        // trick every other indexed container row on this page uses).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, who);

        var badge = new TextBlock
        {
            Text = S.Get("devices/shared_by").Replace("{who}", who),
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Grid.SetColumn(badge, 0);

        var accept = new Button { Content = S.Get("common/accept") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(accept, Ids.FolderShareAcceptButton);
        accept.Click += async (_, _) => await AcceptPendingShareAsync(share);
        Grid.SetColumn(accept, 1);

        var decline = new Button { Content = S.Get("common/decline") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(decline, Ids.FolderShareDeclineButton);
        decline.Click += async (_, _) => await DeclinePendingShareAsync(share);
        Grid.SetColumn(decline, 2);

        row.Children.Add(badge);
        row.Children.Add(accept);
        row.Children.Add(decline);
        return row;
    }

    /// <summary>
    /// <c>folder-share-accept-button</c>: accept a staged share
    /// (<c>FoldersAcceptShareAsync</c>) — resolves the Welcome by inbox id,
    /// MLS-joins the group off the chat rail, then acks the durable row. Requires
    /// <c>_convSession</c> (the join needs the live <c>MlsEngine</c>) — unlike
    /// decline, which does not. On success: re-fetch pending shares (the accepted
    /// row disappears) and refresh the folder machine (the newly-joined set now
    /// appears as a shared-with-me row, since <c>has_group</c> now reports true
    /// for it).
    /// </summary>
    private async Task AcceptPendingShareAsync(uniffi.fauna_ffi.FfiPendingShare share)
    {
        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_accept_share", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // Captured into locals so the non-null state survives the awaits below.
        var session = _convSession;
        var rpc = _rpc;

        try
        {
            await rpc.FoldersAcceptShareAsync(session, share.inboxId);
            await LoadPendingSharesAsync();
            if (_machine is not null) await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    /// <summary>
    /// <c>folder-share-decline-button</c>: decline a staged share
    /// (<c>FoldersDeclineShareAsync</c>) — a bare ack of the durable row; the
    /// Welcome is dropped unprocessed, so declining never joins the group. No
    /// <c>_convSession</c> guard (declining needs only the nest RPC seam). On
    /// success: re-fetch pending shares only — nothing joined, so no folder
    /// row appears and no machine refresh is needed.
    /// </summary>
    private async Task DeclinePendingShareAsync(uniffi.fauna_ffi.FfiPendingShare share)
    {
        if (_rpc is null) return;
        try
        {
            await _rpc.FoldersDeclineShareAsync(share.inboxId);
            await LoadPendingSharesAsync();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = S.Format("devices/error_decline_share", ex.Message);
            ErrorBar.IsOpen = true;
        }
    }

#if P2P_SHARE
    /// <summary>One <c>folder-pending-share</c> row for a group-share
    /// invitation (offline co-present ceremony) — the SAME card the M2 knocks
    /// above use, over a second source (p2p.md § Offline share initiation →
    /// *Built — the affordance, both roles*: "the consent card mints NO ids").
    /// <paramref name="invite"/>.initiator is already the canonical short-id
    /// display form, so it renders through the same <c>devices/shared_by</c>
    /// template the M2 row and the landed-scope badge both use.</summary>
    private FrameworkElement BuildGroupInvitationRow(FfiPendingGroupShare invite)
    {
        var row = new Grid { Padding = new Thickness(8), ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderPendingShare);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, invite.initiator);

        var badge = new TextBlock
        {
            Text = S.Get("devices/shared_by").Replace("{who}", invite.initiator),
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            TextWrapping = TextWrapping.NoWrap,
        };
        Grid.SetColumn(badge, 0);

        var accept = new Button { Content = S.Get("common/accept") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(accept, Ids.FolderShareAcceptButton);
        accept.Click += async (_, _) => await ConsentGroupShareAsync(invite);
        Grid.SetColumn(accept, 1);

        var decline = new Button { Content = S.Get("common/decline") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(decline, Ids.FolderShareDeclineButton);
        decline.Click += async (_, _) => await DeclineGroupShareAsync(invite);
        Grid.SetColumn(decline, 2);

        row.Children.Add(badge);
        row.Children.Add(accept);
        row.Children.Add(decline);
        return row;
    }

    /// <summary><c>folder-share-accept-button</c> on a group-share invitation:
    /// one atomic act (<c>OfflineShareConsentAsync</c>) — mints and rests the
    /// reception keypair, records the accept, awaits the deliver, runs
    /// admission, writes the machinery through (p2p.md § Offline share
    /// initiation → *Built — the affordance, both roles*). Requires an
    /// already-bound seat: the invitation was received on it, so a missing
    /// seat (e.g. a cold app restart since the offer arrived) is a no-op
    /// rather than a crash. On success: the same resolution the section's own
    /// begin act takes (<see cref="OnOfflineShareActResolvedAsync"/>) — the
    /// answered row disappears regardless of outcome, and the folder machine
    /// refreshes only when the ceremony landed a scope.</summary>
    private async Task ConsentGroupShareAsync(FfiPendingGroupShare invite)
    {
        if (_rpc is null || App.CurrentOfflineShareSeat is not { } seat) return;
        try
        {
            App.CurrentOfflineShareStatus = await _rpc.OfflineShareConsentAsync(seat, invite.scopeId);
            await OnOfflineShareActResolvedAsync(App.CurrentOfflineShareStatus);
        }
        catch (Exception ex)
        {
            ReportOfflineShareError(S.Format("folders/error_offline_share", ex.Message));
        }
    }

    /// <summary><c>folder-share-decline-button</c> on a group-share
    /// invitation: a bare ack (<c>OfflineShareDeclineAsync</c>) — nothing is
    /// adopted. Same already-bound-seat requirement as consent above. On
    /// success: re-fetch the knock list only.</summary>
    private async Task DeclineGroupShareAsync(FfiPendingGroupShare invite)
    {
        if (_rpc is null || App.CurrentOfflineShareSeat is not { } seat) return;
        try
        {
            await _rpc.OfflineShareDeclineAsync(seat, invite.scopeId);
            await LoadPendingSharesAsync();
        }
        catch (Exception ex)
        {
            ReportOfflineShareError(S.Format("folders/error_offline_share", ex.Message));
        }
    }
#endif

    private FrameworkElement BuildFolderBody(FolderSummary fs)
    {
        // Set the expanded-set context first so the nested folder section filters to it.
        _expandedFolder = fs.name;

        var panel = new StackPanel { Spacing = 12, Padding = new Thickness(8, 8, 8, 8) };

        var include = new TextBox
        {
            PlaceholderText = S.Get("devices/include_paths"),
            Text = uniffi.fauna_ffi.FaunaFfiMethods.JoinPathsField(fs.includePaths),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(include, Ids.FolderIncludePaths);

        var exclude = new TextBox
        {
            PlaceholderText = S.Get("devices/exclude_paths"),
            Text = uniffi.fauna_ffi.FaunaFfiMethods.JoinPathsField(fs.excludePaths),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(exclude, Ids.FolderExcludePaths);

        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        var save = new Button { Content = S.Get("devices/save_paths"), Tag = fs.name };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(save, Ids.FolderSavePaths);
        save.Click += SavePaths_Click;
        var delete = new Button { Content = S.Get("devices/delete_folder"), Tag = fs.name };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(delete, Ids.FolderDeleteButton);
        delete.Click += DeleteFolder_Click;
        actions.Children.Add(save);
        actions.Children.Add(delete);

        panel.Children.Add(include);
        panel.Children.Add(exclude);
        // (The per-set scan-frequency row that used to sit here retired with
        // folders re-model phase 5: the cadence is a constant, not a choice —
        // file-sync.md § Config, the phase-5 block.)
        // Per-set conflict policy — every owner row (a folder has no type;
        // ui/folders.md § Conflicts).
        panel.Children.Add(BuildConflictPolicyRow(fs));
        // Content residency — every mode, applies on change like the conflict
        // policy above (file-sync.md § Content residency; windows the last of
        // 7 apps).
        panel.Children.Add(BuildResidencyRow(fs));
        // Audience + website serving — EVERY mode, on every expanded owner row
        // (folders.md § Audience and website serving). The audience is the
        // folder's identity, not a per-mode serving option, and the website
        // toggle is the only door to a website folder since the wizard's mode
        // step retired.
        panel.Children.Add(BuildAudienceRow(fs));
        panel.Children.Add(BuildWebsiteToggleRow(fs));
        // Every owner row can be served over WebDAV — a folder has no type
        // (webdav-server.md § What the WebDAV namespace is).
        panel.Children.Add(BuildWebdavToggleRow(fs));
        // Website-enabled sets can be paywalled to a subscription tier — keyed on the
        // website toggle, never on the retired `mode = "web"` spelling; the structural
        // sibling of the webdav toggle above (folders.md § Web paywall).
        if (fs.websiteEnabled) panel.Children.Add(BuildPaywallTierRow(fs));
        panel.Children.Add(BuildDeviceActivitySection(fs));
        // The post-create device-place editor (folders.md § Implementation
        // status today — the place editor). Each enrolled seat's place, edited
        // in place: the wizard sets the flags only at creation, so this is the
        // only door to an existing seat's point.
        panel.Children.Add(BuildPlaceEditorSection(fs));
        panel.Children.Add(BuildSharingSection(fs));
        // The nest place's snapshot policy — on EVERY folder, not just a backup-type
        // one (backup-restore.md § 8b; what replaced the wizard's snapshot-only
        // retention step). Sits where linux puts it: after the sections, before the
        // action buttons.
        panel.Children.Add(BuildNestPlaceSection(fs));
        panel.Children.Add(actions);
        panel.Children.Add(BuildLocationBindingSection(fs));
        panel.Children.Add(BuildDestinationPlacesSection(fs));

        _expandedInclude = include;
        _expandedExclude = exclude;
        return panel;
    }

    // Per-set conflict policy (folder-conflict-policy-select) — file-sync.md §
    // Conflicts, policy. A value/label ComboBox split (the e2e select contract):
    // each item displays the friendly label but carries the WIRE VALUE ("auto" /
    // "latest_wins_always") as its UIA Name, so driver.select(id, value) matches
    // by Name and get_text reads the stable value back. The resolving device
    // reads the policy off the authoritative nest row. Options come from the
    // shared catalog (folders.md § Where logic lives), not a local list.

    private FrameworkElement BuildConflictPolicyRow(FolderSummary fs)
    {
        var picker = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        picker.Children.Add(new TextBlock
        {
            Text = S.Get("devices/conflict_policy"),
            VerticalAlignment = VerticalAlignment.Center,
        });

        var combo = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(combo, Ids.FolderConflictPolicySelect);
        // Absent (a member row) renders as the column default, auto.
        var current = fs.conflictPolicy ?? "auto";
        int selectedIndex = -1;
        int i = 0;
        foreach (var opt in uniffi.fauna_ffi.FaunaFfiMethods.ConflictPolicyOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            combo.Items.Add(item);
            if (opt.value == current) selectedIndex = i;
            i++;
        }
        // Pre-select BEFORE attaching the handler (the render-echo guard: the
        // initial bind must not fire a spurious write).
        if (selectedIndex >= 0) combo.SelectedIndex = selectedIndex;
        combo.Tag = fs.name;
        combo.SelectionChanged += ConflictPolicyChanged;
        picker.Children.Add(combo);
        return picker;
    }

    private async void ConflictPolicyChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_machine is null || sender is not ComboBox c) return;
        if (c.Tag is not string setName) return;
        if (c.SelectedItem is not ComboBoxItem item || item.Tag is not string value) return;
        // Selective fauna.folders.update via the shared gesture; refresh follows
        // from the machine tick.
        await _machine.SetFolderConflictPolicy(setName, value);
    }

    // Content residency (folder-nest-residency-select) — file-sync.md §
    // Content residency. Same value/label ComboBox split as the conflict
    // select above: each item displays the friendly label but carries the WIRE
    // VALUE ("full" / "metadata_only") as its UIA Name. Applies ON CHANGE, its
    // own fauna.folders.update field, deliberately OUTSIDE the batched
    // nest-place save (so an older writer's policy edit can never silently
    // clear it) — every mode, not gated on Sync like the conflict select.
    //
    // The flip to Metadata-only is consent-gated (it deletes the nest's copy
    // of the folder's content): picking it must ARM folder-residency-confirm
    // rather than commit. A WinUI ComboBox commits its pick immediately —
    // unlike apple's Picker, which repaints off the live snapshot and needs no
    // manual revert — so the arm branch snaps the selection back to the
    // current residency FIRST, mirroring the fix linux's own leg needed
    // (file-sync.md § Content residency, the 2026-08-27 addendum): without
    // this the row paints Metadata-only while the confirm sits unanswered and
    // the nest has not moved, reporting a residency the folder does not have.

    private const string ResidencyMetadataOnly = "metadata_only";

    private FrameworkElement BuildResidencyRow(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };
        var picker = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        picker.Children.Add(new TextBlock
        {
            Text = S.Get("devices/folder_residency"),
            VerticalAlignment = VerticalAlignment.Center,
        });

        var combo = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(combo, Ids.FolderNestResidencySelect);
        var current = uniffi.fauna_ffi.FaunaFfiMethods.NormalizeResidency(fs.residency);
        int selectedIndex = -1;
        int i = 0;
        foreach (var opt in uniffi.fauna_ffi.FaunaFfiMethods.ResidencyOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            combo.Items.Add(item);
            if (opt.value == current) selectedIndex = i;
            i++;
        }
        // Pre-select BEFORE attaching the handler (the render-echo guard).
        if (selectedIndex >= 0) combo.SelectedIndex = selectedIndex;
        var name = fs.name;
        combo.SelectionChanged += async (sender, _) => await ResidencyChanged(sender, name, current);
        picker.Children.Add(combo);
        section.Children.Add(picker);

        section.Children.Add(new TextBlock
        {
            Text = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ResidencyHint(current)),
            Opacity = 0.6,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
        });
        return section;
    }

    private async Task ResidencyChanged(object sender, string setName, string currentResidency)
    {
        if (_machine is null || sender is not ComboBox c) return;
        if (c.SelectedItem is not ComboBoxItem item || item.Tag is not string value) return;
        // Re-selecting the folder's current residency is a no-op, never a
        // write — and it is what terminates the snap-back below, which
        // re-enters this handler with value == currentResidency.
        if (value == currentResidency) return;

        if (value == ResidencyMetadataOnly)
        {
            // ⚠ Snap the select back to the CURRENT residency FIRST, then arm
            // — see the section comment above.
            for (int i = 0; i < c.Items.Count; i++)
            {
                if (c.Items[i] is ComboBoxItem ci && ci.Tag is string v && v == currentResidency)
                {
                    c.SelectedIndex = i;
                    break;
                }
            }
            FolderResidencyDialog.XamlRoot = this.XamlRoot;
            await Controls.Dialogs.ShowAsync(FolderResidencyDialog,
                prepare: () => _pendingResidencyFolder = setName);
            return;
        }

        // Selective fauna.folders.update via the shared gesture; refresh
        // follows from the machine tick.
        await _machine.SetFolderResidency(setName, value);
    }

    private async void FolderResidencyConfirm_Click(object sender, RoutedEventArgs e)
    {
        FolderResidencyDialog.Hide();
        if (_machine is not null && _pendingResidencyFolder is { } name)
            await _machine.SetFolderResidency(name, ResidencyMetadataOnly);
        _pendingResidencyFolder = null;
    }

    // ── Audience + website serving (folders re-model phase 4 slice 4d) ──
    // folders.md § Audience and website serving; behavior authority
    // behavior/folders.md § Target re-model. Both render on the expanded OWNER
    // row of EVERY mode: the audience is the folder's identity, not a per-mode
    // serving option.
    //
    // Everything that decides anything here is the shared machine, reached over
    // UniFFI — the option set (audience_options), the rendered value
    // (normalize_audience), the labels (audience_label) and both hints
    // (audience_hint, website_serve_hint). windows derives none of it.
    //
    // The select is the same value/label ComboBox split as the conflict-policy
    // and residency pickers above: each item displays the localized label but
    // carries the WIRE VALUE as its UIA Name, so driver.select(id, value)
    // matches by Name and get_text reads the stable value back.

    private const string AudiencePublic = "public";

    // The actor's own web-address opt-in, as of the last snapshot — the
    // best-effort second half of the website hint's tri-state. `null` is a REAL
    // arm (unwired adapter, failed read) and hedges; it must never
    // claim the site is live.
    private bool? _websiteAddressEnabled;

    // The folder whose declassify confirm is armed, between picking `public` and
    // answering FolderAudienceDialog.
    private string? _pendingAudienceFolder;

    private FrameworkElement BuildAudienceRow(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };
        var picker = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        picker.Children.Add(new TextBlock
        {
            Text = S.Get("devices/folder_audience"),
            VerticalAlignment = VerticalAlignment.Center,
        });

        // Group-bound-ness decides both the option set and the hint, and it is
        // the MLS group the share flow attached — never a separate flag.
        var bound = fs.mlsGroupId is not null;
        var combo = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(combo, Ids.FolderAudienceSelect);
        // The RENDERED value is the normalized one, never the raw column: an absent
        // audience arrives as nothing and FolderSummary defaults
        // it to "", which is outside the select's own option set. The
        // normalization is fail-closed — nothing unparseable ever resolves to
        // `public`.
        var current = uniffi.fauna_ffi.FaunaFfiMethods.NormalizeAudience(fs.audience, bound);
        int selectedIndex = -1;
        int i = 0;
        foreach (var opt in uniffi.fauna_ffi.FaunaFfiMethods.AudienceOptions(bound, current))
        {
            var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            // `selectable: false` is exactly one case — `shared` on a bound
            // folder that is not currently public. It is RENDERED because a
            // bound folder must be able to say what it is, but it is not a
            // destination: bound-ness is entered through the share flow and
            // nowhere else. It is also the folder's CURRENT value there, so the
            // no-op guard in the handler already refuses it; leaving the item
            // enabled keeps the ComboBox drivable by the cross-app select
            // contract, which addresses items by Name.
            combo.Items.Add(item);
            if (opt.value == current) selectedIndex = i;
            i++;
        }
        // Pre-select BEFORE attaching the handler (the render-echo guard).
        if (selectedIndex >= 0) combo.SelectedIndex = selectedIndex;
        var name = fs.name;
        combo.SelectionChanged += async (sender, _) => await AudienceChanged(sender, name, current);
        picker.Children.Add(combo);
        section.Children.Add(picker);

        // The hint rides the SAME (bound, current) inputs as the option set, so
        // the copy and the picker can never disagree about what is offered.
        section.Children.Add(new TextBlock
        {
            Text = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.AudienceHint(bound, current)),
            Opacity = 0.6,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
        });
        return section;
    }

    private async Task AudienceChanged(object sender, string setName, string currentAudience)
    {
        if (_machine is null || sender is not ComboBox c) return;
        if (c.SelectedItem is not ComboBoxItem item || item.Tag is not string value) return;
        // Re-selecting the folder's current audience is a no-op, never a write —
        // and it is what terminates the snap-back below, which re-enters this
        // handler with value == currentAudience. It is also what makes a bound
        // folder's non-selectable `shared` option inert, since `shared` IS the
        // current value exactly there.
        if (value == currentAudience) return;

        if (value == AudiencePublic)
        {
            // Picking Public ARMS; it does not publish.
            //
            // ⚠ Snap the select back to the CURRENT audience FIRST, then arm. A
            // WinUI ComboBox commits its pick immediately — unlike apple's
            // Picker, which repaints off the live snapshot and needs no manual
            // revert — so without this the row would paint `Public` while the
            // confirm sits unanswered and the nest has not moved, reporting an
            // audience the folder does not have. That is the assertion no other
            // test makes and the one an app is most likely to get wrong; it is
            // also the defect the residency twin beside this shipped live for a
            // week.
            for (int i = 0; i < c.Items.Count; i++)
            {
                if (c.Items[i] is ComboBoxItem ci && ci.Tag is string v && v == currentAudience)
                {
                    c.SelectedIndex = i;
                    break;
                }
            }
            FolderAudienceDialog.XamlRoot = this.XamlRoot;
            await Controls.Dialogs.ShowAsync(FolderAudienceDialog,
                prepare: () => _pendingAudienceFolder = setName);
            return;
        }

        // Every other direction the picker offers commits directly — including a
        // bound folder's `→shared` flip-back, which is KEYLESS like the rest:
        // each member's own engine re-seals the corpus off the projected
        // audience, not this caller.
        await _machine.SetFolderAudience(setName, value);
    }

    private async void FolderAudienceConfirm_Click(object sender, RoutedEventArgs e)
    {
        FolderAudienceDialog.Hide();
        if (_machine is not null && _pendingAudienceFolder is { } name)
            await _machine.SetFolderAudience(name, AudiencePublic);
        _pendingAudienceFolder = null;
    }

    // ── Following a public folder (phase 4 slice 4f-iii) ──
    // folders.md § Following a public folder; behavior authority
    // behavior/folders.md § Publicly-synced follow.
    //
    // Three shapes this leg keeps, all of them cross-app rules rather than
    // windows choices: the section is ALWAYS offered (the button is how a user
    // gets their first follow, so gating it on a non-empty list makes it
    // unreachable); a followed folder is a row in its OWN list and never a
    // `folder-row` (it has no roster, no binding and no seat); and the owner half
    // REUSES `recipient-picker-input` — no second picker, exactly as the share
    // flow does (priority #2).
    //
    // Both writes go through the shared `follow_ops` recipes over the FFI, whose
    // façade has already done the two-arm error match. `NotFound` is ONE arm on
    // purpose: absent, private and misspelled are folded by the home nest so
    // nothing can probe for the existence of a sealed folder, and an app that
    // invented a friendlier per-case message would hand back exactly the
    // distinction the nest refused to make.

    // The reused recipient picker, injected into FollowFlowPanel on first reveal
    // (the page builds this flow in C# for the same reason the share row does:
    // there is no fixed XAML slot for a control that exists only while the flow
    // is open).
    private FaunaApp.Controls.RecipientPicker? _followPicker;

    // The picker exposes no public "current text" getter, so the raw input is
    // tracked off its change event and read at confirm time — the same shape
    // BuildSharingSection uses for the share confirm.
    private string? _followOwnerInput;

    private void FollowButton_Click(object sender, RoutedEventArgs e)
    {
        if (_followPicker is null)
        {
            _followPicker = new FaunaApp.Controls.RecipientPicker();
            _followPicker.RawInputChanged += (_, text) => _followOwnerInput = text;
            // Ahead of the name box, so the flow reads owner-then-name.
            FollowFlowPanel.Children.Insert(0, _followPicker);
        }
        var opening = FollowFlowPanel.Visibility != Visibility.Visible;
        FollowFlowPanel.Visibility = opening ? Visibility.Visible : Visibility.Collapsed;
        if (!opening) return;
        // A newly-revealed control can land outside the ScrollViewer's viewport,
        // where is_visible fails on an offscreen-but-realized element. Collapsed →
        // Visible needs a fresh measure/arrange pass before StartBringIntoView
        // sees real bounds, so defer with the one-shot LayoutUpdated idiom this
        // page already uses for the share confirm.
        void OnLayout(object? s, object le)
        {
            if (FollowConfirmBtn.ActualHeight <= 0) return;
            FollowConfirmBtn.StartBringIntoView();
            FollowConfirmBtn.LayoutUpdated -= OnLayout;
        }
        FollowConfirmBtn.LayoutUpdated += OnLayout;
    }

    private async void FollowConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || _machine is null) return;
        // The RAW typed owner: a handle or a bare 64-hex actor id, the same
        // superset share_set takes. Classifying it is the shared recipe's job,
        // not this leg's.
        var owner = (_followOwnerInput ?? string.Empty).Trim();
        var folderName = FollowNameBox.Text.Trim();
        try
        {
            await _rpc.FoldersFollowPublicAsync(owner, folderName);
            FollowFlowPanel.Visibility = Visibility.Collapsed;
            FollowNameBox.Text = string.Empty;
            // Repaint the followed rows from the machine's own re-read rather
            // than from the list the call returned — one source of truth for the
            // section, the same rule the folder list follows.
            await _machine.Refresh();
        }
        catch (Exception ex)
        {
            // The façade already folded the causes; surface its wording verbatim
            // on the page's one error-message bar.
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
        }
    }

    private void RenderFollowed(IReadOnlyList<FollowedFolderSummary> followed)
    {
        FollowedContainer.Children.Clear();
        for (int i = 0; i < followed.Count; i++)
            FollowedContainer.Children.Add(BuildFollowedRow(followed[i]));
    }

    private FrameworkElement BuildFollowedRow(FollowedFolderSummary f)
    {
        // The ROW is the scope its status/badge/unfollow children resolve under,
        // so it needs a non-empty AutomationProperties.Name or UIA prunes the
        // container and a FlaUI count returns 0
        // (reference_winui_flaui_datatemplate_name). A read-only row is never
        // itself clicked — only its unfollow button is — so a Grid root, with no
        // invoke pattern, is the right shape here.
        //
        // Whose folder it is (ui/folders.md § Following a public folder — the owner
        // handle): the one precomputed `owner_display` string, painted on the row's
        // own text so `folder-followed-item[k]`'s read carries it — both halves of
        // it: the bridge joins the row's TextBlocks, and the row's Name joins the
        // same two strings. Never re-derive the handle-or-short-id fallback here.
        var ownerLabel = S.Format("devices/followed_owner", f.ownerDisplay);
        var row = new Grid { ColumnSpacing = 8, Padding = new Thickness(4) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderFollowedItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, $"{f.displayName} {ownerLabel}");
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var name = new TextBlock
        {
            Text = f.displayName,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        var owner = new TextBlock
        {
            Text = ownerLabel,
            Opacity = 0.6,
            FontSize = 12,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        var nameAndOwner = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        nameAndOwner.Children.Add(name);
        nameAndOwner.Children.Add(owner);
        Grid.SetColumn(nameAndOwner, 0);
        row.Children.Add(nameAndOwner);

        // The Public provenance badge — a followed row is somebody else's folder,
        // read off the public plane with no keys.
        var badge = new TextBlock
        {
            Text = S.Get("devices/followed_public_badge"),
            Opacity = 0.6,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(badge, 1);
        row.Children.Add(badge);

        // Following | No longer available. The revoke IS the state: when the home
        // nest stops serving it the row flips loudly and STAYS until the user
        // removes it, and a re-flip resumes it. Dynamic label read by e2e
        // get_text ⇒ AutomationId only, never a static Name
        // (reference_windows_flaui_gettext_dynamic_label_no_name).
        var status = new TextBlock
        {
            Text = f.available
                ? S.Get("devices/followed_status_following")
                : S.Get("devices/followed_status_unavailable"),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(status, Ids.FolderFollowedStatus);
        Grid.SetColumn(status, 2);
        row.Children.Add(status);

        var unfollow = new Button { Content = S.Get("devices/unfollow_folder") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unfollow, Ids.FolderUnfollowButton);
        Grid.SetColumn(unfollow, 3);
        var homeNestUrl = f.homeNestUrl;
        var folderId = f.folderId;
        unfollow.Click += async (_, _) =>
        {
            if (_rpc is null || _machine is null) return;
            try
            {
                // Purely local — nothing to revoke anywhere, because the home
                // nest never knew this follower existed.
                await _rpc.FoldersUnfollowPublicAsync(homeNestUrl, folderId);
                await _machine.Refresh();
            }
            catch (Exception ex)
            {
                ErrorBar.Message = Strings.Error(ex);
                ErrorBar.IsOpen = true;
                App.CurrentErrorMessage = ErrorBar.Message;
            }
        };
        row.Children.Add(unfollow);
        return row;
    }

    // "Serve this folder as your website" (folder-website-toggle) — the
    // structural sibling of the WebDAV toggle below, and the door phase 2 slice
    // e closed when the wizard's mode step retired.
    //
    // It stays ENABLED on a folder that is neither public nor paywalled, and is
    // merely inert there: the flag publishes the folder's HEAD, the audience
    // decides who may READ it. Disabling it would imply the setting is
    // unavailable and would strand the user with no way to prepare a site before
    // publishing it.
    //
    // Keyless like the audience above — SetFolderWebsiteEnabled is a plain
    // fauna.folders.update, NOT the serve_set orchestration the WebDAV toggle
    // runs; nothing here touches a content key.
    private FrameworkElement BuildWebsiteToggleRow(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };

        var toggle = new ToggleSwitch { Header = S.Get("devices/serve_website") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(toggle, Ids.FolderWebsiteToggle);
        // HelpText carries the served state, the same get_attr("state") channel
        // folder-webdav-toggle beside it uses.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(toggle, fs.websiteEnabled ? "on" : "off");
        // Set IsOn BEFORE attaching Toggled so the initial bind doesn't fire a
        // render echo (the guard every toggle on this page keeps).
        toggle.IsOn = fs.websiteEnabled;
        var name = fs.name;
        var wasEnabled = fs.websiteEnabled;
        toggle.Toggled += async (sender, _) =>
        {
            if (_machine is null || sender is not ToggleSwitch t) return;
            if (t.IsOn == wasEnabled) return;
            await _machine.SetFolderWebsiteEnabled(name, t.IsOn);
        };
        section.Children.Add(toggle);

        // The wording is the shared TRI-state on the live serving picture:
        // publishing a site takes switches in TWO places — this toggle plus the
        // actor's own web-address opt-in — and a user who flipped only this half
        // was told nothing while the nest served its info page in their site's
        // place. _websiteAddressEnabled is the best-effort second half; UNKNOWN
        // is its own arm and must never claim the site is live.
        section.Children.Add(new TextBlock
        {
            Text = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.WebsiteServeHint(
                uniffi.fauna_ffi.FaunaFfiMethods.NormalizeAudience(fs.audience, fs.mlsGroupId is not null),
                fs.webPaywallTier is not null,
                _websiteAddressEnabled)),
            Opacity = 0.6,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
        });
        return section;
    }

    // ── The nest place's snapshot policy (folder-nest-*; backup-restore.md § 8b) ──
    //
    // Three knobs, each three-state (on / off / UNSET — "nothing authoritative said",
    // the resting value of every folder and where a knob RETURNS when its owner picks
    // the default), plus the version-retention sibling pair (file-versions.md
    // § Retention ruling 1, apps row 323). The select spells its third state out; for
    // the text boxes a BLANK box IS that third state — so all six are STAGED and
    // committed together by `folder-nest-save-button`, never applied on change like the
    // conflict select above (an apply-on-change knob would have to send its siblings
    // with it, committing half-typed values the user had not saved).
    //
    // The buffers ⇄ policy rules are shared Rust (`fauna_folders_machine::nest_place`
    // over fauna-ffi): this page stages six strings and calls the two prefill + two
    // write functions — it must not re-derive either trap (blank is a VALUE, so a zero
    // bound renders blank and a blank box reaches the nest as unset, never `0`; and a
    // cleared retention rides as the canonical binds-nothing policy, never the wire's
    // leave-unchanged `null`).

    // The staged controls of one row's editor, carried on the save button's Tag (the
    // same idiom DevicesPage's `DeviceRow` uses for per-row state).
    private sealed record NestPlaceControls(
        string Name,
        ComboBox Snapshots,
        TextBox Quiet,
        TextBox RetentionSnapshots,
        TextBox RetentionDays,
        TextBox VersionCount,
        TextBox VersionDays);

    private FrameworkElement BuildNestPlaceSection(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 6 };
        section.Children.Add(new TextBlock
        {
            Text = S.Get("devices/nest_place_section"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });

        // Seed every control from the row through the shared prefill BEFORE any handler
        // exists (the render-echo discipline the two selects above follow): an unset
        // knob shows BLANK, and so does a ZERO retention bound — zero is the nest's own
        // spelling of unset, so rendering it would turn "nothing chosen" into a bound the
        // owner appears to have picked.
        var seed = uniffi.fauna_ffi.FaunaFfiMethods.NestPlaceEditFromRow(
            fs.nestSnapshots, fs.nestSnapshotQuietSecs, fs.retentionPolicy);
        var versionSeed = uniffi.fauna_ffi.FaunaFfiMethods.VersionRetentionEditFromBounds(
            fs.versionRetentionMaxVersions, fs.versionRetentionMaxAgeDays);

        // Same value/label split as `folder-conflict-policy-select`: each item paints the
        // localized label and carries the WIRE VALUE ("default" / "on" / "off") as its
        // UIA Name, so the cross-app `select(id, value)` contract matches by Name and
        // `get_text` reads the stable value back. Options come from the shared catalog.
        var snapshots = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(snapshots, Ids.FolderNestSnapshotsSelect);
        int selectedIndex = -1;
        int i = 0;
        foreach (var opt in uniffi.fauna_ffi.FaunaFfiMethods.NestSnapshotsOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            snapshots.Items.Add(item);
            if (opt.value == seed.snapshots) selectedIndex = i;
            i++;
        }
        if (selectedIndex >= 0) snapshots.SelectedIndex = selectedIndex;
        section.Children.Add(LabeledRow(S.Get("devices/nest_snapshots"), snapshots));

        var quiet = NestPlaceBox("folder-nest-quiet-input", "devices/nest_quiet", seed.quietSecs);
        section.Children.Add(LabeledRow(S.Get("devices/nest_quiet"), quiet));
        var retentionSnapshots = NestPlaceBox("folder-nest-retention-snapshots", "devices/nest_retention_snapshots", seed.retentionSnapshots);
        section.Children.Add(LabeledRow(S.Get("devices/nest_retention_snapshots"), retentionSnapshots));
        var retentionDays = NestPlaceBox("folder-nest-retention-days", "devices/nest_retention_days", seed.retentionDays);
        section.Children.Add(LabeledRow(S.Get("devices/nest_retention_days"), retentionDays));
        var versionCount = NestPlaceBox("folder-version-retention-count", "devices/version_retention_count", versionSeed.count);
        section.Children.Add(LabeledRow(S.Get("devices/version_retention_count"), versionCount));
        var versionDays = NestPlaceBox("folder-version-retention-days", "devices/version_retention_days", versionSeed.days);
        section.Children.Add(LabeledRow(S.Get("devices/version_retention_days"), versionDays));

        // Not decoration: the only on-screen statement that emptying a box is a real
        // choice rather than a no-op.
        section.Children.Add(new TextBlock
        {
            Text = S.Get("devices/nest_place_blank_hint"),
            Opacity = 0.6,
            TextWrapping = TextWrapping.Wrap,
        });

        var save = new Button
        {
            Content = S.Get("devices/nest_save"),
            HorizontalAlignment = HorizontalAlignment.Right,
            Tag = new NestPlaceControls(
                fs.name, snapshots, quiet, retentionSnapshots, retentionDays, versionCount, versionDays),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(save, Ids.FolderNestSaveButton);
        save.Click += NestPlaceSave_Click;
        section.Children.Add(save);
        return section;
    }

    private static TextBox NestPlaceBox(string id, string placeholderKey, string seed)
    {
        var box = new TextBox { PlaceholderText = S.Get(placeholderKey), Text = seed, MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(box, id);
        return box;
    }

    private static FrameworkElement LabeledRow(string label, FrameworkElement control)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        row.Children.Add(new TextBlock
        {
            Text = label,
            VerticalAlignment = VerticalAlignment.Center,
            Width = 220,
            TextWrapping = TextWrapping.Wrap,
        });
        row.Children.Add(control);
        return row;
    }

    private async void NestPlaceSave_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || sender is not Button b || b.Tag is not NestPlaceControls c) return;
        // The six raw control values go through the shared write functions, which own
        // both traps: every knob rides on every save (the nest applies the policy whole,
        // so an emptied box must arrive as unset), and a cleared retention rides as the
        // canonical binds-nothing policy — never `null`, which the wire reads as "leave
        // unchanged" and would let a user set retention and never take it back. An
        // unselected combo reads as "" and the shared parse lands it on unset.
        var snapshotsValue = (c.Snapshots.SelectedItem as ComboBoxItem)?.Tag as string ?? "";
        var write = uniffi.fauna_ffi.FaunaFfiMethods.NestPlaceWrite(
            new NestPlaceEdit(snapshotsValue, c.Quiet.Text ?? "", c.RetentionSnapshots.Text ?? "", c.RetentionDays.Text ?? ""));
        // The version-retention sibling rides the same save, its own family sent
        // whole: both boxes blank ⇒ the binds-nothing write that clears the policy
        // (never the wire's leave-unchanged null — the knobs are on screen, so what
        // they say is what the user said).
        var versionRetention = uniffi.fauna_ffi.FaunaFfiMethods.VersionRetentionWrite(
            new VersionRetentionEdit(c.VersionCount.Text ?? "", c.VersionDays.Text ?? ""));
        // One `fauna.folders.update`; the machine refreshes on success (RenderFolders
        // re-opens this row) and surfaces a failure on the page ErrorBar through the
        // observer, like every other gesture on this page.
        await _machine.SetFolderNestPlace(
            c.Name, write.snapshots, write.quietSecs, write.retention, versionRetention);
    }

    // ── Sync defaults (page-level; sync-default-conflict-policy-select) ──
    // The global default conflict policy stamped onto NEWLY created sets
    // (fauna.state.sync-prefs over the shared sync-prefs FFI; file-sync.md §
    // Conflicts, policy). Existing sets keep their own row policy.

    // True while the combo is being populated/pre-selected programmatically, so
    // the SelectionChanged handler never echoes a load back as a save.
    private bool _suppressDefaultPolicyEvents;

    private async Task LoadSyncDefaultsAsync()
    {
        if (_rpc is null) return;
        string? stored = null;
        try { stored = await _rpc.DefaultConflictPolicyGetAsync(); }
        catch { /* best-effort — absent preference renders as auto */ }
        var current = stored ?? "auto";

        _suppressDefaultPolicyEvents = true;
        DefaultConflictPolicyCombo.Items.Clear();
        int selectedIndex = 0;
        int i = 0;
        foreach (var opt in uniffi.fauna_ffi.FaunaFfiMethods.ConflictPolicyOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(opt.label), Tag = opt.value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, opt.value);
            DefaultConflictPolicyCombo.Items.Add(item);
            if (opt.value == current) selectedIndex = i;
            i++;
        }
        DefaultConflictPolicyCombo.SelectedIndex = selectedIndex;
        _suppressDefaultPolicyEvents = false;
    }

    private async void DefaultConflictPolicy_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (_suppressDefaultPolicyEvents || _rpc is null) return;
        if (DefaultConflictPolicyCombo.SelectedItem is not ComboBoxItem item || item.Tag is not string value) return;
        try { await _rpc.DefaultConflictPolicySetAsync(value); }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    // Per-set "serve over WebDAV" opt-in (every owner row) — webdav-server.md §
    // Independent enablement point 2 / folders.md § Element IDs. Flipping it drives
    // the shared FoldersAuthor::serve_set via FaunaFfiMethods.FoldersServeSet:
    // content-key genesis/migration + WebdavKeysBlob provision (ON), or content-key
    // rotation + blob re-provision without the set (OFF). The row's served state
    // reads from FolderSummary.webdavEnabled. Disabled-with-hint when _canServeWebdav
    // is false (webdav-server.md § Implementation status → 6b-2 c disable-with-hint):
    // serve_set flips the nest webdav_enabled flag BEFORE it re-provisions, so an actor
    // with no mail MSEK would otherwise commit the flag and only then fail NoMsek.
    private FrameworkElement BuildWebdavToggleRow(FolderSummary fs)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };

        var toggle = new ToggleSwitch
        {
            Header = S.Get("devices/serve_webdav"),
            DataContext = fs,
            IsEnabled = _canServeWebdav,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(toggle, Ids.FolderWebdavToggle);
        // HelpText carries the served/unserved state (mirrors folder-location-mode-toggle's
        // mode HelpText — reference_windows_flaui_state_attr_helptext).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(toggle, fs.webdavEnabled ? "served" : "unserved");
        // Set IsOn BEFORE attaching Toggled so the initial bind doesn't fire a render echo.
        toggle.IsOn = fs.webdavEnabled;
        toggle.Toggled += WebdavToggle_Toggled;
        row.Children.Add(toggle);

        row.Children.Add(new TextBlock
        {
            Text = S.Get(_canServeWebdav ? "devices/serve_webdav_hint" : "devices/serve_webdav_needs_mail"),
            Opacity = 0.6,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
        });
        return row;
    }

    /// <summary>
    /// <c>folder-webdav-toggle</c>: flip the set's WebDAV serve state
    /// (<c>FoldersServeSetAsync</c> → the shared <c>FoldersAuthor::serve_set</c>).
    /// Guards the render echo (a toggle whose new value already matches the row's
    /// state is the binding setting it, not a user gesture) exactly like
    /// <see cref="LocationModeToggle_Toggled"/>. A <c>NoMsek</c> failure (the actor
    /// has no mail credential yet — webdav-server.md § Implementation status
    /// today) is an expected, not-yet-designed-for case: it surfaces on the shared
    /// error-message/ErrorBar like any other failed gesture, same guard shape as
    /// <see cref="LeaveSharedFolderAsync"/>. Always refreshes afterward (success
    /// or failure) so the toggle reflects nest ground truth, mirroring
    /// <c>LocationsViewModel</c>'s always-re-list-after-mutation convention.
    /// </summary>
    private async void WebdavToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch t || t.DataContext is not FolderSummary fs) return;
        var wantServed = t.IsOn;
        if (wantServed == fs.webdavEnabled) return; // render echo, not a user toggle

        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_serve_webdav", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            t.IsOn = fs.webdavEnabled;
            return;
        }
        // Captured into locals so the non-null state survives the await below.
        var session = _convSession;
        var rpc = _rpc;

        try
        {
            await rpc.FoldersServeSetAsync(session, fs.name, fs.mlsGroupId, wantServed);
            // Either direction moves the set's content key; the agent re-keys on the
            // custody write's account-state change itself (on-demand-files.md § Shared sets
            // on a capability host → One mechanism).
        }
        catch (Exception ex)
        {
            ErrorBar.Message = S.Format("devices/error_serve_webdav", ex.Message);
            ErrorBar.IsOpen = true;
        }
        finally
        {
            if (_machine is not null) await _machine.Refresh();
        }
    }

    // Per-set "paywall to tier" control (folder-paywall-tier-select, website-enabled rows
    // only) — folders.md § Web paywall / monetization.md § Pillar 2. The structural
    // sibling of the webdav toggle above: NOT a DevicesMachine config write,
    // it runs the full paywall orchestration (content-key genesis/re-seal + the nest
    // web_paywall_tier flag + the web-serve-holder content.read{folder:set} grant mint)
    // via the shared FoldersAuthor::paywall_set, over FaunaFfiMethods.FoldersPaywallSet.
    //
    // Same value/label ComboBox split as the conflict select: each item
    // DISPLAYS its label but carries the WIRE VALUE as its UIA Name — each own-tier's
    // name, plus an empty-string sentinel for the "Not paywalled (public)" placeholder —
    // so driver.select(id, tier) matches by Name and get_text reads the value back.
    //
    // v1 is SET-ONLY (ratified 2026-07-13): the placeholder is offered only while the set
    // is still public; once paywalled it is gone (no clear affordance — no client offers
    // one yet). No tiers ⇒ nothing to paywall to: the select is DISABLED with a "create a
    // tier first" hint, mirroring the webdav "set up mail first" gate.
    private FrameworkElement BuildPaywallTierRow(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };
        var picker = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        picker.Children.Add(new TextBlock
        {
            Text = S.Get("devices/paywall_tier"),
            VerticalAlignment = VerticalAlignment.Center,
        });

        var current = fs.webPaywallTier;
        var hasTiers = _ownTiers.Count > 0;

        // Model = wire values. The empty placeholder rides only while the set is public.
        // A tier the set is already paywalled to is always present even if it has since
        // been removed from the tier list, so the row still shows its own state.
        var values = new List<string>();
        if (current is null) values.Add("");
        values.AddRange(_ownTiers);
        if (current is not null && !_ownTiers.Contains(current)) values.Add(current);

        var combo = new ComboBox { MinWidth = 220, IsEnabled = hasTiers };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(combo, Ids.FolderPaywallTierSelect);
        int selectedIndex = 0;
        for (int i = 0; i < values.Count; i++)
        {
            var value = values[i];
            var item = new ComboBoxItem
            {
                Content = value.Length == 0 ? S.Get("devices/paywall_tier_none") : value,
                Tag = value,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, value);
            combo.Items.Add(item);
            if (current is not null && value == current) selectedIndex = i;
        }
        // Pre-select the row's current tier BEFORE attaching the handler (the render-echo
        // guard, same as the conflict picker). Public ⇒ the placeholder at 0.
        combo.SelectedIndex = selectedIndex;
        combo.Tag = fs;
        combo.SelectionChanged += PaywallTierChanged;
        picker.Children.Add(combo);
        section.Children.Add(picker);

        // Say WHY it is disabled, inline — a tooltip alone is not discoverable on a
        // control the user cannot focus (same intent as the webdav needs-mail hint).
        section.Children.Add(new TextBlock
        {
            Text = S.Get(hasTiers ? "devices/paywall_tier_hint" : "devices/paywall_tier_needs_tier"),
            Opacity = 0.6,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
        });
        return section;
    }

    /// <summary>
    /// <c>folder-paywall-tier-select</c>: paywall the set to the picked tier
    /// (<c>FoldersPaywallSetAsync</c> → the shared <c>FoldersAuthor::paywall_set</c>).
    /// The empty placeholder is NOT a write — v1 is set-only, there is no "clear the
    /// paywall" path yet — and re-picking the tier the set already carries is a render
    /// echo, not a user gesture. Always refreshes afterward (success or failure) so the
    /// select reflects nest ground truth, like <see cref="WebdavToggle_Toggled"/>.
    /// </summary>
    private async void PaywallTierChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is not ComboBox c || c.Tag is not FolderSummary fs) return;
        if (c.SelectedItem is not ComboBoxItem item || item.Tag is not string tier) return;
        if (tier.Length == 0) return;              // the "Not paywalled" placeholder — no write
        if (tier == fs.webPaywallTier) return;     // render echo, not a user pick

        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = PaywallError(S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // Captured into locals so the non-null state survives the await below.
        var session = _convSession;
        var rpc = _rpc;

        try
        {
            await rpc.FoldersPaywallSetAsync(session, fs.name, fs.mlsGroupId, tier);
        }
        catch (Exception ex)
        {
            ErrorBar.Message = PaywallError(ex.Message);
            ErrorBar.IsOpen = true;
        }
        finally
        {
            if (_machine is not null) await _machine.Refresh();
        }
    }

    /// <summary>The paywall failure banner. <c>devices/error_paywall_set</c> carries its
    /// own <c>{message}</c> — like every key in the family and like
    /// <c>devices/error_serve_webdav</c> above — so it formats rather than concatenates
    /// (ui/README.md § Copy comprehensibility: the hand-built prefix was the convention
    /// that let tui and windows drop the detail while linux/web/android kept it).</summary>
    private static string PaywallError(string detail) =>
        S.Format("devices/error_paywall_set", detail);

    private async void SavePaths_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || sender is not Button b || b.Tag is not string name) return;
        await _machine.SetFolderPaths(
            name,
            uniffi.fauna_ffi.FaunaFfiMethods.ParsePathsField(_expandedInclude?.Text ?? ""),
            uniffi.fauna_ffi.FaunaFfiMethods.ParsePathsField(_expandedExclude?.Text ?? ""));
    }

    private async void DeleteFolder_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button b || b.Tag is not string name) return;
        FolderDeleteDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(FolderDeleteDialog, prepare: () =>
        {
            _pendingDeleteFolder = name;
            FolderDeleteText.Text = S.Get("devices/delete_confirm_body").Replace("{name}", name);
            FolderDeleteDialog.Title = S.Get("devices/delete_confirm_title");
        });
    }

    private async void FolderDeleteConfirm_Click(object sender, RoutedEventArgs e)
    {
        FolderDeleteDialog.Hide();
        if (_machine is not null && _pendingDeleteFolder is { } name)
            await _machine.DeleteFolder(name);
        _pendingDeleteFolder = null;
    }

    // ── Per-set device activity (fauna.folders.devices) — file-sync.md §
    // Implementation status today. The ordinary sync change signal, distinct
    // from the set's cached snapshot totals (snapshot-only). Lazy-loaded on
    // first expand (below) and LIVE-UPDATED on every fauna.sync.changed push while
    // this exact set stays expanded (OnFolderChangedPushed) — that live refresh
    // is the entire point, mirroring linux's populate_fileset_devices /
    // web's loadDeviceActivity.

    private FrameworkElement BuildDeviceActivitySection(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };
        section.Children.Add(new TextBlock
        {
            Text = S.Get("devices/device_activity"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });

        var container = new StackPanel { Spacing = 2 };
        container.Children.Add(new TextBlock
        {
            Text = S.Get("common/loading"),
            Opacity = 0.6,
            FontSize = 12,
        });
        section.Children.Add(container);

        _expandedDeviceActivityContainer = container;
        _ = LoadDeviceActivityAsync(fs.name, container);
        return section;
    }

    /// <summary>
    /// Fetch + repaint the device-activity roster for set <paramref name="name"/>
    /// into <paramref name="container"/> — called once on first expand
    /// (<see cref="BuildDeviceActivitySection"/>) and again on every
    /// <c>fauna.sync.changed</c> push for this exact set while it stays expanded
    /// (<see cref="OnFolderChangedPushed"/>). Re-runnable in place, mirroring
    /// linux's <c>populate_fileset_devices</c>.
    /// </summary>
    private async Task LoadDeviceActivityAsync(string name, StackPanel container)
    {
        if (_rpc is null) return;
        try
        {
            var devices = await _rpc.FoldersDevicesAsync(name);
            container.Children.Clear();
            if (devices.Count == 0)
            {
                container.Children.Add(new TextBlock
                {
                    Text = S.Get("devices/no_device_activity"),
                    Opacity = 0.6,
                    FontSize = 12,
                });
                return;
            }
            foreach (var d in devices)
                container.Children.Add(BuildDeviceActivityRow(d));
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    // ── The post-create device-place editor (folders re-model phase 2) ──
    // folders.md § Implementation status today owns the shape; tui led it
    // 2026-08-19, web/linux/apple followed, windows is the sixth.
    //
    // Slice b landed `fauna.folders.places.set` and slice f made every flag
    // point writable — with no app able to reach an EXISTING seat's flags,
    // because the wizard sets them only at creation. This section is that door.
    //
    // Three rules it must keep, none of them windows' own invention:
    //
    //   1. C# DERIVES NOTHING. `fauna_protocol::folders::place_rows` has already
    //      run at the FFI boundary, so each FfiFolderMember arrives carrying its
    //      flag triple; the row paints it as-is.
    //   2. EVERY seat paints its row and its three checkboxes, and keeps its
    //      index — `folder-place-row[j]` is the e2e address.
    //   3. A toggle writes the WHOLE point (all three flags together) and then
    //      repaints from a roster RE-READ. Nest truth, never the optimistic
    //      flip: the row must show what the nest now holds.
    //
    // `get_attr(id, "state")` must answer the literal "on"/"off". On windows
    // that read resolves to AutomationProperties.HelpText (the FlaUI bridge's
    // GetAttr default arm — a ToggleState enum is NOT the string the cross-app
    // contract wants), so each box stamps the two literals itself, exactly as
    // folder-webdav-toggle stamps served/unserved.

    private FrameworkElement BuildPlaceEditorSection(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4 };
        section.Children.Add(new TextBlock
        {
            Text = S.Get("devices/folder_places_title"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });

        var container = new StackPanel { Spacing = 2 };
        section.Children.Add(container);
        _ = LoadPlaceRowsAsync(fs.name, container);
        return section;
    }

    /// <summary>
    /// Fetch the device roster for <paramref name="name"/> and repaint the place
    /// rows into <paramref name="container"/>. Re-runnable in place — which is
    /// what makes rule 3 above real: every toggle calls it again, so the boxes
    /// show the nest's answer rather than the click.
    /// </summary>
    private async Task LoadPlaceRowsAsync(string name, StackPanel container)
    {
        if (_rpc is null) return;
        IReadOnlyList<FfiFolderMember> seats;
        try
        {
            // The page's own unsealed device roster names each seat.
            var devices = (IReadOnlyList<DeviceSummary>?)_machine?.Snapshot().devices
                ?? new List<DeviceSummary>();
            seats = await _rpc.FoldersPlaceRowsAsync(name, devices);
        }
        catch (Exception ex)
        {
            // A roster READ failure never blanks the folder body — the section
            // degrades to no rows — but it does SAY so on the page's one
            // error-message bar. Apple maps this arm to a silent empty list;
            // windows does not, because a silent empty roster here is
            // indistinguishable from "this folder genuinely has no seats", and
            // that ambiguity is exactly what makes the section undebuggable
            // from a failing test (measured: the first `--app windows` run of
            // `test_folder_place_editor.py` reported only `count=0`, with the
            // cause swallowed).
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
            seats = System.Array.Empty<FfiFolderMember>();
        }
        // ⚠ The RENDER is inside a try of its own, not left bare after the read's.
        // This method is fire-and-forget (`_ = LoadPlaceRowsAsync(...)`), so an
        // exception thrown while building a row would be swallowed by the
        // orphaned Task and leave the just-cleared container empty — visible to a
        // test as nothing but `count=0`, indistinguishable from a folder with no
        // seats. That is precisely how this leg's first three e2e runs read.
        try
        {
            RenderPlaceRows(name, seats, container);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("FoldersPage", $"place editor render failed: {ex}");
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
        }
    }

    private void RenderPlaceRows(
        string name, IReadOnlyList<FfiFolderMember> seats, StackPanel container)
    {
        container.Children.Clear();
        if (seats.Count == 0)
        {
            container.Children.Add(new TextBlock
            {
                Text = S.Get("devices/no_devices_enrolled"),
                Opacity = 0.6,
                FontSize = 12,
            });
            return;
        }
        foreach (var seat in seats)
            container.Children.Add(BuildPlaceRow(name, seat, container));
    }

    private FrameworkElement BuildPlaceRow(string folder, FfiFolderMember seat, StackPanel container)
    {
        // ⚠ The Name is the DEVICE ID, never the label. A non-empty
        // AutomationProperties.Name is what keeps the row in the UIA tree at all
        // — without one, UIA prunes the container and FlaUI counts 0, so the
        // scope its three checkboxes resolve under (`folder-place-row[j]`)
        // stops existing. A seat's label is nest data and can legitimately be
        // empty; the device id never is. Measured, not reasoned: with the label
        // here this leg rendered its seat (`seats=1` in the app log, no
        // exception) and the e2e still saw `count=0`. `folder-device-activity-item`
        // beside it keys on the device id for exactly this reason.
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderPlaceRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, seat.deviceId);

        row.Children.Add(new TextBlock
        {
            Text = seat.label,
            VerticalAlignment = VerticalAlignment.Center,
        });

        // The same three labels the wizard's enrollment step paints — the box
        // states plainly what it does, in the voice that replaced the role nouns
        // a live user called incomprehensible.
        row.Children.Add(PlaceBox(folder, seat, container, "folder-place-originates",
            "devices/wizard/place_originates", seat.originates));
        row.Children.Add(PlaceBox(folder, seat, container, "folder-place-accepts",
            "devices/wizard/place_accepts", seat.accepts));
        row.Children.Add(PlaceBox(folder, seat, container, "folder-place-applies-deletes",
            "devices/wizard/place_applies_deletes", seat.appliesDeletes));
        return row;
    }

    private CheckBox PlaceBox(
        string folder,
        FfiFolderMember seat,
        StackPanel container,
        string automationId,
        string labelKey,
        bool on)
    {
        var box = new CheckBox { Content = S.Get(labelKey), MinWidth = 0 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(box, automationId);
        // The literal "on"/"off" the cross-app get_attr(id, "state") contract
        // reads — see the section comment above.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(box, on ? "on" : "off");
        // Set IsChecked BEFORE attaching the handler (the render-echo guard every
        // control on this page keeps).
        box.IsChecked = on;
        box.Click += async (_, _) =>
        {
            if (_machine is null) return;
            // Rule 3: send the WHOLE point. The seat's other two flags come from
            // the record this row was painted from; only the clicked one moves.
            var originates = automationId == "folder-place-originates"
                ? box.IsChecked == true : seat.originates;
            var accepts = automationId == "folder-place-accepts"
                ? box.IsChecked == true : seat.accepts;
            var appliesDeletes = automationId == "folder-place-applies-deletes"
                ? box.IsChecked == true : seat.appliesDeletes;
            try
            {
                await _machine.SetFolderPlace(
                    folder, seat.deviceId, originates, accepts, appliesDeletes);
            }
            catch (Exception ex)
            {
                ErrorBar.Message = Strings.Error(ex);
                ErrorBar.IsOpen = true;
                App.CurrentErrorMessage = ErrorBar.Message;
            }
            // Repaint from a roster RE-READ whether the write succeeded or not:
            // on success the row shows the nest's new truth, on failure it snaps
            // back to what the nest still holds instead of keeping the click.
            await LoadPlaceRowsAsync(folder, container);
        };
        return box;
    }

    private FrameworkElement BuildDeviceActivityRow(FfiFolderDevice d)
    {
        // AutomationProperties.Name keeps the row in the UIA content view so
        // FlaUI's ByAutomationId resolves the nested child ids (mirrors
        // BuildMemberRow above).
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderDeviceActivityItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, d.deviceId);

        var label = new TextBlock { Text = d.label, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(label, Ids.FolderDeviceActivityLabel);
        row.Children.Add(label);

        // "Changes" caption — declares what the number means; the inline peer of
        // web's <th>{t.devices.col_changes}</th> column header (mirrors linux's
        // per-row caption — no shared table header here).
        row.Children.Add(new TextBlock
        {
            Text = S.Get("devices/col_changes"),
            Opacity = 0.6,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
        });

        var count = new TextBlock { Text = d.changeCount.ToString(), VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(count, Ids.FolderDeviceActivityCount);
        row.Children.Add(count);

        return row;
    }

    /// <summary>
    /// <c>fauna.sync.changed</c> push for THIS exact expanded set (transport.md §
    /// Push events; file-sync.md § Implementation status today) — the live-update
    /// half of per-set device activity, mirroring linux's
    /// <c>PushEvent::SyncChanged</c> guard (<c>folder_row_is_expanded</c>). A push
    /// for a collapsed row, or any other set, is a no-op: re-fetching costs a WS
    /// round trip for nothing the user can see.
    /// </summary>
    private void OnFolderChangedPushed(string folder)
    {
        if (_expandedFolder != folder) return;
        if (_expandedDeviceActivityContainer is not { } container) return;
        _ = LoadDeviceActivityAsync(folder, container);
    }

    /// <summary>
    /// Reconnect-sweep arm (transport.md § Which surfaces a push invalidates,
    /// windows-leg audit): a reconnect or ResyncRequired may have dropped a
    /// fauna.sync.changed push for the currently-expanded set (there is no
    /// folder name to match against here, unlike <see cref="OnFolderChangedPushed"/>),
    /// so re-fetch whatever set IS expanded unconditionally. A no-op when
    /// nothing is expanded.
    /// </summary>
    private void OnReconnected()
    {
        if (_expandedFolder is not { } folder) return;
        if (_expandedDeviceActivityContainer is not { } container) return;
        _ = LoadDeviceActivityAsync(folder, container);
    }

    // ── Per-folder destination places (backup-destinations.md § Ordinary-folder
    // coverage) — one `folder-destination-row` per ATTACHED backup destination
    // (label + detach button), then — while at least one enrolled destination
    // remains unattached — the `folder-destination-attach-select` +
    // `folder-destination-attach-button` pair. Rebuilt on every expand
    // (BuildFolderBody re-realizes the whole body), so there is no separate
    // first-expand guard the way the device-activity section needs one — no
    // live-update trigger either, since a folder's destination coverage never
    // pushes. `places.Count == 0` (no destination enrolled at all) hides the
    // whole section: an affordance that cannot work must not paint. Mirrors
    // android's `DestinationPlacesSection` / linux's
    // `populate_folder_destinations` (windows reuses android's FFI face).

    private FrameworkElement BuildDestinationPlacesSection(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 4, Visibility = Visibility.Collapsed };
        section.Children.Add(new TextBlock
        {
            Text = S.Get("devices/folder_destinations_title"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });

        var container = new StackPanel { Spacing = 4 };
        section.Children.Add(container);

        _ = RefreshDestinationPlacesAsync(fs.name, fs.id, section, container);
        return section;
    }

    /// <summary>
    /// Fetch <paramref name="folderId"/>'s destination places and repaint
    /// <paramref name="container"/> (called once per expand, and again after
    /// every attach/detach with the mutation's own re-read — never an
    /// optimistic flip).
    /// </summary>
    private async Task RefreshDestinationPlacesAsync(string name, long folderId, StackPanel section, StackPanel container)
    {
        if (_rpc is null) return;
        try
        {
            var places = await _rpc.FolderDestinationsListAsync(folderId);
            RenderDestinationPlaces(name, folderId, section, container, places);
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    private void RenderDestinationPlaces(
        string name, long folderId, StackPanel section, StackPanel container,
        IReadOnlyList<FfiFolderDestinationPlace> places)
    {
        container.Children.Clear();
        if (places.Count == 0)
        {
            section.Visibility = Visibility.Collapsed;
            return;
        }
        section.Visibility = Visibility.Visible;

        foreach (var place in places)
        {
            if (!place.attached) continue;
            container.Children.Add(BuildDestinationPlaceRow(name, folderId, section, container, place));
        }

        var attachable = places.Where(p => !p.attached).ToList();
        if (attachable.Count > 0)
            container.Children.Add(BuildDestinationAttachRow(name, folderId, section, container, attachable));
    }

    private FrameworkElement BuildDestinationPlaceRow(
        string name, long folderId, StackPanel section, StackPanel container, FfiFolderDestinationPlace place)
    {
        // AutomationProperties.Name keeps the row in the UIA content view so
        // FlaUI's ByAutomationId resolves the nested detach button (mirrors
        // BuildDeviceActivityRow above).
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.FolderDestinationRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, place.destinationId);

        row.Children.Add(new TextBlock { Text = place.label, VerticalAlignment = VerticalAlignment.Center });

        var detach = new Button { Content = S.Get("devices/folder_destination_detach") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(detach, Ids.FolderDestinationDetachButton);
        var folderSet = place.folderSet ?? "";
        detach.Click += async (_, _) =>
        {
            if (_rpc is null) return;
            try
            {
                var places = await _rpc.FolderDestinationDetachAsync(folderId, place.destinationId, folderSet);
                RenderDestinationPlaces(name, folderId, section, container, places);
            }
            catch (Exception ex)
            {
                ErrorBar.Message = Strings.Error(ex);
                ErrorBar.IsOpen = true;
            }
        };
        row.Children.Add(detach);

        return row;
    }

    /// <summary>
    /// The attach select + button pair, painted while ≥1 enrolled destination
    /// remains unattached. Same value/label ComboBox split as
    /// <see cref="BuildConflictPolicyRow"/>: each item DISPLAYS the
    /// destination's label but carries the wire value (its
    /// <c>destinationId</c>) as its UIA Name, so <c>driver.select(id, value)</c>
    /// matches by Name and the pre-selected first entry always leaves a valid
    /// pick staged for <c>folder-destination-attach-button</c> — always
    /// enabled rather than bound to a selection state
    /// (reference_windows_disabled_button_no_invoke).
    /// </summary>
    private FrameworkElement BuildDestinationAttachRow(
        string name, long folderId, StackPanel section, StackPanel container,
        IReadOnlyList<FfiFolderDestinationPlace> attachable)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };

        var combo = new ComboBox { MinWidth = 220 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(combo, Ids.FolderDestinationAttachSelect);
        foreach (var place in attachable)
        {
            var item = new ComboBoxItem { Content = place.label, Tag = place.destinationId };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, place.destinationId);
            combo.Items.Add(item);
        }
        combo.SelectedIndex = 0;
        row.Children.Add(combo);

        var attach = new Button { Content = S.Get("devices/folder_destination_attach") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(attach, Ids.FolderDestinationAttachButton);
        attach.Click += async (_, _) =>
        {
            if (_rpc is null) return;
            if (combo.SelectedItem is not ComboBoxItem item || item.Tag is not string destinationId) return;
            try
            {
                var places = await _rpc.FolderDestinationAttachAsync(folderId, destinationId);
                RenderDestinationPlaces(name, folderId, section, container, places);
            }
            catch (Exception ex)
            {
                ErrorBar.Message = Strings.Error(ex);
                ErrorBar.IsOpen = true;
            }
        };
        row.Children.Add(attach);

        return row;
    }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ──
    //
    // Inline "Shared with" section on each expanded folder-row: a Share… button
    // that reveals the REUSED conversations recipient-picker (no new picker IDs —
    // priority #2), the shared-with actor roster (lazy-loaded on expand, ONLY when
    // the set is already bound to an MLS group — fs.mlsGroupId is not null; an
    // owner-only set's members.list_actors read would return
    // fauna.folders.not_shared, which must NOT surface as a page error, so the
    // call is skipped entirely rather than caught), and a per-member remove button
    // (rotates the content key). Owner-only: the caller is always the set's owner
    // here (Slices 2-3 — a member never re-shares), so only role == "member" rows
    // render; role == "owner" is the caller themself. Recipient-side rows
    // (shared-with-me, BuildSharedWithMeRow above) and the pending-share knock
    // surface (the page-level "Shared with you" section, below) are implemented
    // elsewhere on this page (folders.md § Sharing — Recipient side).

    private FrameworkElement BuildSharingSection(FolderSummary fs)
    {
        var section = new StackPanel { Spacing = 6 };

        var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        header.Children.Add(new TextBlock
        {
            Text = S.Get("devices/shared_with"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            VerticalAlignment = VerticalAlignment.Center,
        });
        var badge = new TextBlock { VerticalAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(badge, Ids.FolderSharedBadge);
        header.Children.Add(badge);
        section.Children.Add(header);

        var members = new StackPanel { Spacing = 4 };
        section.Children.Add(members);

        var shareButton = new Button { Content = S.Get("devices/share_button") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(shareButton, Ids.FolderShareButton);
        section.Children.Add(shareButton);

        // The share row: the REUSED RecipientPicker, constructed dynamically (this
        // page builds each expanded row's body in C#, so — unlike ConversationsPage's
        // fixed-slot NewThreadPicker/AddParticipantPicker — there is no fixed XAML
        // slot to bind to) plus the confirm button. Both start collapsed; Share…
        // reveals them (ui.yaml optional_elements: folder-share-confirm present
        // only while the picker is open).
        var picker = new FaunaApp.Controls.RecipientPicker { Visibility = Visibility.Collapsed };

        // The recipient's access grant (multi-writer Phase 1, folders.md § Sharing) —
        // Reader | Writer, default Reader, from the shared member_access_options()
        // catalog (same wire-value-via-Name pattern as the member roster's
        // folder-member-role-select above). Present only while the share flow is
        // open. A fresh Writer grant always starts uncapped (the owner caps it
        // afterwards on the member row), so the shared folder-writer-uncapped-warning
        // shows advisory whenever Writer is selected here.
        var shareRoleSelect = new ComboBox { Visibility = Visibility.Collapsed, MinWidth = 100 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(shareRoleSelect, Ids.FolderShareRoleSelect);
        foreach (var option in uniffi.fauna_ffi.FaunaFfiMethods.MemberAccessOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(option.@label), Tag = option.@value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, option.@value);
            shareRoleSelect.Items.Add(item);
            if (option.@value == "reader") shareRoleSelect.SelectedItem = item;
        }
        var shareWarning = new TextBlock
        {
            Text = S.Get("devices/writer_uncapped_warning"),
            Opacity = 0.8,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(shareWarning, Ids.FolderWriterUncappedWarning);
        // The published-folder writer warning (folders.md § Sharing) — stacked with the
        // quota warning above, never replacing it. State-based: it asks the shared
        // writer_grant_reach with the set's audience as this row was built, so it is
        // right whichever of the grant and the publish came first.
        var sharePublished = new TextBlock
        {
            Opacity = 0.8,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(sharePublished, Ids.FolderWriterPublishedWarning);
        string ShareAccess() => shareRoleSelect.SelectedItem is ComboBoxItem { Tag: string t } ? t : "reader";
        void UpdateSharePublished(bool open)
        {
            var text = open ? PublishedWriterWarning(fs, ShareAccess()) : null;
            sharePublished.Text = text ?? string.Empty;
            sharePublished.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
        }
        shareRoleSelect.SelectionChanged += (_, _) =>
        {
            shareWarning.Visibility = ShareAccess() == "writer" ? Visibility.Visible : Visibility.Collapsed;
            UpdateSharePublished(open: true);
        };

        var confirm = new Button
        {
            Content = S.Get("devices/share_button"),
            Visibility = Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(confirm, Ids.FolderShareConfirm);
        section.Children.Add(picker);
        section.Children.Add(shareRoleSelect);
        section.Children.Add(shareWarning);
        section.Children.Add(sharePublished);
        section.Children.Add(confirm);

        // RecipientPicker exposes no public "current text" getter — track the raw
        // input via its change event (mirrors ConversationsPage.WireRecipientPicker's
        // NewThreadPicker.RawInputChanged → _vm.SetNewThreadRecipientInput). Captured
        // by the confirm handler below at click time.
        string? typedInput = null;
        picker.RawInputChanged += (_, text) => typedInput = text;

        // Revealing the picker is a pure UI toggle — no FFI call happens here, so
        // it is NOT gated on _convSession (unlike confirm/remove below, which DO
        // call FFI and must guard for a missing session). The e2e bridge login
        // path (set_state) never threads a real ConversationsSession into
        // ServiceClients, so gating the reveal itself would make the dialog
        // unopenable under the very fixture this page's target test drives.
        void ToggleShareFlow()
        {
            var opening = picker.Visibility != Visibility.Visible;
            picker.Visibility = opening ? Visibility.Visible : Visibility.Collapsed;
            shareRoleSelect.Visibility = opening ? Visibility.Visible : Visibility.Collapsed;
            shareWarning.Visibility = opening && ShareAccess() == "writer" ? Visibility.Visible : Visibility.Collapsed;
            UpdateSharePublished(opening);
            confirm.Visibility = opening ? Visibility.Visible : Visibility.Collapsed;
            if (!opening) return;
            // The device-activity section above (BuildDeviceActivitySection) pushes this
            // row's content further down the page than before, so a newly-revealed confirm
            // button can land outside the ScrollViewer's viewport — is_visible fails on an
            // offscreen-but-realized control even though it's logically Visible
            // (reference_windows_e2e_is_visible_offscreen; mirrors BuildFolderRow's
            // Expanding handler). Collapsed → Visible needs a fresh measure/arrange pass
            // before StartBringIntoView sees real bounds, so defer with the same one-shot
            // LayoutUpdated idiom.
            void OnLayout(object? s, object le)
            {
                if (confirm.ActualHeight <= 0) return;
                confirm.StartBringIntoView();
                confirm.LayoutUpdated -= OnLayout;
            }
            confirm.LayoutUpdated += OnLayout;
        }
        shareButton.Click += (_, _) => ToggleShareFlow();
        // Opened by a folder-share route (RenderFolders stashed this set): the
        // same reveal a Share… click performs, once.
        if (_openShareFor == fs.name)
        {
            _openShareFor = null;
            ToggleShareFlow();
        }

        confirm.Click += async (_, _) => await ShareFolderAsync(fs, picker, confirm, typedInput, ShareAccess());

        if (fs.mlsGroupId is not null)
            _ = LoadSharedMembersAsync(fs, members, badge);

        return section;
    }

    /// <summary>
    /// The set named <paramref name="name"/> as the machine's CURRENT projection
    /// sees it — re-read after a membership write so callers act on the set's new
    /// truth rather than the <c>FolderSummary</c> their row closed over at build
    /// time. The share is exactly where that matters: it is the write that turns
    /// <c>mlsGroupId</c> from null into a group, so every captured summary on an
    /// expanded row is stale the instant it succeeds.
    /// </summary>
    private FolderSummary? FreshFolder(string name)
    {
        if (_machine is null) return null;
        foreach (var f in _machine.Snapshot().folders)
            if (f.name == name) return f;
        return null;
    }

    /// <summary>
    /// Commit the <c>folder-share-confirm</c> gesture: classify the picker's typed
    /// input via the shared <c>classify_recipient</c> — the SAME parse
    /// <see cref="ContactsViewModel.ClassifyRecipientInput"/> uses (priority #2,
    /// never a per-page re-derivation) — then, for a bare handle, resolve it through
    /// the two-hop federated discovery seam exactly like
    /// <see cref="ContactsViewModel.LookUpRecipientAsync"/>'s Handle branch
    /// (resolve_nest → resolve_handle), reusing the picker's own resolve-state
    /// strings ("not-found" / "error") rather than inventing new ones. On a
    /// resolved actor: share the set, hide the
    /// share row, and refresh the page (folders.md § Sharing — Where logic lives).
    /// <paramref name="access"/> is the recipient's grant (multi-writer Phase 1,
    /// <c>folder-share-role-select</c>) — <c>"reader"</c> (the default) or
    /// <c>"writer"</c>.
    /// </summary>
    private async Task ShareFolderAsync(
        FolderSummary fs, FaunaApp.Controls.RecipientPicker picker, Button confirm, string? typedInput, string access)
    {
        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_share_set", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // Captured into locals (not re-read off the field) so the non-null state
        // survives the awaits below — a field's flow-narrowing doesn't persist
        // across an await/call boundary the way a local's does.
        var session = _convSession;
        var rpc = _rpc;

        var c = ContactsViewModel.ClassifyRecipientInput(typedInput);
        string actorIdHex;
        switch (c.Kind)
        {
            case ContactsViewModel.RecipientKind.ActorId:
                actorIdHex = c.ActorId;
                picker.UpdateResolveState(ResolveState.Resolved);
                break;

            case ContactsViewModel.RecipientKind.Handle:
                try
                {
                    // Two-hop anonymous federated discovery (libs/fauna-ffi/src/resolve.rs):
                    // this client's own home nest SRV-resolves the handle's domain →
                    // owning node URL, then that node maps the handle → actor id.
                    var nodeUrl = await uniffi.fauna_ffi.FaunaFfiMethods.ResolveNest(rpc.HomeUrl, c.Domain);
                    var resolved = await uniffi.fauna_ffi.FaunaFfiMethods.ResolveHandle(nodeUrl, c.User, c.Domain);
                    actorIdHex = resolved[0];
                    picker.UpdateResolveState(ResolveState.Resolved);
                }
                catch
                {
                    picker.UpdateResolveState(ResolveState.NotFound);
                    return;
                }
                break;

            default:
                picker.UpdateResolveState(ResolveState.Error);
                return;
        }

        try
        {
            var memberId = Convert.FromHexString(actorIdHex);
            await rpc.FoldersShareAsync(session, fs.name, memberId, memberNestUrl: null, access: access);
            picker.Visibility = Visibility.Collapsed;
            confirm.Visibility = Visibility.Collapsed;
            // The refresh re-renders the list, and `RenderFolders` re-opens the row
            // this share was performed in — which rebuilds its "Shared with" section
            // from the REFRESHED summary, whose `mlsGroupId` this share just minted.
            // That build-time load is the one that paints the new member; there is
            // deliberately no second repaint here, because the panels this method
            // closed over are detached by the re-render and writing to them would be
            // dead code that looks load-bearing.
            if (_machine is not null) await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    /// <summary>
    /// Lazy-load the shared-with actor roster on expand — ONLY called when the set
    /// is already bound to an MLS group (<c>fs.mlsGroupId is not null</c>, checked by
    /// the caller). Renders one <c>folder-member-item</c> per <c>role == "member"</c>
    /// entry and the "Shared · N" badge; an empty roster hides the badge.
    /// </summary>
    private async Task LoadSharedMembersAsync(FolderSummary fs, StackPanel members, TextBlock badge)
    {
        if (_rpc is null) return;
        try
        {
            var actors = await _rpc.FoldersMembersListActorsAsync(fs.name);
            // Only role == "member" rows are the set's shared-with roster — role ==
            // "owner" is the caller themself, not someone the set is shared WITH.
            var shared = uniffi.fauna_ffi.FaunaFfiMethods.FolderMemberActors(actors.ToArray()).ToList();

            members.Children.Clear();
            foreach (var m in shared)
                members.Children.Add(BuildMemberRow(fs, m));

            if (shared.Count > 0)
            {
                badge.Text = S.Get("devices/shared_badge").Replace("{count}", shared.Count.ToString());
                badge.Visibility = Visibility.Visible;
            }
            else
            {
                badge.Visibility = Visibility.Collapsed;
            }
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    private FrameworkElement BuildMemberRow(FolderSummary fs, uniffi.fauna_ffi.FfiFolderActorMember member)
    {
        // The outer container carries folder-member-item + the AutomationProperties.Name
        // (reference_winui_flaui_datatemplate_name) so every nested child id below —
        // including the access-grant row added here — resolves under ONE indexed scope.
        var container = new StackPanel { Spacing = 4 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(container, Ids.FolderMemberItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(container, member.actorId);

        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };

        var handle = new TextBlock { Text = member.display, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(handle, Ids.FolderMemberHandle);

        // Always "Active" — there is deliberately no optimistic "Pending" state in
        // this task (the nest read reports only actors the share already reached;
        // folders.md § Sharing — owner side).
        var status = new TextBlock { Text = S.Get("common/active"), VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(status, Ids.FolderMemberStatus);

        var remove = new Button { Content = S.Get("devices/remove_member"), Tag = member.actorId };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(remove, Ids.FolderMemberRemoveButton);
        remove.Click += async (_, _) => await RemoveSharedMemberAsync(fs, member.actorId);

        row.Children.Add(handle);
        row.Children.Add(status);
        row.Children.Add(remove);
        container.Children.Add(row);
        container.Children.Add(BuildMemberAccessRow(fs, member.actorId, member.@access, member.@byteCap));
        return container;
    }

    /// <summary>
    /// The owner-editable-in-place access grant on a member row (multi-writer Phase
    /// 1, folders.md § Sharing): <c>folder-member-role-select</c> (Reader | Writer,
    /// from the shared <c>member_access_options()</c> catalog — never hand-rolled)
    /// + <c>folder-member-cap-input</c> (blank = uncapped, commits on blur) +
    /// <c>folder-writer-uncapped-warning</c> when Writer is granted with no cap, and,
    /// stacked with it, <c>folder-writer-published-warning</c> when Writer is granted
    /// on a folder whose content is readable beyond its members (independent of the cap).
    /// Both the select and the input send the FULL (access, cap) pair on every edit
    /// — <c>members.set_access</c> upserts the whole row, so sending one half would
    /// silently clear the other (mirrors android's <c>commit(newAccess, newCapText)</c>).
    /// A static label, not PlaceholderText, on the cap input — an empty WinUI TextBox's
    /// UIA ValuePattern can read back its PlaceholderText instead of "" (measured on
    /// admin-users-max-free-users-input), which
    /// would break get_text's "blank = uncapped" contract on this field too.
    /// </summary>
    private FrameworkElement BuildMemberAccessRow(FolderSummary fs, string actorIdHex, string? access, long? byteCap)
    {
        var accessRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
        var roleSelect = new ComboBox { MinWidth = 100 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(roleSelect, Ids.FolderMemberRoleSelect);
        var capLabel = new TextBlock { Text = S.Get("devices/member_byte_cap"), VerticalAlignment = VerticalAlignment.Center };
        var capInput = new TextBox { Width = 100, Text = byteCap?.ToString() ?? string.Empty };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(capInput, Ids.FolderMemberCapInput);

        var warning = new TextBlock
        {
            Text = S.Get("devices/writer_uncapped_warning"),
            Opacity = 0.8,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(warning, Ids.FolderWriterUncappedWarning);

        // The published-folder writer warning — independent of the cap (a byte cap
        // bounds the owner's quota, not what the writer can change for the people
        // outside the set), so only the picked access moves it.
        var published = new TextBlock
        {
            Opacity = 0.8,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(published, Ids.FolderWriterPublishedWarning);

        // Populate BEFORE wiring SelectionChanged (below) — the programmatic
        // SelectedItem set below would otherwise fire the handler and dispatch a
        // spurious commit at row-build time.
        roleSelect.Items.Clear();
        foreach (var option in uniffi.fauna_ffi.FaunaFfiMethods.MemberAccessOptions())
        {
            var item = new ComboBoxItem { Content = S.Resolve(option.@label), Tag = option.@value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, option.@value);
            roleSelect.Items.Add(item);
            if (option.@value == (access ?? "reader")) roleSelect.SelectedItem = item;
        }
        if (roleSelect.SelectedItem is null && roleSelect.Items.Count > 0) roleSelect.SelectedIndex = 0;

        string CurrentAccess() => roleSelect.SelectedItem is ComboBoxItem { Tag: string t } ? t : "reader";
        void UpdateWarning() => warning.Visibility =
            CurrentAccess() == "writer" && uniffi.fauna_ffi.FaunaFfiMethods.ParseCountI64(capInput.Text) is null
                ? Visibility.Visible : Visibility.Collapsed;
        void UpdatePublished()
        {
            var text = PublishedWriterWarning(fs, CurrentAccess());
            published.Text = text ?? string.Empty;
            published.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
        }
        UpdateWarning();
        UpdatePublished();

        roleSelect.SelectionChanged += async (_, _) =>
        {
            UpdateWarning();
            UpdatePublished();
            await CommitMemberAccessAsync(fs, actorIdHex, CurrentAccess(), capInput.Text);
        };
        capInput.LostFocus += async (_, _) =>
        {
            UpdateWarning();
            await CommitMemberAccessAsync(fs, actorIdHex, CurrentAccess(), capInput.Text);
        };
        // Commits on Enter too (not just blur), for the user who finishes typing
        // with the keyboard: Enter doesn't move focus off a single-line TextBox, so
        // the LostFocus handler above never observes it. The e2e driver reaches the
        // blur handler instead — its click on an editable with no stable clickable
        // point shifts focus to the nearest neighbour rather than injecting a
        // keystroke (flaui-bridge/Actions.cs's CommitEditable), keeping the
        // cross-app "click an editable to commit" contract (actions/backups.py's
        // set_member_cap, mirroring linux/tui's SpinButton-activate idiom) free of
        // SendInput. Both triggers are load-bearing; neither is test-only.
        capInput.KeyDown += async (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter) return;
            UpdateWarning();
            await CommitMemberAccessAsync(fs, actorIdHex, CurrentAccess(), capInput.Text);
        };

        accessRow.Children.Add(roleSelect);
        accessRow.Children.Add(capLabel);
        accessRow.Children.Add(capInput);

        var section = new StackPanel { Spacing = 2 };
        section.Children.Add(accessRow);
        section.Children.Add(warning);
        section.Children.Add(published);
        return section;
    }

    /// <summary>The <c>folder-writer-published-warning</c> copy for a grant of
    /// <paramref name="access"/> on <paramref name="fs"/>, or <c>null</c> when the folder
    /// reaches nobody outside its members (folders.md § Sharing). The reach test is the
    /// shared <c>writer_grant_reach</c> — <c>public</c> or paywalled ⇒ a writer changes
    /// what people OUTSIDE the set read (public ⇒ "anyone", paywalled ⇒ "subscribers") —
    /// never re-derived here, and fed the NORMALIZED audience exactly as the audience
    /// select is, so an unparseable column can never claim the set is world-readable.</summary>
    private static string? PublishedWriterWarning(FolderSummary fs, string access)
    {
        var reach = uniffi.fauna_ffi.FaunaFfiMethods.WriterGrantReach(
            access,
            uniffi.fauna_ffi.FaunaFfiMethods.NormalizeAudience(fs.audience, fs.mlsGroupId is not null),
            fs.webPaywallTier is not null);
        return reach is null ? null : S.Resolve(reach);
    }

    /// <summary>Commit a member's access grant (<c>fauna.folders.members.set_access</c>)
    /// — sends the FULL (access, cap) pair, blank cap text parsed via the shared
    /// <c>parse_count_i64</c> (null = uncapped). On success: refresh the page, which
    /// re-renders the expanded owner row (and the roster inside it) from the nest's
    /// persisted truth — proving the round trip, not an optimistic flip (mirrors
    /// RemoveSharedMemberAsync / ShareFolderAsync above).</summary>
    private async Task CommitMemberAccessAsync(FolderSummary fs, string actorIdHex, string access, string capText)
    {
        if (_rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_set_member_access", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        try
        {
            var byteCap = uniffi.fauna_ffi.FaunaFfiMethods.ParseCountI64(capText);
            await _rpc.FoldersSetMemberAccessAsync(fs.name, actorIdHex, access, byteCap);
            if (_machine is not null) await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    /// <summary>
    /// <c>folder-member-remove-button</c>: derive the set's <c>ChannelId</c> from
    /// its <c>mls_group_id</c> (<c>folder_channel_id_from_group_id</c> — the
    /// blake3 derive native apps can't reproduce client-side) and remove the
    /// member (rotates the content key for forward secrecy). On success: refresh the
    /// page.
    /// </summary>
    private async Task RemoveSharedMemberAsync(FolderSummary fs, string memberActorIdHex)
    {
        if (_convSession is null || _rpc is null)
        {
            ErrorBar.Message = S.Format("devices/error_remove_member", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // The group id must come from the set's CURRENT projection, not from the
        // summary this row closed over. That capture is genuinely null on the path
        // that shares an owner-only set and then removes the member without leaving
        // the row — the roster is painted by the share itself now, so `BuildMemberRow`
        // is reachable with a pre-share summary. This used to be `if (... is not { }
        // groupIdHex) return;` under a comment reasoning it "shouldn't happen"; the
        // comment was right about the old call graph and the bare `return` would have
        // turned the new one into a remove button that does nothing, silently
        // (e2e-conventions.md § point 11 — honour the gesture or fail loudly).
        var current = FreshFolder(fs.name) ?? fs;
        if (current.mlsGroupId is not { } groupIdHex)
        {
            ErrorBar.Message = S.Format("devices/error_remove_member", S.Get("errors/nest_unreachable"));
            ErrorBar.IsOpen = true;
            return;
        }
        // Captured into locals so the non-null state survives the await below.
        var session = _convSession;
        var rpc = _rpc;

        try
        {
            var channelId = uniffi.fauna_ffi.FaunaFfiMethods.FolderChannelIdFromGroupId(groupIdHex);
            var memberId = Convert.FromHexString(memberActorIdHex);
            await rpc.FoldersRemoveMemberAsync(session, fs.name, channelId, memberId);
            // Same as the share: the re-render re-opens this row and rebuilds its
            // roster, so the evicted member disappears and the "Shared · N" badge
            // drops without a second repaint here.
            if (_machine is not null) await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
    }

    // ── Nested local-folder binding (desktop) — contextual to the open set ──
    //
    // The binding is device-local config over the fauna-sync helper named pipe
    // (folders.md § Binding). Nested under each set, the set is contextual — binding
    // records only the folder path, so there is no free-text set-name field and no
    // per-row set-name display (the removed folder-location-fileset-input + folder-location-fileset).

    private FrameworkElement BuildLocationBindingSection(FolderSummary fs)
    {
        var setName = fs.name;
        var section = new StackPanel { Spacing = 6 };
        section.Children.Add(new TextBlock
        {
            Text = S.Get("folders/synced_locations"),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });

        // Bound-folder list. A Border carries the test id with a non-empty Name so an
        // empty list keeps a visible rectangle (the bare-id-root prune trap); the rows
        // live in a non-virtualizing StackPanel so FlaUI realizes every row.
        var rows = new StackPanel { Spacing = 4 };
        var empty = new TextBlock
        {
            Text = S.Get("folders/no_locations_bound"),
            Opacity = 0.5,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        var listInner = new StackPanel { Spacing = 4 };
        listInner.Children.Add(empty);
        listInner.Children.Add(rows);
        var listBorder = new Border
        {
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(6),
            Padding = new Thickness(12),
            Child = listInner,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(listBorder, Ids.FolderLocationList);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(listBorder, S.Get("folders/synced_locations"));

        // Add area: type (or Choose to fill) the local folder path, then Bind — the
        // folder binds to THIS set's context (setName), not a typed name. A Grid
        // with a single `*` column for the path input, NOT a horizontal StackPanel
        // — the same fix BuildPendingShareRow/BuildMemberHeaderGrid already needed:
        // a StackPanel gives an unconstrained TextBox all the width it asks for,
        // pushing the LAST child (bind, the only way to commit the gesture) past
        // the page's clip edge — present in the UIA tree (count=1) but IsOffscreen
        // (measured: on the writer-member row, where this section is the ENTIRE
        // expanded body with nothing else establishing a bounding width,
        // folder-location-add-button read visible=False while its sibling
        // folder-location-path-input, right next to it, read visible=True).
        var addRow = new Grid { ColumnSpacing = 8 };
        addRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        addRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        addRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var pathInput = new TextBox
        {
            PlaceholderText = S.Get("folders/location_path_placeholder"),
            MinWidth = 280,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(pathInput, Ids.FolderLocationPathInput);
        Grid.SetColumn(pathInput, 0);
        var browse = new Button { Content = S.Get("folders/choose") };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(browse, Ids.FolderLocationBrowseButton);
        browse.Click += (_, _) => BrowseFolder(pathInput);
        Grid.SetColumn(browse, 1);
        var bind = new Button
        {
            Content = S.Get("folders/bind_location"),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            Tag = fs,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(bind, Ids.FolderLocationAddButton);
        bind.Click += BindLocation_Click;
        Grid.SetColumn(bind, 2);
        addRow.Children.Add(pathInput);
        addRow.Children.Add(browse);
        addRow.Children.Add(bind);

        section.Children.Add(listBorder);
        section.Children.Add(addRow);

        _expandedLocationContainer = rows;
        _expandedLocationEmpty = empty;
        _expandedLocationInput = pathInput;
        RefreshExpandedLocations();
        return section;
    }

    // Re-filter the open set's bound folders out of the VM's flat list and rebuild the
    // nested rows. Called on first build and after every folder mutation (the VM re-lists
    // Locations → CollectionChanged).
    private void RefreshExpandedLocations()
    {
        if (_expandedLocationContainer is null || _expandedFolder is null || _syncVm is null) return;
        _expandedLocationContainer.Children.Clear();
        var mine = _syncVm.Locations
            .Where(f => string.Equals(f.Folder, _expandedFolder, StringComparison.Ordinal))
            .ToList();
        foreach (var row in mine)
            _expandedLocationContainer.Children.Add(BuildLocationRow(row));
        if (_expandedLocationEmpty is not null)
            _expandedLocationEmpty.Visibility = mine.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private FrameworkElement BuildLocationRow(LocationRowVm row)
    {
        // AutomationProperties.Name keeps the row in the UIA content view so FlaUI's
        // ByAutomationId resolves the child IDs (reference_winui_flaui_datatemplate_name).
        var grid = new Grid { ColumnSpacing = 8, RowSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var path = new TextBlock
        {
            Text = row.Path,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            IsTextSelectionEnabled = true,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(path, Ids.FolderLocationPath);
        Grid.SetColumn(path, 0);
        // NB: no folder-location-fileset — the set is contextual (you are inside its row).

        var rightControls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
        // Windows-only per-row on-demand/always toggle (cfapi placeholder host). HelpText
        // carries the mode so the e2e reads the toggle state via get_attr "state"
        // (reference_windows_flaui_state_attr_helptext).
        var toggle = new ToggleSwitch
        {
            Header = S.Get("devices/sync_locations/on_demand_label"),
            DataContext = row,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(toggle, Ids.FolderLocationModeToggle);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(toggle, row.Mode);
        // Set IsOn BEFORE attaching Toggled so the initial bind doesn't fire a render echo.
        toggle.IsOn = row.IsOnDemand;
        toggle.Toggled += LocationModeToggle_Toggled;
        var remove = new Button { Content = S.Get("devices/sync_locations/remove"), Tag = row.Path };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(remove, Ids.FolderLocationRemoveButton);
        remove.Click += RemoveLocation_Click;
        rightControls.Children.Add(toggle);
        rightControls.Children.Add(remove);
        Grid.SetColumn(rightControls, 1);

        grid.Children.Add(path);
        grid.Children.Add(rightControls);

        // The mass-delete floor's confirm affordance (delete-propagation.md § A
        // wholesale-vanished folder is infrastructure failure). Present ONLY while HasDeletesHeld — 0 renders NEITHER element, never a
        // zeroed line or a disabled button: a standing offer to destroy files over a
        // healthy folder is worse than no affordance at all, and 0 is the only thing that
        // ever retracts a displayed hold (the hold is derived per reconcile pass, never
        // stored). Same row (Grid.Row 1, spanning both columns) as the path/controls above.
        if (row.HasDeletesHeld)
        {
            var heldPanel = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
            var heldText = new TextBlock
            {
                Text = row.DeletesHeldText,
                TextWrapping = TextWrapping.Wrap,
                Opacity = 0.8,
                VerticalAlignment = VerticalAlignment.Center,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(heldText, Ids.FolderLocationDeletesHeld);
            var applyButton = new Button { Content = row.ApplyDeletesText, Tag = row.Folder };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(applyButton, Ids.FolderLocationApplyDeletesButton);
            applyButton.Click += ApplyHeldDeletes_Click;
            heldPanel.Children.Add(heldText);
            heldPanel.Children.Add(applyButton);
            Grid.SetRow(heldPanel, 1);
            Grid.SetColumn(heldPanel, 0);
            Grid.SetColumnSpan(heldPanel, 2);
            grid.Children.Add(heldPanel);
        }

        // The delete rail's unreadable-path line (delete-propagation.md § Unreadable is
        // not absent): part of the folder could not
        // be read, so nothing was changed there and that subtree stopped syncing.
        // Deliberately a bare line with NO button — there is nothing to confirm, and an
        // apply verb here would be the bug; the remedy (permissions, the mount) is outside
        // the app. Independent of HasDeletesHeld: a row can show either, both, or neither.
        if (row.HasUnreadable)
        {
            var unreadableText = new TextBlock
            {
                Text = row.UnreadableText,
                TextWrapping = TextWrapping.Wrap,
                Opacity = 0.8,
                VerticalAlignment = VerticalAlignment.Center,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(unreadableText, Ids.FolderLocationUnreadable);
            Grid.SetRow(unreadableText, 2);
            Grid.SetColumn(unreadableText, 0);
            Grid.SetColumnSpan(unreadableText, 2);
            grid.Children.Add(unreadableText);
        }

        var border = new Border
        {
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(6),
            Padding = new Thickness(12),
            Child = grid,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(border, Ids.FolderLocationRow);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(border, row.Path);
        return border;
    }

    /// <summary>Open the OS folder picker and fill the set's folder-location-path-input. The
    /// picker is not e2e-driveable, so the typed path field is the source of truth.</summary>
    private async void BrowseFolder(TextBox target)
    {
        if (App.MainWindow is null) return;
        try
        {
            var picker = new Windows.Storage.Pickers.FolderPicker();
            picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.Desktop;
            picker.FileTypeFilter.Add("*");

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var folder = await picker.PickSingleFolderAsync();
            if (folder is not null) target.Text = folder.Path;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("FoldersPage", $"folder pick failed: {ex.Message}");
        }
    }

    /// <summary>Bind the typed folder to THIS set (the set is contextual — no typed name).
    /// Resolves the set's <c>FolderRef</c> wire identity from the row already rendered
    /// (on-demand-files.md § Hosting multiple on-demand folders — a name alone can't say
    /// which of two same-named sets a folder belongs to); a row that yields none is refused
    /// on the error line, never bound by name. AddLocationAsync re-lists Locations →
    /// CollectionChanged → RefreshExpandedLocations.</summary>
    private async void BindLocation_Click(object sender, RoutedEventArgs e)
    {
        if (_syncVm is null || sender is not Button b || b.Tag is not FolderSummary fs) return;
        var path = _expandedLocationInput?.Text?.Trim();
        if (string.IsNullOrEmpty(path)) return;
        var folderId = uniffi.fauna_ffi.FaunaFfiMethods.FolderRefForRow(fs.id, fs.mlsGroupId, fs.homeNestUrl);
        await _syncVm.AddLocationAsync(path, fs.name, folderId);
        if (_expandedLocationInput is not null) _expandedLocationInput.Text = string.Empty;
    }

    /// <summary>Toggle a row's on-demand/always mode. Guards the render echo: a toggle
    /// whose new value already matches the row's mode is the binding setting it (not a
    /// user gesture) and is a no-op.</summary>
    private async void LocationModeToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncVm is null || sender is not ToggleSwitch t || t.DataContext is not LocationRowVm row) return;
        var wantOnDemand = t.IsOn;
        if (wantOnDemand == row.IsOnDemand) return; // render echo, not a user toggle
        await _syncVm.SetModeAsync(row.Path, wantOnDemand ? "on-demand" : "always");
    }

    private async void RemoveLocation_Click(object sender, RoutedEventArgs e)
    {
        if (_syncVm is null || sender is not Button b || b.Tag is not string path) return;
        await _syncVm.RemoveLocationAsync(path);
    }

    /// <summary>Apply a folder's held deletes (delete-propagation.md § A wholesale-vanished
    /// folder is infrastructure failure). Tag carries the FOLDER (the set), not the local
    /// path — the agent's ListEngines roster keys on the set, and the same held count is
    /// shared by every path bound to it.</summary>
    private async void ApplyHeldDeletes_Click(object sender, RoutedEventArgs e)
    {
        if (_syncVm is null || sender is not Button b || b.Tag is not string folder) return;
        await _syncVm.ApplyHeldDeletesAsync(folder);
    }

    // ── Conflicts (REVIEW LIST — auto-resolve, file-sync.md § Conflicts) ──
    //
    // Conflicts auto-resolve on the detecting device (clean text three-way merge,
    // else latest-wins; the losing version is always retained as a file version),
    // so nothing here blocks on the user. A resolved row shows the resolution
    // (conflict-type-badge), the file + winning head (conflict-file-info), and a
    // one-tap "use the other version" (conflict-resolve-button) that re-points
    // the file at the retained loser via DevicesMachine.UseOtherVersion (the
    // File-Versions restore record — itself reversible). A still-unresolved row
    // (the sync engine's report when its local-version upload fails) renders informationally —
    // the detecting device resolves it; no blocking chooser here.

    private void RenderConflicts(IReadOnlyList<ConflictSummary> conflicts)
    {
        ConflictsContainer.Children.Clear();
        foreach (var c in conflicts)
            ConflictsContainer.Children.Add(BuildConflictRow(c));
        ConflictsContainer.Visibility = conflicts.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        NoConflictsText.Visibility = conflicts.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private FrameworkElement BuildConflictRow(ConflictSummary c)
    {
        // AutomationProperties.Name keeps the row in the UIA content view so FlaUI's
        // ByAutomationId resolves the child IDs (same trick as device-card).
        var resolved = c.resolvedAt is not null;
        var panel = new StackPanel { Spacing = 4, Padding = new Thickness(8) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(panel, c.fileInfo);

        var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        // Badge: the resolution on a review row; the conflict type on an
        // unresolved row.
        var badgeText = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ConflictBadgeLabel(c.resolution, c.resolvedAt, c.conflictType));
        var badge = new TextBlock { Text = badgeText, FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(badge, Ids.ConflictTypeBadge);
        var info = new TextBlock { Text = c.fileInfo, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(info, Ids.ConflictFileInfo);
        header.Children.Add(badge);
        header.Children.Add(info);
        panel.Children.Add(header);

        if (c.hasOtherVersion)
        {
            // Auto-resolved with a retained loser: the one-tap re-point.
            var btn = new Button { Content = S.Get("devices/conflicts/use_other_version"), Tag = c.id };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(btn, Ids.ConflictResolveButton);
            btn.Click += UseOtherVersion_Click;
            panel.Children.Add(btn);
        }
        else if (!resolved)
        {
            // Unresolved report — informational only (resolution happens on
            // the detecting device).
            panel.Children.Add(new TextBlock
            {
                Text = S.Get("devices/conflicts/awaiting_device"),
                Opacity = 0.6,
                FontSize = 12,
            });
        }
        // Resolved with nothing to re-point at (candidate-free (mark-only) resolve): nothing
        // actionable; version history is the finer surface.
        return panel;
    }

    private async void UseOtherVersion_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || sender is not Button b || b.Tag is not long id) return;
        // The restore record is attributed to this device (same device-id glue as
        // the Media restore).
        var deviceId = _account?.DeviceId;
        if (string.IsNullOrEmpty(deviceId)) return;
        await _machine.UseOtherVersion(id, deviceId!);
    }

    // ── Folder creation wizard (embedded FolderWizardMachine) ──
    //
    // The page renders the wizard off DevicesMachine.Wizard() (folders.md § Where logic
    // lives / § Don't do these — no wizard logic client-side).

    private async void FolderAdd_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        // A wizard is already open or opening on this page (a rapid second click): the
        // preparation below awaits before the gate is reached, and re-opening the
        // machine's wizard would reset the open one's state.
        if (_wizardDialogShown) return;

        // Static labels (set once; safe to repeat).
        WizardNameBox.PlaceholderText = S.Get("devices/wizard/name_placeholder");
        WizardDevicesHeading.Text = S.Get("devices/wizard/select_devices_roles");
        WizardNoDevicesText.Text = S.Get("devices/wizard/no_devices_available");
        WizardBackBtn.Content = S.Get("devices/wizard/back");
        WizardNextBtn.Content = S.Get("devices/wizard/next");
        WizardCreateBtn.Content = S.Get("devices/wizard/create");

        // Open the embedded wizard (seeded with the machine's current device list).
        // OpenWizard ticks the observer (enqueued → runs after this method returns), so
        // building the fixed-shape children here first is safe.
        _machine.OpenWizard();
        if (Wiz is null) return;
        // Inject the user's global default conflict policy (fauna.state.sync-prefs)
        // so Submit stamps it onto the create — the client-glue half of the Sync
        // defaults contract (file-sync.md § Conflicts, policy). Best-effort: an
        // unreadable config leaves the wizard un-injected (new set lands on the
        // column default, auto).
        if (_rpc is not null)
        {
            try
            {
                var policy = await _rpc.DefaultConflictPolicyGetAsync();
                if (policy is not null) Wiz?.SetDefaultConflictPolicy(policy);
            }
            catch { /* best-effort */ }
        }
        BuildWizardDeviceRows();

        WizardErrorText.Visibility = Visibility.Collapsed;
        WizardDialog.XamlRoot = this.XamlRoot;
        WizardDialog.Title = S.Get("devices/wizard/new_folder");
        _wizardDialogShown = true;
        RenderWizardSteps();
        // Through the gate: a later test's reset() force-closes it if the test left it
        // open, and another dialog already up refuses this open legibly instead of
        // crashing (Controls.Dialogs states the policy).
        await Controls.Dialogs.ShowAsync(WizardDialog);

        // The show returns when the dialog closes (Done hid it programmatically, the
        // user cancelled, a reset() Hid it) or at once when the gate refused it. Drop
        // the wizard if still open so the embedded snapshot clears. Guarded: a reset()-driven Hide resumes this
        // continuation after the page was navigated away, so the machine tick must not
        // throw out of this async void handler.
        _wizardDialogShown = false;
        try { _machine.CloseWizard(); } catch { }
    }

    // ── Wizard gestures (forward to the embedded machine) ──

    private void WizardName_Changed(object sender, TextChangedEventArgs e)
    {
        if (_suppressWizardEvents) return;
        Wiz?.SetName(WizardNameBox.Text ?? "");
    }

    // (The wizard-mode-* gestures that sat here are RETIRED — a folder has no type,
    // folders re-model phase 2 slice e; the machine creates every folder `sync`.)
    private void WizardNext_Click(object sender, RoutedEventArgs e) => Wiz?.Next();
    private void WizardBack_Click(object sender, RoutedEventArgs e) => Wiz?.Back();

    private async void WizardCreate_Click(object sender, RoutedEventArgs e)
    {
        if (Wiz is null) return;
        // submit() ticks the observer (Submitting → Done/Failed), so RenderWizardSteps
        // drives the UI — including closing on Done. Failures stay on Review with the
        // structured error surfaced by RefreshReview.
        try { await Wiz.Submit(); }
        catch (Exception ex)
        {
            WizardErrorText.Text = Strings.Error(ex);
            WizardErrorText.Visibility = Visibility.Visible;
        }
    }

    // ── Wizard render (off the embedded machine) ──

    private void RenderWizard(FolderWizardSnapshot? wizard)
    {
        if (wizard is null)
        {
            if (_wizardDialogShown) { _wizardDialogShown = false; WizardDialog.Hide(); }
            return;
        }
        if (_wizardDialogShown) RenderWizardSteps();
    }

    private void RenderWizardSteps()
    {
        if (Wiz is null) return;
        var step = Wiz.Step();

        if (step == FolderWizardStep.Done)
        {
            // Created — close + refresh the page (the folder list picks it up).
            // ⚠ Clear the shown flag HERE, not only when FolderAdd_Click's ShowAsync
            // resumes: the refresh below re-renders this page, and while the flag
            // still read true every render landed back on this Done branch and
            // refreshed again — a self-sustaining refresh loop that outlived the
            // page.
            _wizardDialogShown = false;
            WizardDialog.Hide();
            _ = _machine?.Refresh();
            return;
        }

        _suppressWizardEvents = true;
        try
        {
            WizardStep1.Visibility = step == FolderWizardStep.Name ? Visibility.Visible : Visibility.Collapsed;
            WizardStep2.Visibility = step == FolderWizardStep.Devices ? Visibility.Visible : Visibility.Collapsed;
            WizardStep3.Visibility = step == FolderWizardStep.Review ? Visibility.Visible : Visibility.Collapsed;

            switch (step)
            {
                case FolderWizardStep.Name: RefreshName(); break;
                case FolderWizardStep.Devices: RefreshDevicePlaces(); break;
                case FolderWizardStep.Review: RefreshReview(); break;
            }

            WizardBackBtn.Visibility = step == FolderWizardStep.Name ? Visibility.Collapsed : Visibility.Visible;
            var onNav = step is FolderWizardStep.Name or FolderWizardStep.Devices;
            WizardNextBtn.Visibility = onNav ? Visibility.Visible : Visibility.Collapsed;
            if (onNav)
            {
                WizardNextBtn.IsEnabled = step != FolderWizardStep.Name
                    || Wiz.NameSnapshot().continueEnabled;
            }
            var onReview = step == FolderWizardStep.Review;
            WizardCreateBtn.Visibility = onReview ? Visibility.Visible : Visibility.Collapsed;
            if (onReview) WizardCreateBtn.IsEnabled = Wiz.ReviewSnapshot().createEnabled;
        }
        finally { _suppressWizardEvents = false; }
    }

    // ── Wizard step 1: the name alone (the type step is retired — a folder has no type) ──

    private void RefreshName()
    {
        var snap = Wiz!.NameSnapshot();
        if (WizardNameBox.Text != snap.name) WizardNameBox.Text = snap.name;
    }

    // ── Wizard step 2: per-device enrollment + place flags ──
    //
    // The `wizard-device-role` Source/Sync/Backup/Mirror picker is RETIRED (folders
    // re-model phase 2 slice e): a seat's place is three flags, each painted as a
    // checkbox that says what it does, with its own one-line explainer. Every point the
    // three boxes span is sendable (slice f) — there is no refusal to render.

    private void BuildWizardDeviceRows()
    {
        WizardDeviceContainer.Children.Clear();
        _wizardDeviceChecks.Clear();
        _wizardDeviceFlagBoxes.Clear();

        var snap = Wiz!.DevicePlacesSnapshot();
        var devices = snap.devices;
        WizardNoDevicesText.Visibility = devices.Length == 0 ? Visibility.Visible : Visibility.Collapsed;

        for (int i = 0; i < devices.Length; i++)
        {
            int index = i; // capture for the closures
            var seat = new StackPanel { Spacing = 4 };

            // The device's label IS the checkbox's content, so the box names the device it
            // enrolls — to a screen reader and to UIA alike. A bare box beside a separate
            // TextBlock read back as an unnamed "check" (the wizard-places witness found
            // four indistinguishable '' boxes on windows, 2026-09-25).
            var check = new CheckBox { Content = devices[i].label };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(check, Ids.WizardDeviceCheck);
            check.Checked += (_, _) => OnWizardDeviceToggle(index, true);
            check.Unchecked += (_, _) => OnWizardDeviceToggle(index, false);
            seat.Children.Add(check);

            // The three place-flag boxes, one per flag, each followed by its explainer
            // — the pattern the retired role picker used per seat, now per box, because
            // the flags are the thing being explained. Rendered for EVERY seat, enrolled
            // or not (tui, the lead app, and linux/web/apple do the same).
            var boxes = new CheckBox[PlaceFlagBoxes.Length];
            for (int k = 0; k < PlaceFlagBoxes.Length; k++)
            {
                var (id, labelKey, descKey, kind) = PlaceFlagBoxes[k];
                var flagBox = new CheckBox
                {
                    Content = S.Get(labelKey),
                    Margin = new Thickness(24, 0, 0, 0),
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(flagBox, id);
                flagBox.Checked += (_, _) => OnWizardFlagToggle(index, kind, true);
                flagBox.Unchecked += (_, _) => OnWizardFlagToggle(index, kind, false);
                seat.Children.Add(flagBox);
                seat.Children.Add(new TextBlock
                {
                    Text = S.Get(descKey),
                    Opacity = 0.6,
                    TextWrapping = TextWrapping.Wrap,
                    Margin = new Thickness(48, 0, 0, 4),
                });
                boxes[k] = flagBox;
            }

            _wizardDeviceChecks.Add(check);
            _wizardDeviceFlagBoxes.Add(boxes);
            WizardDeviceContainer.Children.Add(seat);
        }
    }

    private void OnWizardDeviceToggle(int index, bool want)
    {
        if (_suppressWizardEvents || Wiz is null) return;
        var devices = Wiz.DevicePlacesSnapshot().devices;
        if (index < devices.Length && devices[index].selected != want)
            Wiz.ToggleDeviceMember((uint)index);
    }

    // One box flipped: send the seat's WHOLE triple with that one flag replaced — the
    // shape `set_device_flags` takes, which is whole-value like the nest write. A flip
    // that matches the machine's current value is a refresh echo, not a gesture.
    private void OnWizardFlagToggle(int index, PlaceFlagKind kind, bool on)
    {
        if (_suppressWizardEvents || Wiz is null) return;
        var devices = Wiz.DevicePlacesSnapshot().devices;
        if (index >= devices.Length) return;
        var d = devices[index];
        if (ReadFlag(d, kind) == on) return;
        var (originates, accepts, appliesDeletes) = kind switch
        {
            PlaceFlagKind.Originates => (on, d.accepts, d.appliesDeletes),
            PlaceFlagKind.Accepts => (d.originates, on, d.appliesDeletes),
            _ => (d.originates, d.accepts, on),
        };
        Wiz.SetDeviceFlags((uint)index, originates, accepts, appliesDeletes);
    }

    private void RefreshDevicePlaces()
    {
        var devices = Wiz!.DevicePlacesSnapshot().devices;
        for (int i = 0; i < devices.Length; i++)
        {
            // HelpText carries the literal "on"/"off" the cross-app get_attr(id, "state")
            // contract reads — the same channel the row's place editor uses
            // (PlaceBox); a box without it read back as None.
            if (i < _wizardDeviceChecks.Count)
            {
                if (_wizardDeviceChecks[i].IsChecked != devices[i].selected)
                    _wizardDeviceChecks[i].IsChecked = devices[i].selected;
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                    _wizardDeviceChecks[i], devices[i].selected ? "on" : "off");
            }
            if (i >= _wizardDeviceFlagBoxes.Count) continue;
            var boxes = _wizardDeviceFlagBoxes[i];
            for (int k = 0; k < boxes.Length && k < PlaceFlagBoxes.Length; k++)
            {
                bool want = ReadFlag(devices[i], PlaceFlagBoxes[k].Kind);
                if (boxes[k].IsChecked != want) boxes[k].IsChecked = want;
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(boxes[k], want ? "on" : "off");
            }
        }
    }

    // ── Wizard step 3: read-only review ──
    // (The scan-frequency step that used to precede it — and the backup-type
    // retention inputs it carried — retired with folders re-model phase 5; the
    // cadence is a constant and retention is the nest place's per-folder policy.)

    private void RefreshReview()
    {
        var snap = Wiz!.ReviewSnapshot();
        WizardReviewContainer.Children.Clear();
        AppendReviewRow(S.Get("devices/wizard/review_name"), snap.name);
        // No mode line, no retention line, no cadence line: a folder has no type
        // (phase 2 slice e), retention is the nest place's policy edited on the row
        // rather than chosen at create, and the scan cadence is a constant (phase 5).
        // The enrolled list names devices, not roles — a seat's place is three flags,
        // which one review noun cannot summarize honestly.
        var devicesText = snap.enrolled.Length == 0
            ? S.Get("devices/wizard/review_no_devices")
            : string.Join(", ", snap.enrolled.Select(d => d.label));
        AppendReviewRow(S.Get("devices/wizard/review_devices"), devicesText);

        if (snap.error is { } err)
        {
            WizardErrorText.Text = S.Resolve(err);
            WizardErrorText.Visibility = Visibility.Visible;
        }
        else
        {
            WizardErrorText.Visibility = Visibility.Collapsed;
        }
    }

    private void AppendReviewRow(string key, string value)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        row.Children.Add(new TextBlock { Text = key, Opacity = 0.6, Width = 110 });
        row.Children.Add(new TextBlock { Text = value, TextWrapping = TextWrapping.Wrap });
        WizardReviewContainer.Children.Add(row);
    }
}
