using System.Collections.Generic;
using System.Linq;
using Windows.System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Root navigation page with sidebar. Hosts a Frame that navigates
/// between Feed, Conversations, Contacts, Sync, Backups, Media, Search,
/// Conflicts, Settings, and Status.
/// </summary>
public sealed partial class MainPage : Page
{
    private MainViewModel? _viewModel;
    private ServiceClients? _clients;

    // The sub-page id requested by NavigateToSettingsSubPage, applied inside
    // NavView_SelectionChanged's "Settings" branch — never by an immediate
    // post-assignment check in the caller. See NavigateToSettingsSubPage's own
    // comment for why.
    private string? _pendingSettingsSubId;

    // ~10s local sync-agent process-health poll (sync-agent.md § Local agent
    // health) — mirrors linux's SYNC_AGENT_STATUS_POLL interval and EventsPage's
    // DispatcherTimer split (the VM can't reference DispatcherTimer directly).
    //
    // ⚠ MainPage is per-LOGIN, NOT app-lifetime. Every sign-out, account switch and
    // e2e reset() navigates the root frame to OnboardingPage, and the next login
    // builds a BRAND-NEW MainPage — so "constructed once per login" is precisely what
    // makes an app-lifetime assumption wrong here. A running DispatcherTimer is rooted
    // by the dispatcher, so a page left un-stopped never dies: it pins the detached
    // MainPage forever and keeps polling the agent pipe. Because
    // NamedPipeClientStream.ConnectAsync wraps a BLOCKING connect in Task.Run, each
    // leaked poller parks a thread-pool thread for up to its timeout every 10s; after a
    // handful of logins the pool starves, WS-RPC continuations stop being serviced, and
    // the app stops responding to UIA and to test-agent post-actions. That was the
    // "second family journey" flake: 8 logins → 7 leaked pollers → state
    // pushes drifting from ~1s to 28s. OnNavigatedFrom stops it — same contract as
    // EventsPage/FeedPage, the convention this page had drifted from.
    private const int SyncAgentStatusPollIntervalSecs = 10;
    private DispatcherTimer? _syncAgentStatusPollTimer;

    // Guardian Notify flush tick (family-safety.md § Guardian Notify) — same
    // per-LOGIN lifetime and OnNavigatedFrom-stop contract as
    // _syncAgentStatusPollTimer directly above; see that field's comment for why
    // an un-stopped timer here would starve the process. A short interval is
    // safe because the real cadence gate is GuardianNotifyCache's own
    // ≤hourly notify_report_min_interval_secs check, not this tick.
    private const int GuardianNotifyFlushPollIntervalSecs = 10;
    private DispatcherTimer? _guardianNotifyFlushTimer;

    // Screen-time production tick (family-safety.md § Screen time, Slice E) —
    // same per-LOGIN lifetime and OnNavigatedFrom-stop contract as
    // _syncAgentStatusPollTimer above. ScreenTimeCache.TickIntervalSecs (60s)
    // matches the shared engine's own accrual-step requirement of a caller
    // (MAX_ACCRUAL_STEP_SECS is two minutes, so a slower tick would silently
    // under-count) — unlike the two timers above, this interval IS the real
    // cadence gate, not just a flush-check frequency.
    private DispatcherTimer? _screenTimeTickTimer;

    /// <summary>
    /// Number of sync-agent status pollers currently running across ALL MainPage
    /// instances. Exactly one live MainPage owns the poll at a time, so this is 1
    /// while signed in and 0 while signed out; anything higher means a detached page
    /// kept its timer (the leak above). Surfaced in the e2e state protocol
    /// (<c>diagnostics.live_sync_agent_pollers</c>) so the regression is assertable
    /// headlessly instead of only as a downstream timing flake.
    /// </summary>
    internal static int LiveSyncAgentPollers;

    /// <summary>
    /// Singleton accessor so the test agent command handler in App.xaml.cs
    /// can drive tab navigation without a direct reference to the page instance.
    /// </summary>
    public static MainPage? Current { get; private set; }

    /// <summary>
    /// The FeedPage currently navigated-to in the main content frame, if any.
    /// Used by the compose.file state-protocol branch in App.xaml.cs to push
    /// an attachment into the active feed composer for E2E image-upload tests.
    /// Set/cleared by FeedPage on navigate-in/navigate-out.
    /// </summary>
    internal FeedPage? ActiveFeedPage { get; set; }

    /// <summary>
    /// Maps state-protocol view names (lowercase) to NavigationView tag strings (PascalCase).
    /// </summary>
    private static readonly Dictionary<string, string> ViewToTag = new()
    {
        ["feed"] = "Feed",
        ["conversations"] = "Conversations",
        ["contacts"] = "Contacts",
        ["profile"] = "Profile",
        ["events"] = "Calendar",
        ["bridges"] = "Bridges",
        ["backups"] = "Backups",
        ["media"] = "Media",
        ["search"] = "Search",
        ["moderation"] = "Moderation",
        ["notifications"] = "Notifications",
        ["settings"] = "Settings",
        // "status" has no standalone tab anymore — it is folded into the Settings
        // shell as the default sub-page. App.xaml.cs's nav-stack dispatch routes
        // {view:"status"} to NavigateToSettingsSubPage("status") before NavigateToView
        // is ever reached, so no "status" tag mapping is needed here.
        //
        // The former top-level "sync"/"devices" (→ SyncPage) and "conflicts" (→
        // ConflictsPage) views were RETIRED by the 2026-06-28 sync/folder UI
        // unification: the roster lives on Settings → Devices, the folder control
        // plane + conflicts on Settings → Folders. The e2e reaches them via the
        // two-element settings nav ({view:settings},{view:settings,id:devices|folders}).
        // The admin section is a single gated entry → the distinct AdminShellPage
        // (admin.md § Navigation model). The shell's per-page sub-nav is driven by
        // NavigateToAdminSubPage, not these flat per-page tags.
        ["admin"] = "Admin",
        // family-tab is a gated entry (visible only when fauna.family.status
        // returns a relationship — family-safety.md § App surface); the map
        // entry itself is unconditional, same as "admin", so the state-protocol
        // nav (driver.navigate_to("family")) reaches FamilyPage whenever the tab
        // is actually present.
        ["family"] = "Family",
    };

    /// <summary>
    /// Reverse map: NavigationView tag (PascalCase) → state-protocol view name (lowercase).
    /// Built from ViewToTag, skipping one-way aliases like "devices".
    /// </summary>
    private static readonly Dictionary<string, string> TagToView = new()
    {
        ["Feed"] = "feed",
        ["Conversations"] = "conversations",
        ["Contacts"] = "contacts",
        ["Profile"] = "profile",
        ["Calendar"] = "events",
        ["Bridges"] = "bridges",
        ["Backups"] = "backups",
        ["Media"] = "media",
        ["Search"] = "search",
        ["Moderation"] = "moderation",
        ["Notifications"] = "notifications",
        ["Settings"] = "settings",
        // No "Status" tag — status is the Settings shell's default sub-page (folded
        // in 2026-06-04). While the shell is shown the selected tag is "Settings",
        // so CurrentViewName reports "settings".
        ["Admin"] = "admin",
        ["Family"] = "family",
    };

    /// <summary>
    /// Returns the current view as a state-protocol name (lowercase).
    /// Falls back to "feed" if nothing is selected.
    /// </summary>
    public string CurrentViewName
    {
        get
        {
            if (NavView.SelectedItem is NavigationViewItem item && item.Tag is string tag)
            {
                return TagToView.TryGetValue(tag, out var view) ? view : tag.ToLowerInvariant();
            }
            return "feed";
        }
    }

    /// <summary>
    /// Navigate to a view by its state-protocol name (lowercase).
    /// Must be called on the UI thread.
    /// </summary>
    public void NavigateToView(string viewName)
    {
        if (ViewToTag.TryGetValue(viewName, out var tag))
        {
            NavigateToTag(tag);
        }
    }

    public MainPage()
    {
        this.InitializeComponent();

        // Ctrl+Comma → Settings (VK_OEM_COMMA = 188, not in VirtualKey enum)
        var settingsAccel = new KeyboardAccelerator
        {
            Modifiers = VirtualKeyModifiers.Control,
            Key = (VirtualKey)188,
        };
        settingsAccel.Invoked += SettingsAccelerator_Invoked;
        KeyboardAccelerators.Add(settingsAccel);
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        Current = this;
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
            // THE defined point at which the RPC seam learns the UI
            // SynchronizationContext every `Raise…OnUi` marshals through. Here
            // because this is the one place BOTH login paths — production's
            // StartMainAppAsync and the TestAgent `set_state` login — reach on the
            // UI thread; before it, the context was whichever pump happened to
            // start first, and under the e2e login that is a THREAD-POOL thread
            // . Ordered before the pumps' own
            // best-effort fallbacks matter, and idempotent besides.
            clients.Rpc?.CaptureUiContext();
            // Resolve the agent session per probe, not here: this shell VM is built once
            // per login while the session comes and goes across sign-out/account switch.
            _viewModel = new MainViewModel(
                clients.Rpc!,
                new AgentStatusProbe(() => App.CurrentSyncAgent?.Channel));
            DataContext = _viewModel;
            // The screen-time lock overlay must react to every ContentFrame
            // navigation, not only to a fauna.family.status read or the 60s
            // tick — leaving/entering the Family page while locked has to
            // update the overlay immediately (family-safety.md § Screen time,
            // the "Family page stays reachable" invariant).
            ContentFrame.Navigated += ContentFrame_Navigated;
            // Re-fire the family-status read on WS reconnect (family-safety.md
            // § Content policy, clause 1 — "refresh fires at cold launch and
            // on WS reconnect"): today's single Page_Loaded read never re-fires,
            // so a guardian's policy edit only bound at the ward's NEXT login.
            // Keep-on-failure already lives inside CheckFamilyStatusAsync's own
            // catch (unchanged), so a reconnect blip cannot clear an
            // already-restored floor — only a SUCCESSFUL re-read ever moves it.
            clients.Rpc!.Reconnected += OnRpcReconnected;
            // Mirror the shell VM's app-global unread-notification count into the
            // e2e state snapshot (`data.notifications.unread_count`). Published from
            // the View, like every other AppDataSnapshot writer (ContactsPage,
            // EventsPage, FeedPage, FoldersPage) — FaunaApp.Core must not reach into
            // the app shell's services. The SHELL is what publishes it, not
            // NotificationsPage, because the count has to move on a push that lands
            // while the user is somewhere else entirely — which is the case
            // `test_nest_flip_resilience` asserts, sitting on the feed
            // .
            _viewModel.PropertyChanged += MainViewModel_PropertyChanged;
        }
    }

    /// <summary>
    /// Re-read the app-global unread-notification count. Called by
    /// <see cref="NotificationsPage"/> after a load or a mark-all-read so the shell's
    /// count — and the <c>data.notifications.unread_count</c> snapshot it publishes —
    /// tracks a mutation the page made, WITHOUT a second writer racing the same field.
    /// One writer (this shell), one source (`fauna.notifications.count`).
    /// </summary>
    public void RefreshUnreadNotificationCount()
        => _ = _viewModel?.RefreshUnreadNotificationCountAsync();

    private void MainViewModel_PropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        if (e.PropertyName == nameof(MainViewModel.UnreadNotificationCount))
        {
            Services.AppDataSnapshot.SetUnreadCount((int)_viewModel.UnreadNotificationCount);
        }
    }

    private void OnRpcReconnected()
    {
        // The witness that a WS reconnect actually reached the app's view layer.
        // Worth a permanent trace line because its ABSENCE is invisible: this
        // handler and every `ViewModelBase.RefreshOnReconnect` re-fetch hang off
        // the same `INestRpcClient.Reconnected`, so when the pump behind it is
        // not running the whole reconnect re-hydrate silently does nothing and
        // every downstream assertion fails somewhere else entirely. That is the
        // shape row 134 cost two sessions to find .
        FaunaApp.Core.Logs.E2eTrace.Write(
            $"[nav] OnRpcReconnected content={ContentFrame.Content?.GetType().Name} "
            + $"selected={(NavView.SelectedItem as NavigationViewItem)?.Tag}");
        _ = CheckFamilyStatusAsync();
        // The region relay is asked at every reconnect too, like the apple leg.
        StartRegionRefresh();
    }

    private void ContentFrame_Navigated(object sender, NavigationEventArgs e)
        => UpdateScreenTimeLockOverlay();

    /// <summary>
    /// Toggle the global screen-time-lock overlay (family-safety.md § Screen
    /// time): visible whenever <see cref="ScreenTimeCache.LockMessage"/> is
    /// non-null AND the content frame is NOT currently on <see
    /// cref="FamilyPage"/> — the goal-doc invariant that the Family page
    /// stays reachable read-only while locked.
    /// </summary>
    private void UpdateScreenTimeLockOverlay()
    {
        var message = ScreenTimeCache.LockMessage;
        var onFamilyPage = ContentFrame.Content is FamilyPage;
        if (message is not null && !onFamilyPage)
        {
            ScreenTimeLockMessageText.Text = message;
            ScreenTimeLockOverlay.Visibility = Visibility.Visible;
        }
        else
        {
            ScreenTimeLockOverlay.Visibility = Visibility.Collapsed;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] enter");
        if (_viewModel is not null)
        {
            await _viewModel.LoadCommand.ExecuteAsync(null);
        }
        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] after LoadCommand");

        // Default to the first item ONLY if nothing is selected yet. A
        // state-protocol nav (e.g. admin_app's {view:"admin"} → NavigateToAdminSubPage)
        // can select a page during the same Frame.Navigate, before this Loaded
        // handler runs; clobbering it back to item[0] would defeat all admin /
        // deep-link navigation (the page would flip to Feed right after landing).
        if (NavView.SelectedItem is null && NavView.MenuItems.Count > 0)
        {
            NavView.SelectedItem = NavView.MenuItems[0];
        }

        // Restore the last-known supervision snapshot BEFORE any live status
        // read lands (family-safety.md § Content policy, clause 2) — must run
        // ahead of CheckAdminStatusAsync too, since that's the first `await`
        // to yield control back to the message loop on this page.
        RestoreSupervisionSnapshot();

        // Check admin status and show admin nav items if user is admin
        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] before CheckAdminStatusAsync");
        await CheckAdminStatusAsync();
        // Check family-safety status and show the gated family-tab / global
        // supervised-indicator as appropriate (family-safety.md § App surface).
        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] before CheckFamilyStatusAsync");
        await CheckFamilyStatusAsync();
        // Content-policy hydration, own-thresholds half (moderation.md §
        // Categories & enforcement item 1). The OTHER half rides
        // CheckFamilyStatusAsync above; this one needs its own read, and an
        // UNSUPERVISED viewer has ONLY this half — so it must not be gated on
        // family status. Fire-and-forget: nothing below depends on it, and a
        // slow/failed spam-prefs read must never delay or break Page_Loaded.
        StartContentPolicyOwnThresholdHydration();
        // The region content plane's relay ask — the third render source
        // (region-blocking.md § How an app obtains its region's policy). Fire-and-
        // forget for the same reason as the line above.
        StartRegionRefresh();

        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] before StartSyncAgentStatusPoll");
        StartSyncAgentStatusPoll();
        StartGuardianNotifyFlushPoll();
        StartScreenTimeTick();
        // A fauna:// route this process was launched with (the Explorer Share
        // leaf's hand-off — windows.md § Shell Extension → The Share hand-off):
        // applied now that the shell exists. Before the owed-kit discharge, so a
        // kit the user owes still takes the screen last.
        App.ApplyPendingLaunchRoute();
        DischargeOwedSuccessionKit();
        FaunaApp.Core.Logs.E2eTrace.Write("[loaded] exit");
    }

    /// <summary>
    /// The successor's <b>post-auth hook</b> — the closing act of an identity
    /// succession (<c>identity-succession.md</c> § The RecoveryKey → <i>At
    /// succession</i>): land this session on Settings → Account, where the section
    /// mints and SHOWS the kit the successor is owed.
    ///
    /// <para><b>Navigation only; the mint belongs to the section.</b> This runs on
    /// the successor's first authenticated main-app render — the switch's own
    /// re-launch, or a later cold launch if that first one could not reach the nest —
    /// and the Account page's hydrate then calls
    /// <c>RecoveryKitViewModel.DischargeOwedSuccessionKitAsync</c>, which claims the
    /// obligation once and only for the actor that really is the successor. Keeping
    /// the mint there rather than here is what makes the kit <i>shown</i>: a mint
    /// nobody was shown leaves a kit nobody holds, which is strictly worse than
    /// never-created.</para>
    ///
    /// <para><b>Bound to the successor, not merely to "a kit is owed".</b> The
    /// ceremony runs from a live Account page that outlives the teardown by the width
    /// of the switch, so an unbound navigation would fire for the OUTGOING session
    /// too. Nothing is consumed here — a mismatch simply does not navigate, and the
    /// obligation waits for the session that can perform it.</para>
    /// </summary>
    private void DischargeOwedSuccessionKit()
    {
        // ⚠ ShellLog, not E2eTrace alone. This routing decision has three outcomes and
        // only one of them navigates, so the two that do NOT are exactly what a
        // "the successor was never shown a kit" post-mortem needs — and E2eTrace is a
        // no-op unless a human exported FAUNA_E2E_AGENT_LOG for the run
        // (`drivers/windows.py` only forwards an INHERITED value), so in an ordinary
        // e2e run this branch was invisible in all three directions. ShellLog reaches
        // the product ring and its on-disk file, which is the same log the aftermath's
        // own outcome line lands in — one place to read the closing act of a
        // succession, in the field as well as under test.
        if (!FaunaApp.Core.Services.SuccessionHandoff.KitOwed)
        {
            ShellLog.Debug("MainPage", "[succession] no kit owed — nothing to discharge");
            return;
        }
        var successor = FaunaApp.Core.Services.SuccessionHandoff.SuccessorActorIdHex;
        var signedIn = _clients?.Crypto is { HasKey: true } crypto ? crypto.ActorIdHex : null;
        if (successor is null || signedIn is null
            || !string.Equals(successor, signedIn, System.StringComparison.Ordinal))
        {
            // Not a fault and not a drop: the obligation is bound to the successor and
            // WAITS for the session that can perform it (the ceremony runs from a page
            // that outlives the switch, so this fires for the outgoing session too).
            // Logged because "waited forever" and "fired for the wrong actor" look
            // identical from outside, and only this line tells them apart.
            ShellLog.Info("MainPage",
                "[succession] owed kit not discharged here: "
                + $"successor={(successor is null ? "<none>" : "set")} "
                + $"signedIn={(signedIn is null ? "<none>" : "set")} match=False");
            return;
        }
        ShellLog.Info("MainPage", "[succession] owed kit — navigating to Settings/Account");
        NavigateToSettingsSubPage("account");
    }

    /// <summary>
    /// Seed <see cref="ContentPolicyCache"/>'s own-thresholds half from
    /// <c>fauna.spam.get_preferences</c> (family-safety.md § Content policy).
    /// Fire-and-forget; <see cref="ContentPolicyPreloader"/> owns the
    /// best-effort/fail-closed posture and swallows its own faults, so there is
    /// nothing to await and nothing that can throw into <c>Page_Loaded</c>.
    /// </summary>
    private void StartContentPolicyOwnThresholdHydration()
    {
        if (_clients?.Rpc is null)
        {
            // Never a silent drop (e2e rule 11) — an unhydrated cache renders as
            // "no collapse ever", which downstream looks exactly like a product bug.
            ShellLog.Warn("MainPage", "[content-policy] own-threshold seed skipped: no Rpc on _clients");
            return;
        }
        _ = new ContentPolicyPreloader(_clients.Rpc).RunAsync();
    }

    /// <summary>
    /// Ask this session's nest (the relay) for the device's region chain and arm the
    /// one-minute <c>refresh_if_due</c> tick (<see cref="RegionPlaneHost.RefreshAsync"/>).
    /// On <c>Page_Loaded</c> — the one site on EVERY login path, e2e's <c>set_state</c>
    /// login included, for the reason <see cref="ContentPolicyPreloader"/> records —
    /// and on every WS reconnect. A fold that changed the answer repaints the feed
    /// through <see cref="RegionPlaneHost.Changed"/>.
    /// </summary>
    private void StartRegionRefresh()
    {
        if (_clients?.Rpc is null)
        {
            ShellLog.Warn("MainPage", "[region] relay ask skipped: no Rpc on _clients");
            return;
        }
        _ = RegionPlaneHost.RefreshAsync(_clients.Rpc);
    }

    /// <summary>
    /// Fetch the sync-agent-status indicator immediately, then start the ~10s
    /// poll (sync-agent.md § Local agent health). Idempotent — `Page_Loaded` can
    /// re-run (e.g. re-entry), and a running timer is left alone.
    /// </summary>
    private void StartSyncAgentStatusPoll()
    {
        if (_viewModel is null) return;
        _ = _viewModel.PollSyncAgentStatusAsync();
        _syncAgentStatusPollTimer ??= BuildSyncAgentStatusPollTimer();
        if (!_syncAgentStatusPollTimer.IsEnabled)
        {
            _syncAgentStatusPollTimer.Start();
            System.Threading.Interlocked.Increment(ref LiveSyncAgentPollers);
        }
    }

    /// <summary>
    /// Stop the sync-agent poll when this page leaves the root frame (sign-out,
    /// account switch, e2e reset). Mirrors <c>EventsPage.OnNavigatedFrom</c>; see the
    /// <see cref="LiveSyncAgentPollers"/> field comment for why an un-stopped timer
    /// here starves the whole process rather than merely leaking memory.
    /// </summary>
    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        if (_syncAgentStatusPollTimer is { IsEnabled: true } timer)
        {
            timer.Stop();
            System.Threading.Interlocked.Decrement(ref LiveSyncAgentPollers);
        }
        _guardianNotifyFlushTimer?.Stop();
        _screenTimeTickTimer?.Stop();
        if (_clients?.Rpc is not null) _clients.Rpc.Reconnected -= OnRpcReconnected;
        base.OnNavigatedFrom(e);
    }

    private DispatcherTimer BuildSyncAgentStatusPollTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(SyncAgentStatusPollIntervalSecs) };
        timer.Tick += SyncAgentStatusPoll_Tick;
        return timer;
    }

    private async void SyncAgentStatusPoll_Tick(object? sender, object e)
    {
        if (_viewModel is null) return;
        await _viewModel.PollSyncAgentStatusAsync();
        await PollEngineHoldsAsync();
        // The agent's binding park (file-sync.md § Multi-writer shared sets → Revocation):
        // same tick, same reason as the engine holds — the agent derives it, so nothing
        // else re-drives the reconcile that would carry it to the row. Skips a failed read.
        if (App.CurrentLocationBindings is { } bindings) await bindings.PollParksAsync();
    }

    /// <summary>
    /// The mass-delete floor's confirm affordance (delete-propagation.md § A
    /// wholesale-vanished folder is infrastructure failure) — riding the SAME ~10s tick as the sync-agent status poll above, never
    /// folded into <see cref="Core.Services.LocationBindingsController.ReconcileAsync"/>
    /// (that runs on user mutations and reachability edges; a reconcile that reset the
    /// hold would blank the surface between polls). Lives here, not on
    /// <see cref="Core.ViewModels.MainViewModel"/>: the VM (in FaunaApp.Core) cannot
    /// reference the App-level statics <see cref="App.CurrentSyncAgent"/> /
    /// <see cref="App.CurrentLocationBindings"/> that carry the live provisioner and the
    /// session-scoped binding controller the Folders page renders from. Best-effort — no
    /// sync-agent session, or a transient list failure, just means the next tick tries
    /// again; this must never surface an error banner (the same "opt-in, not required"
    /// posture as the status poll beside it).
    /// </summary>
    private async Task PollEngineHoldsAsync()
    {
        if (App.CurrentSyncAgent is not { } agent || App.CurrentLocationBindings is not { } bindings) return;
        try
        {
            var holds = await agent.Channel.ListEngineHolds();
            bindings.FoldEngineHolds(holds);
        }
        catch (Exception ex)
        {
            E2eTrace.Write($"[folders] ListEngineHolds poll threw: {ex.Message}");
        }
    }

    /// <summary>
    /// Start the Guardian Notify flush tick (family-safety.md § Guardian
    /// Notify). Idempotent — <c>Page_Loaded</c> can re-run, and a running timer
    /// is left alone; mirrors <see cref="StartSyncAgentStatusPoll"/>.
    /// </summary>
    private void StartGuardianNotifyFlushPoll()
    {
        _guardianNotifyFlushTimer ??= BuildGuardianNotifyFlushTimer();
        if (!_guardianNotifyFlushTimer.IsEnabled) _guardianNotifyFlushTimer.Start();
    }

    private DispatcherTimer BuildGuardianNotifyFlushTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(GuardianNotifyFlushPollIntervalSecs) };
        timer.Tick += GuardianNotifyFlushPoll_Tick;
        return timer;
    }

    private async void GuardianNotifyFlushPoll_Tick(object? sender, object e)
    {
        if (_viewModel is null) return;
        await _viewModel.CheckGuardianNotifyAsync();
    }

    /// <summary>
    /// Start the screen-time production tick (family-safety.md § Screen time).
    /// Idempotent — <c>Page_Loaded</c> can re-run, and a running timer is left
    /// alone; mirrors <see cref="StartGuardianNotifyFlushPoll"/>.
    /// </summary>
    private void StartScreenTimeTick()
    {
        _screenTimeTickTimer ??= BuildScreenTimeTickTimer();
        if (!_screenTimeTickTimer.IsEnabled) _screenTimeTickTimer.Start();
    }

    private DispatcherTimer BuildScreenTimeTickTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(ScreenTimeCache.TickIntervalSecs) };
        timer.Tick += ScreenTimeTick_Tick;
        return timer;
    }

    private async void ScreenTimeTick_Tick(object? sender, object e)
    {
        if (_clients?.Rpc is null) return;
        await ScreenTimeCache.TickAsync(_clients.Rpc);
        UpdateScreenTimeLockOverlay();
    }

    private void NavView_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item
            && item.Tag is string tag
            && _clients is not null)
        {
            NavigateForSelection(tag);
#if DEBUG || FAUNA_E2E_AGENT
            NoteViewWhenLoaded(CurrentViewName);
#endif
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// Publish <paramref name="view"/> as <c>nav.stack[0].view</c> once the page
    /// the selection put in <c>ContentFrame</c> has loaded — "on Settings" must mean
    /// the Settings shell is in the tree, so a test that waits for the view and then
    /// reads the shell's own controls (<c>settings-nav-back</c>) cannot read the
    /// frame before it (convention 11: the state provider answers what is on
    /// screen). Before this the view was written only by <c>nav</c> patches and read
    /// "feed" through a whole walk the user took through the tabs.
    /// </summary>
    private void NoteViewWhenLoaded(string view)
    {
        if (ContentFrame.Content is not FrameworkElement page || page.IsLoaded)
        {
            App.NoteShellView(view);
            return;
        }
        void OnLoaded(object sender, RoutedEventArgs e)
        {
            page.Loaded -= OnLoaded;
            // A later selection may have replaced this page before it loaded.
            if (ReferenceEquals(ContentFrame.Content, page)) App.NoteShellView(view);
        }
        page.Loaded += OnLoaded;
    }
#endif

    private void NavigateForSelection(string tag)
    {
        if (tag == "Admin")
        {
            // Enter the distinct admin shell (Model B). Hide the main app pane
            // so AdminShellPage's vertical (Left) nav rail takes over the
            // sidebar slot in place — the uniform desktop "vertical sidebar-swap"
            // (admin.md § Navigation model; linux/web are the references), one
            // rail not two. Restored by the non-admin branch below on leave.
            NavView.IsPaneVisible = false;
            // Re-selecting the admin entry while already in the shell must NOT
            // reset the current sub-page, so only navigate when the content
            // isn't the shell.
            if (ContentFrame.Content is not AdminShellPage)
            {
                ContentFrame.Navigate(typeof(AdminShellPage), _clients);
            }
            _viewModel?.NavigateToCommand.Execute(tag);
            return;
        }
        if (tag == "Settings")
        {
            // Enter the distinct settings shell (sidebar-swap shape). Hide the
            // main app pane so SettingsShellPage's vertical (Left) nav rail
            // takes over the sidebar slot in place — the uniform desktop
            // "vertical sidebar-swap" (settings.md § Navigation model; linux/web
            // are the references), one rail not two. Restored by the non-shell
            // branch below on leave (LeaveSettings → Conversations).
            NavView.IsPaneVisible = false;
            // Re-selecting the settings entry while already in the shell must
            // NOT reset the current sub-page, so only navigate when the content
            // isn't the shell.
            if (ContentFrame.Content is not SettingsShellPage)
            {
                ContentFrame.Navigate(typeof(SettingsShellPage), _clients);
            }
            // The shell now definitely exists as ContentFrame.Content (just
            // constructed above, or already there) — apply any subId
            // NavigateToSettingsSubPage stashed, regardless of whether THIS
            // SelectionChanged firing was synchronous with that call.
            ApplyPendingSettingsSubId();
            _viewModel?.NavigateToCommand.Execute(tag);
            return;
        }
        // Any non-shell destination restores the main app sidebar (covers
        // leave-settings → Conversations and every other navigation that
        // leaves a shell).
        NavView.IsPaneVisible = true;
        var pageType = TagToPageType(tag);
        if (pageType != null)
        {
            // Every content-frame navigation, named. Cheap, and it is what makes
            // a page teardown/rebuild legible in a trace — including a
            // re-navigation to the page already on screen, which this branch
            // (unlike the Admin/Settings branches above) does not guard against.
            FaunaApp.Core.Logs.E2eTrace.Write(
                $"[nav] generic-branch navigate tag={tag} to={pageType.Name} "
                + $"from={ContentFrame.Content?.GetType().Name}");
            ContentFrame.Navigate(pageType, _clients);
            _viewModel?.NavigateToCommand.Execute(tag);
        }
    }

    private void ComposeAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        // Navigate to Feed page for compose
        if (_clients is null) return;
        foreach (var item in NavView.MenuItems.OfType<NavigationViewItem>())
        {
            if (item.Tag is string tag && tag == "Feed")
            {
                NavView.SelectedItem = item;
                break;
            }
        }
        args.Handled = true;
    }

    private async void QuickSwitchAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        var navItems = new List<string> { "Feed", "Conversations", "Contacts", "Bridges", "Sync", "Calendar", "Backups", "Media", "Search", "Moderation", "Admin", "Settings" };
        var suggestBox = new AutoSuggestBox
        {
            PlaceholderText = S.Get("search_page/placeholder"),
            ItemsSource = navItems,
        };
        suggestBox.TextChanged += (s, e) =>
        {
            if (e.Reason == AutoSuggestionBoxTextChangeReason.UserInput)
            {
                var query = s.Text.ToLowerInvariant();
                s.ItemsSource = navItems.Where(n => n.ToLowerInvariant().Contains(query)).ToList();
            }
        };
        var dialog = new ContentDialog
        {
            Title = S.Get("navigation/quick_switcher"),
            Content = suggestBox,
            CloseButtonText = S.Get("common/cancel"),
            XamlRoot = this.XamlRoot,
        };
        suggestBox.SuggestionChosen += (s, e) =>
        {
            dialog.Hide();
            NavigateToTag(e.SelectedItem?.ToString() ?? "");
        };
        await Controls.Dialogs.ShowAsync(dialog);
    }

    private void SettingsAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_clients is null) return;
        foreach (var item in NavView.FooterMenuItems.OfType<NavigationViewItem>())
        {
            if (item.Tag is string tag && tag == "Settings")
            {
                NavView.SelectedItem = item;
                break;
            }
        }
        args.Handled = true;
    }

    private async Task CheckAdminStatusAsync()
    {
        if (_clients?.Rpc is null) return;

        try
        {
            var isAdmin = await _clients.Rpc.AmIAdminAsync();
            ShellLog.Info("MainPage", $"[admin-gate] am_i_admin={isAdmin}");
            if (isAdmin)
            {
                ShowAdminNavItems();
                ShellLog.Info("MainPage", "[admin-gate] revealed admin-tab");

                // Stage-2 admin auto-default (long-term-store.md § Per-account re-auth):
                // this IS the client's "when does an account count as admin" observation,
                // so flip require-confirm ON for the active account here — iff the user
                // never touched its toggle. Idempotent + best-effort; re-fires on every
                // switch because MainPage is re-navigated and Page_Loaded re-runs.
                App.AutoEnableRequireConfirmForActiveAdmin();
            }
        }
        catch (Exception ex)
        {
            // Not admin or network error — admin nav stays hidden (fail-closed),
            // but logged: this gate is one-shot at Page_Loaded too, so a throw
            // hides admin-tab for the session with nothing on error-message.
            ShellLog.Warn("MainPage", $"[admin-gate] am_i_admin read failed: {ex.Message}");
        }
    }

    private void ShowAdminNavItems()
    {
        // Reveal the single gated admin entry (admin.md § Navigation model). The
        // per-admin-page navigation lives inside AdminShellPage, not the main nav.
        AdminSeparator.Visibility = Visibility.Visible;
        NavAdmin.Visibility = Visibility.Visible;
    }

    /// <summary>
    /// Restore the last-known supervision snapshot (family-safety.md §
    /// Content policy, clause 2) BEFORE the first <c>fauna.family.status</c>
    /// read lands — seeds the SAME stores <see cref="CheckFamilyStatusAsync"/>'s
    /// success path feeds (the guardian floor, <c>content_notify</c>,
    /// screen-time policy, the supervised indicator, the <c>family-tab</c>
    /// gate), so a cold launch enforces from disk rather than leaving a
    /// supervised ward unenforced for the read's round trip. Usage minutes
    /// are deliberately left <c>null</c> (unknown) here — the bedtime WINDOW
    /// half of screen-time is pure client-clock and needs no usage figure at
    /// all, and the budget half simply reads as "unknown" until the live
    /// read lands moments later. <c>null</c> (no snapshot, or one whose last
    /// read said unsupervised) leaves every store at its unsupervised
    /// default; the live read below is authoritative either way.
    /// </summary>
    private void RestoreSupervisionSnapshot()
    {
        if (_clients?.Rpc is null) return;
        if (_clients.Rpc.SupervisionSnapshot(_clients.Crypto.ActorIdHex) is not { } snapshot) return;

        ContentPolicyCache.SetGuardianPolicy(snapshot.contentPolicy);
        GuardianNotifyCache.SetEnabled(snapshot.contentNotify);
        ScreenTimeCache.SetWardScreenTime(snapshot.screenTime, snapshot.supervisedBy.handle, usageTodayMinutes: null);
        UpdateScreenTimeLockOverlay();
        NavFamily.Visibility = Visibility.Visible;
        SupervisedIndicatorButton.Content = S.Get("family/supervised_indicator")
            .Replace("{guardian}", snapshot.supervisedBy.handle);
        SupervisedIndicatorButton.Visibility = Visibility.Visible;
    }

    /// <summary>
    /// <c>fauna.family.status</c> — reveals the gated <c>family-tab</c> footer
    /// row + the global <c>supervised-indicator</c> chrome as appropriate
    /// (family-safety.md § App surface). Fail-closed: any error (incl. a
    /// transport fault) leaves both hidden, same shape
    /// as <see cref="CheckAdminStatusAsync"/>.
    /// </summary>
    private async Task CheckFamilyStatusAsync()
    {
        if (_clients?.Rpc is null)
        {
            // Never a silent drop (e2e rule 11 / the  lesson: a *guard* that
            // logs nothing is indistinguishable from a product bug downstream).
            ShellLog.Warn("MainPage", "[family-gate] skipped: no Rpc on _clients");
            return;
        }

        try
        {
            var status = await _clients.Rpc.FamilyStatusAsync();
            ShellLog.Info("MainPage",
                $"[family-gate] status: wards={status.@wards.Length} "
                + $"supervised={status.@supervisedBy is not null} "
                + $"incoming={status.@incomingTransfers.Length}");
            // Content policy / Guardian Notify / screen time, all off the SAME
            // read — no extra RPC, and on EVERY login path (the production-only
            // App.StartMainAppAsync is not one: the e2e set_state login never
            // enters it, which would leave the caches empty under test and make
            // every render assertion fail against correct product code).
            // FamilyStatusHandler binds all three from `status.supervision` —
            // the shared SupervisionSnapshot::from_status fold, gated on
            // supervised_by — never from the raw `status.policy` document
            // (family-client-enforcement.md § Implementation status today): a
            // reply that still carries a policy but names no guardian must bind
            // nothing enforceable. Set unconditionally: a null fold is the real
            // "unsupervised, nothing enforceable" value, not a skip, and
            // re-login must clear a previous account's floor/knob/lock rather
            // than inherit it.
            FamilyStatusHandler.ApplySupervision(status);
            UpdateScreenTimeLockOverlay();
            // Widens on an incoming transfer too — a proposed guardian with no other
            // family relationship still needs to reach the accept/decline prompt
            // (family-safety.md § Graduation & transfer; matches linux/web/apple).
            if (status.@wards.Length > 0 || status.@supervisedBy is not null
                || status.@incomingTransfers.Length > 0)
            {
                NavFamily.Visibility = Visibility.Visible;
                ShellLog.Info("MainPage", "[family-gate] revealed family-tab");
            }
            if (status.@supervisedBy is { } guardian)
            {
                SupervisedIndicatorButton.Content = S.Get("family/supervised_indicator")
                    .Replace("{guardian}", guardian.@handle);
                SupervisedIndicatorButton.Visibility = Visibility.Visible;
            }
        }
        catch (Exception ex)
        {
            // Fail-closed for the USER (a failed status read
            // must not surface an error banner), but never silent for a
            // DIAGNOSER: this gate runs exactly once, at Page_Loaded, so a throw
            // here hides family-tab for the whole session with nothing on
            // error-message to say why.
            //
            // The content-policy guardian half is deliberately left ABSENT here
            // rather than defaulted: absent fails closed to UNDER-enforcement (a
            // labeled item at most badges), which is the right side to err on
            // when we don't know the floor — inventing one could wrongly block a
            // legitimate post. The viewer's own-thresholds half is unaffected,
            // since ContentPolicyPreloader reads it independently.
            ShellLog.Warn("MainPage", $"[family-gate] status read failed: {ex.Message}");
        }
    }

    private void SupervisedIndicatorButton_Click(object sender, RoutedEventArgs e)
        => NavigateToTag("Family");

    /// <summary>
    /// Navigate to an admin sub-page by its shared-protocol id (the GTK Stack
    /// child name the shared e2e action layer sends as <c>nav.stack[last].id</c> —
    /// see tests/e2e-unified/actions/admin.py). Enters the distinct admin shell
    /// (Model B, admin.md § Navigation model) via the single admin entry, then
    /// routes to the requested sub-page within the shell (null/unknown → dashboard,
    /// via the unit-tested AdminNavigation map). Must be called on the UI thread.
    /// </summary>
    public void NavigateToAdminSubPage(string? subId)
    {
        ShowAdminNavItems();
        // Selecting the admin entry normally navigates the content frame to
        // AdminShellPage via NavView_SelectionChanged — but a tab this SAME
        // call just revealed (ShowAdminNavItems() above, same synchronous
        // tick) can still be treated as not-yet-selectable by
        // NavigationView's own selection machinery until its next layout
        // pass, so `SelectedItem = NavAdmin` alone is not reliable here: it
        // can silently never raise SelectionChanged, leaving ContentFrame on
        // whatever it showed before and admin-dashboard-heading never
        // rendering, with nothing on error-message to say why — the same "Collapsed item wedge" class NavigateToTag's own
        // check exists to avoid, for the ordinary non-shell tags). Set the
        // selection for chrome consistency, but drive the frame ourselves
        // rather than trusting the event to land — idempotent either way,
        // since NavView_SelectionChanged's own Admin branch re-checks
        // `ContentFrame.Content is not AdminShellPage` before it navigates.
        NavView.SelectedItem = NavAdmin;
        if (ContentFrame.Content is not AdminShellPage)
        {
            // row 197 diagnostic: only fires when SelectionChanged did NOT
            // already land the shell — tells apart "the event was just slow"
            // from "the event never fired at all" once FAUNA_E2E_AGENT_LOG is set.
            FaunaApp.Core.Logs.E2eTrace.Write(
                $"[nav] admin fallback: SelectionChanged had not landed AdminShellPage " +
                $"(from={ContentFrame.Content?.GetType().Name ?? "null"}); driving directly");
            NavView.IsPaneVisible = false;
            ContentFrame.Navigate(typeof(AdminShellPage), _clients);
            _viewModel?.NavigateToCommand.Execute("Admin");
        }
        if (ContentFrame.Content is AdminShellPage shell)
        {
            shell.NavigateToSubPage(subId);
        }
        else
        {
            FaunaApp.Core.Logs.E2eTrace.Write(
                $"[nav] admin: STILL not AdminShellPage after fallback " +
                $"(content={ContentFrame.Content?.GetType().Name ?? "null"}, " +
                $"clients-null={_clients is null})");
        }
    }

    /// <summary>
    /// Navigate to a settings sub-page by its shared-protocol id (the GTK Stack
    /// child name the shared e2e action layer sends as <c>nav.stack[last].id</c>).
    /// Enters the distinct settings shell (sidebar-swap, settings.md § Navigation
    /// model) via the single settings entry, then routes to the requested sub-page
    /// within the shell (null/unknown/"status" → the Status sub-page, via the
    /// unit-tested SettingsNavigation map). Must be called on the UI thread.
    ///
    /// Until 2026-08-23 a redirect hook sent the shared id "p2p" to a real
    /// top-level P2P page outside this shell; that page was WireGuard
    /// registration only and went with the stack, so "p2p" now falls through to
    /// Status like any other id windows has no sub-page for.
    /// </summary>
    public void NavigateToSettingsSubPage(string? subId)
    {
        // The subId travels via _pendingSettingsSubId, applied from inside
        // NavView_SelectionChanged's "Settings" branch — NOT by an immediate
        // check here. `NavView.SelectedItem = NavSettings` does not guarantee
        // SelectionChanged (and therefore ContentFrame.Navigate, which
        // constructs the shell) has already run by the time this method's
        // next line would otherwise check `ContentFrame.Content`; this
        // codebase has already measured that same assumption fail once for a
        // COLLAPSED item (NavigateToTag's own "wedge" comment); the
        // intermittent successor owed-kit render is
        // the same class for an ordinary visible one — a live trace showed
        // Page_Loaded correctly reach "owed kit — navigating to
        // Settings/Account" and then NOTHING further from this page for the
        // rest of the run, consistent with `shell.NavigateToSubPage` being
        // skipped because the immediate post-assignment check raced the
        // shell's own construction. Centralizing
        // the apply in the handler that actually constructs the shell is
        // correct regardless of whether that handler fires synchronously or
        // is deferred to a later dispatcher tick.
        _pendingSettingsSubId = subId;
        if (!ReferenceEquals(NavView.SelectedItem, NavSettings))
        {
            NavView.SelectedItem = NavSettings;
        }
        else
        {
            // Already selected: SelectionChanged will not re-fire, so apply
            // directly — same shape NavigateToTag uses for its own
            // already-selected case.
            if (ContentFrame.Content is not SettingsShellPage)
            {
                NavigateToTag("Settings");
            }
            ApplyPendingSettingsSubId();
        }
    }

    /// <summary>
    /// Apply a subId NavigateToSettingsSubPage stashed, once the Settings
    /// shell actually exists as ContentFrame.Content. Called from both
    /// NavigateToSettingsSubPage (the already-selected case, where no
    /// SelectionChanged event will fire) and NavView_SelectionChanged's
    /// Settings branch (the normal case, where the shell was just
    /// constructed or reused) — a no-op when nothing is pending.
    /// </summary>
    private void ApplyPendingSettingsSubId()
    {
        if (_pendingSettingsSubId is null) return;
        if (ContentFrame.Content is SettingsShellPage shell)
        {
            shell.NavigateToSubPage(_pendingSettingsSubId);
            _pendingSettingsSubId = null;
        }
    }

    /// <summary>
    /// Leave the admin shell, returning to the non-admin app's primary view
    /// (Conversations), mirroring <see cref="LeaveSettings"/> and the linux/web
    /// references — both "leave shell" affordances land on the primary view.
    /// admin.md § Navigation model (corrected 2026-06-07): admin is a top-level nav
    /// peer reached via the admin entry, <b>not</b> nested under Settings, so
    /// exiting it parallels <c>settings-nav-back</c> rather than chaining
    /// admin → settings → main. Called by AdminShellPage's <c>admin-nav-back</c>
    /// button; selecting the primary view navigates the content frame away from the
    /// admin shell, tearing it down.
    /// </summary>
    public void LeaveAdmin() => NavigateToPrimaryView();

    /// <summary>
    /// Leave the Settings shell, returning to the non-settings app's primary view
    /// (Conversations), mirroring linux (settings_nav_back → conversations).
    /// settings.md § Navigation model — the uniform "leave settings" affordance.
    /// </summary>
    public void LeaveSettings() => NavigateToPrimaryView();

    /// <summary>
    /// Select the primary view (Conversations) in the main sidebar — the single
    /// landing for leaving <b>either</b> the admin or the settings shell
    /// (admin.md / settings.md § Navigation model: both "leave shell" affordances
    /// return to the main view, so they share one exit path and cannot drift).
    /// Selecting the item drives <see cref="NavView_SelectionChanged"/>, which
    /// restores <c>IsPaneVisible</c> and navigates the content frame, tearing down
    /// whichever shell was shown.
    /// </summary>
    private void NavigateToPrimaryView()
    {
        foreach (var item in NavView.MenuItems.OfType<NavigationViewItem>())
        {
            if (item.Tag is string tag && tag == "Conversations")
            {
                NavView.SelectedItem = item;   // → NavView_SelectionChanged restores IsPaneVisible + navigates
                return;
            }
        }
    }

    private void NavigateToTag(string tag)
    {
        if (_clients is null || string.IsNullOrEmpty(tag))
        {
            ShellLog.Warn("MainPage", $"[nav-tag] '{tag}' DROPPED: _clients null? {_clients is null}");
            return;
        }

        var allItems = NavView.MenuItems.OfType<NavigationViewItem>()
            .Concat(NavView.FooterMenuItems.OfType<NavigationViewItem>());
        foreach (var item in allItems)
        {
            if (item.Tag is string t && t == tag)
            {
                if (ReferenceEquals(NavView.SelectedItem, item)
                    || item.Visibility == Visibility.Collapsed)
                {
                    // Two cases route through a DIRECT ContentFrame.Navigate rather than
                    // `NavView.SelectedItem = item`:
                    //   1. the item is already selected (force a reload of the same page);
                    //   2. the item is a gated tab (family-tab / admin-tab) the shell has
                    //      NOT revealed — an old guardian whose only ward just transferred
                    //      away, OR the async family/admin gate simply not landed yet when
                    //      this state-protocol nav arrives (a race with CheckFamilyStatus
                    //      Async). Assigning SelectedItem a COLLAPSED NavigationViewItem
                    //      does not raise SelectionChanged (the frame never navigates) and
                    //      can WEDGE the NavigationView outright — measured: the set_state
                    //      nav then never acks and the app is killed + relaunched, the whole
                    //      family-journey e2e flake. The state-protocol nav must reach
                    //      the view regardless of the tab chrome, so navigate the frame
                    //      directly (leaving the collapsed tab collapsed — the gate, not the
                    //      nav, owns its visibility).
                    var pageType = TagToPageType(tag);
                    if (pageType != null)
                    {
                        NavView.IsPaneVisible = true;
                        ContentFrame.Navigate(pageType, _clients);
                        _viewModel?.NavigateToCommand.Execute(tag);
                    }
                }
                else
                {
                    NavView.SelectedItem = item;
                }
                return;
            }
        }
        ShellLog.Warn("MainPage", $"[nav-tag] '{tag}' NO MATCHING ITEM in nav collections");
    }

    /// <summary>
    /// Update test message elements on the current page. Called on UI thread by TestAgent.
    /// Finds per-page InfoBars by x:Name and opens/closes them — InfoBars with IsOpen=True
    /// appear in UIA tree (FlaUI can find them); InfoBars with IsOpen=False are absent.
    /// Also updates the global TextBox elements as a fallback.
    /// </summary>
    public void UpdateTestMessages(string? error, string? warning, string? info)
    {
        // Update global fallback TextBoxes. Visibility tracks CONTENT — a shim with
        // nothing to say leaves the UIA tree entirely (MessageShim.ShouldShow), so it
        // stops answering for the `error-message` id it shares with every page's own
        // InfoBar. Without this the id is structurally always-visible and the
        // "no error at page start" assertion is unobservable (see MessageShim).
        ApplyGlobalError(error);
        GlobalWarningText.Text = warning ?? "";
        GlobalWarningText.Visibility = ShimVisibility(warning);
        GlobalInfoText.Text = info ?? "";
        GlobalInfoText.Visibility = ShimVisibility(info);

        var page = ActiveContentPage();
        // Through the shared locked writer — an unsynchronized third appender over
        // the same file drops lines from all three (see FaunaApp.Core.Logs.E2eTrace).
        void Log(string msg) => FaunaApp.Core.Logs.E2eTrace.Write($"[UTM] {msg}");
        Log($"ContentFrame.Content type: {page?.GetType().Name ?? "null"}, error={error}");
        if (page is not null)
        {
            ApplyPageError(page, error);
            var warningBar = page.FindName("WarningBar") as Microsoft.UI.Xaml.Controls.InfoBar;
            if (warningBar != null)
            {
                warningBar.Message = warning ?? "";
                warningBar.IsOpen = warning != null;
            }
            if (page.FindName("WarningTextMirror") is Microsoft.UI.Xaml.Controls.TextBlock warnMirror)
            {
                warnMirror.Text = warning ?? " ";
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(warnMirror, warning ?? " ");
                warnMirror.Visibility = ShimVisibility(warning);
            }
            page.UpdateLayout();
        }
    }

    /// <summary>
    /// State <paramref name="error"/> on the active page's error surface (its
    /// ErrorBar, its TextBlock mirror, the global shim) without touching the
    /// warning/info surfaces. Used by the shell for a refusal no page owns —
    /// <see cref="FaunaApp.Controls.Dialogs"/>' refused second dialog.
    /// </summary>
    internal void ShowError(string error)
    {
        ApplyGlobalError(error);
        if (ActiveContentPage() is Page page) ApplyPageError(page, error);
    }

    /// <summary>The page the user is looking at. While the admin or settings
    /// shell is the main content, the real page lives in the shell's inner frame —
    /// drill through so its per-page surfaces are the ones addressed (admin.md
    /// § Navigation model — the shell hosts the pages).</summary>
    private Page? ActiveContentPage() => ContentFrame.Content switch
    {
        AdminShellPage adminShell => adminShell.CurrentContent,
        SettingsShellPage settingsShell => settingsShell.CurrentContent,
        var content => content as Page,
    };

    private void ApplyGlobalError(string? error)
    {
        GlobalErrorText.Text = error ?? "";
        GlobalErrorText.Visibility = ShimVisibility(error);
    }

    /// <summary>Open/close the page's ErrorBar (FlaUI sees an InfoBar only while
    /// IsOpen) and its TextBlock mirror. The mirror needs BOTH Text and
    /// AutomationProperties.Name — FlaUI reads Name, not Text — and, like the
    /// global shim, is collapsed unless it carries a message: these TextBlocks
    /// also wear `error-message`, and their historical cleared value was a
    /// literal " " that kept them permanently on-screen (hence MessageShim's
    /// whitespace clause).</summary>
    private static void ApplyPageError(Page page, string? error)
    {
        if (page.FindName("ErrorBar") is Microsoft.UI.Xaml.Controls.InfoBar errorBar)
        {
            errorBar.Message = error ?? "";
            errorBar.IsOpen = error != null;
        }
        if (page.FindName("ErrorTextMirror") is Microsoft.UI.Xaml.Controls.TextBlock errMirror)
        {
            errMirror.Text = error ?? " ";
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(errMirror, error ?? " ");
            errMirror.Visibility = ShimVisibility(error);
        }
    }

    /// <summary>A test-message shim is in the UIA tree iff it carries a message.</summary>
    private static Visibility ShimVisibility(string? message) =>
        FaunaApp.Core.Helpers.MessageShim.ShouldShow(message)
            ? Visibility.Visible
            : Visibility.Collapsed;

    /// <summary>
    /// Convention 17 layer (c)'s `switch_pane` — windows leg
    /// (`docs/goal/architecture/e2e-systematic-ui-walks.md` § Implementation
    /// status today; contract owned by `fauna_e2e_agent::{SWITCH_PANE,
    /// switch_pane_target}`). Called by <see cref="FaunaApp.Testing.TestAgent"/>
    /// on the UI thread.
    ///
    /// <para><c>"page"</c> scopes to <see cref="ContentFrame"/> directly — a
    /// clean, content-only subtree. <c>"sidebar"</c> scopes to the whole
    /// <see cref="NavView"/>: WinUI's <c>NavigationView</c> exposes no narrower
    /// public search root for just its pane (the pane and the content share one
    /// template), so a candidate that resolves INSIDE <see cref="ContentFrame"/>
    /// is excluded rather than trusted — the same landmark-exclusion rule the
    /// web leg uses for its `&lt;nav&gt;`-not-inside-`&lt;main&gt;` check — so a
    /// future NavigationView template detail can never silently redirect
    /// "sidebar" into page content.</para>
    ///
    /// <para>Both scopes are resolved through
    /// <see cref="FocusManager.FindFirstFocusableElement(DependencyObject)"/> —
    /// the SAME focus-engine traversal WinUI's own Tab/Shift-Tab handling walks,
    /// scoped to a region, never a hand-picked control (the trap the linux leg's
    /// hand-written widget path fell into). Returns <c>null</c> when the region
    /// has no current tab stop, which callers must treat as a legitimate,
    /// silent no-op — not a refusal — matching apple's ruling for the same
    /// case (an empty pane's content has nothing to focus).</para>
    /// </summary>
    internal DependencyObject? FindFocusPaneCandidate(string pane)
    {
        if (pane == "page")
        {
            return FocusManager.FindFirstFocusableElement(ContentFrame);
        }
        var candidate = FocusManager.FindFirstFocusableElement(NavView);
        if (candidate is null) return null;
        return IsWithin(candidate, ContentFrame) ? null : candidate;
    }

    private static bool IsWithin(DependencyObject element, DependencyObject ancestor)
    {
        var current = element;
        while (current is not null)
        {
            if (ReferenceEquals(current, ancestor)) return true;
            current = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(current);
        }
        return false;
    }

    private static Type? TagToPageType(string tag) => tag switch
    {
        "Feed" => typeof(FeedPage),
        "Conversations" => typeof(ConversationsPage),
        "Contacts" => typeof(ContactsPage),
        "Profile" => typeof(ProfilePage),
        "Bridges" => typeof(BridgesPage),
        "Backups" => typeof(BackupsPage),
        "Media" => typeof(MediaPage),
        "Search" => typeof(SearchResultsPage),
        "Calendar" => typeof(EventsPage),
        "Moderation" => typeof(ModerationPage),
        "Notifications" => typeof(NotificationsPage),
        // The admin pages live inside AdminShellPage's inner frame (Model B);
        // the main nav routes the single Admin entry to the shell. Likewise the
        // settings sub-pages (incl. Status, folded in) live inside SettingsShellPage's
        // inner frame; the main nav routes the single Settings entry to that shell.
        "Admin" => typeof(AdminShellPage),
        "Settings" => typeof(SettingsShellPage),
        "Family" => typeof(FamilyPage),
        _ => null,
    };
}
